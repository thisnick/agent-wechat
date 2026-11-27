//! Group version notifications schedule read-only reconciliation. Unknown
//! controls stay in the sync journal; refresh completion never resubmits edits.
use super::*;
use anyhow::ensure;
use wechat_profile::{GroupProjection, RawFields, SqlValue};
use wechat_protocol::{protobuf as p, ContactRecord, MetadataReply};

/// Groups refreshed per pass. Each group makes up to three sequential metadata
/// requests (contact, member roster, detail), so 5 groups caps one pass at at
/// most 15 requests — comparable to `contact_remarks::LOOKUPS_PER_PASS` (10)
/// and `big_xml::EXPANSIONS_PER_PASS` (4) — while returning to the account loop
/// between passes so sync projection, sends and other queues are not starved.
const GROUPS_PER_PASS: usize = 5;
/// A failing refresh moves to `incoming-group-held:` after this many attempts
/// and emits `group.refresh_blocked`.
const MAX_ATTEMPTS: u8 = 3;
/// Initial retry delay in seconds after a failed refresh attempt; doubles on
/// the next failure (5 s, then 10 s before moving to `held:` on attempt 3) so
/// one failing group does not block healthy groups queued behind it.
const RETRY_BACKOFF_BASE_SECONDS: i64 = 5;

#[derive(Serialize, Deserialize)]
struct Refresh {
    group: String,
    source: String,
    origin: String,
    members: bool,
    detail: bool,
    member_version: u32,
    detail_version: u32,
    epoch: Option<Vec<u8>>,
    attempts: u8,
    contact_raw: Option<Vec<u8>>,
    roster_raw: Option<Vec<u8>>,
    detail_raw: Option<Vec<u8>>,
    #[serde(default)]
    contact_revision: Option<Vec<u8>>,
    #[serde(default)]
    not_before: i64,
}
impl Refresh {
    fn has_owned_responses(&self) -> bool {
        self.contact_raw.is_some()
            && (!self.members || self.roster_raw.is_some())
            && (!self.detail || self.detail_raw.is_some())
    }
}
struct GroupRefreshSession<'a> {
    permit: &'a mut crate::account_network::AccountNetworkPermit,
    fence: &'a SessionFence,
    desired: Option<&'a [u8]>,
}
fn retry_backoff_seconds(attempts: u8) -> i64 {
    let shift = u32::from(attempts.saturating_sub(1)).min(8);
    RETRY_BACKOFF_BASE_SECONDS << shift
}
fn is_session_ended(error: &anyhow::Error) -> bool {
    wechat_protocol::AccountInterrupted::find(error).is_some()
        || error.chain().any(|cause| {
            matches!(
                cause.to_string().as_str(),
                "session_superseded" | "push_session_superseded" | "account runtime stopped"
            )
        })
}
fn key(receipt: &str) -> String {
    format!(
        "incoming-group-pending:{:x}",
        Sha256::digest(receipt.as_bytes())
    )
}
/// Response bodies a refresh replaced after the group changed again, one copy
/// per replacement. Raw replies for investigation; nothing reads them back.
fn superseded(key: &str, attempts: u8) -> String {
    format!("{}:{attempts}", key.replace("pending:", "superseded:"))
}
fn done(receipt: &str) -> String {
    format!(
        "incoming-group-done:{:x}",
        Sha256::digest(receipt.as_bytes())
    )
}
fn scalar(fields: &[p::Field], field: u32) -> Result<Option<u32>> {
    match p::optional(fields, field)? {
        None => Ok(None),
        Some(p::Value::Varint(n)) => Ok(Some(u32::try_from(*n)?)),
        _ => bail!("invalid_group_metadata"),
    }
}
impl Core {
    /// Returns whether a known version signal is still awaiting reconciliation.
    pub(super) fn queue_incoming_group(
        &self,
        account: &str,
        contact: &ContactRecord,
        receipt: &str,
        origin: &str,
    ) -> Result<bool> {
        let f = p::decode(&contact.raw)?;
        let member_version = scalar(&f, 53)?.unwrap_or_default();
        let detail_version = scalar(&f, 64)?.unwrap_or_default();
        let members = member_version > 0 || p::optional_data(&f, 57)?.is_some();
        let detail = detail_version > 0;
        if !members && !detail {
            return Ok(false);
        }
        if self.store.runtime(account, &done(receipt))?.is_some() {
            return Ok(false);
        }
        let key = key(receipt);
        if self
            .store
            .runtime(account, &key.replace("pending:", "held:"))?
            .is_some()
        {
            return Ok(true);
        }
        if self.store.runtime(account, &key)?.is_none() {
            let job = Refresh {
                group: contact.username.clone(),
                source: receipt.into(),
                origin: origin.into(),
                members,
                detail,
                member_version,
                detail_version,
                epoch: self.store.runtime(account, "logout-epoch")?,
                attempts: 0,
                contact_raw: None,
                roster_raw: None,
                detail_raw: None,
                contact_revision: None,
                not_before: 0,
            };
            self.save_incoming_group(account, &key, &job)?;
            self.wake_account(account);
        }
        Ok(true)
    }
    fn save_incoming_group(&self, account: &str, key: &str, job: &Refresh) -> Result<()> {
        self.store.put_runtime(
            account,
            key,
            &self.vault.seal(account, key, &serde_json::to_vec(job)?)?,
        )
    }
    pub(super) fn group_read<F>(
        &self,
        account: &str,
        epoch: Option<&[u8]>,
        read: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce(&mut AccountState) -> Result<Vec<u8>>,
    {
        let mut state = self.account_state(account)?;
        let expected = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        ensure!(
            self.store.runtime(account, "logout-epoch")?.as_deref() == epoch
                && self
                    .store
                    .saved_login_authorization_current(account, &expected, epoch)?,
            "authorization_generation_changed"
        );
        let raw = read(&mut state)?;
        self.check_state_profile(account, &state)?;
        let sealed = self
            .vault
            .seal(account, "account-state", &serde_json::to_vec(&state)?)?;
        self.store
            .save_push_authorization(account, &sealed, &expected, epoch)?;
        Ok(raw)
    }
    fn group_read_with_permit<F>(
        &self,
        account: &str,
        epoch: Option<&[u8]>,
        session: Option<&mut GroupRefreshSession<'_>>,
        read: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce(&Arc<dyn crate::upstream::Upstream>, &mut AccountState) -> Result<Vec<u8>>,
    {
        let (active_fence, active_desired, permit) = match session {
            Some(session) => (
                session.fence.clone(),
                session.desired.map(ToOwned::to_owned),
                Some(&mut *session.permit),
            ),
            None => (
                self.session_fence(account)?,
                self.store.runtime(account, "desired")?,
                None,
            ),
        };
        let mut state = self.session_state(account, &active_fence)?;
        let expected = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        ensure!(
            self.store.runtime(account, "logout-epoch")?.as_deref() == epoch
                && self
                    .store
                    .saved_login_authorization_current(account, &expected, epoch)?,
            "authorization_generation_changed"
        );
        let before_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&state)?);
        let upstream = self.account_upstream(account)?;
        let raw = match permit {
            Some(permit) => permit.transfer(self, account, || read(&upstream, &mut state))?,
            None => read(&upstream, &mut state)?,
        };
        ensure!(
            self.store.runtime(account, "logout-epoch")?.as_deref() == epoch,
            "authorization_generation_changed"
        );
        ensure!(!self.stopping.load(Ordering::SeqCst), "session_superseded");
        let returned_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&state)?);
        if returned_bytes.as_slice() == before_bytes.as_slice() {
            self.session_state(account, &active_fence)?;
            ensure!(
                self.store.runtime(account, "desired")?.as_deref() == active_desired.as_deref(),
                "session_superseded"
            );
            self.check_state_profile(account, &state)?;
            self.persisted_native_state(account, &state)?;
        } else {
            self.save_network_state(
                account,
                &state,
                &expected,
                &active_fence,
                active_desired.as_deref(),
            )?;
        }
        Ok(raw)
    }
    fn finish_incoming_group(&self, account: &str, key: &str, job: &Refresh) -> Result<()> {
        self.apply_incoming_group(account, key, job)?;
        // Retain all verified response bodies for offline local repair.
        let retained_key = key.replace("pending:", "receipt:");
        self.store.put_runtime(
            account,
            &retained_key,
            &sealed_receipt(self, account, &retained_key, job)?,
        )?;
        self.store.append_event_once(
            account,
            &format!("group-refreshed:{}", job.source),
            "group.updated",
            &json!({
                "groupId": job.group,
                "origin": job.origin,
                "source": "remote_sync_refresh",
                "membersRefreshed": job.members,
                "announcementRefreshed": job.detail,
                "profileProjection": "partial"
            }),
        )?;
        // Store the source receipt so retention can match this marker
        // against unfinished sync batches.
        self.store
            .put_runtime(account, &done(&job.source), job.source.as_bytes())?;
        // Keep the completed job until its original command has been
        // revisited, so a restart cannot lose this local recovery work.
        self.resume_incoming_group_source(account, job)?;
        self.store.clear_runtime(account, key)?;
        Ok(())
    }
    fn has_due_incoming_group_refreshes(&self, account: &str, now: i64) -> Result<bool> {
        let current_epoch = self.store.runtime(account, "logout-epoch")?;
        for (key, sealed) in self.store.incoming_group_refreshes(account)? {
            let job: Refresh =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            if job.not_before <= now && job.epoch == current_epoch {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Seconds until the earliest backed-off current-epoch group refresh is
    /// due, capped to `max_seconds`. Ignores jobs already due so a network
    /// outage keeps its normal tick interval instead of retrying every second.
    pub(super) fn next_incoming_group_retry_delay(
        &self,
        account: &str,
        max_seconds: u64,
    ) -> Result<Option<u64>> {
        let max_seconds = max_seconds.max(1);
        let now = chrono::Utc::now().timestamp();
        let current_epoch = self.store.runtime(account, "logout-epoch")?;
        let mut earliest: Option<u64> = None;
        for (key, sealed) in self.store.incoming_group_refreshes(account)? {
            let job: Refresh =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            if job.epoch != current_epoch || job.not_before <= now {
                continue;
            }
            let wait = u64::try_from(job.not_before - now)
                .unwrap_or(max_seconds)
                .clamp(1, max_seconds);
            earliest = Some(earliest.map_or(wait, |prev| prev.min(wait)));
        }
        Ok(earliest)
    }
    /// A normal worker may fetch missing metadata. Local repair passes false
    /// and uses only responses already owned by the journal.
    pub(super) fn refresh_incoming_groups(
        &self,
        account: &str,
        allow_fetch: bool,
    ) -> Result<usize> {
        self.refresh_incoming_groups_with_permit(account, allow_fetch, None, None, None)
    }
    pub(super) fn refresh_incoming_groups_with_permit(
        &self,
        account: &str,
        allow_fetch: bool,
        permit: Option<&mut crate::account_network::AccountNetworkPermit>,
        fence: Option<&SessionFence>,
        desired: Option<&[u8]>,
    ) -> Result<usize> {
        let mut session = match (permit, fence) {
            (Some(permit), Some(fence)) => Some(GroupRefreshSession {
                permit,
                fence,
                desired,
            }),
            _ => None,
        };
        let rows = self.store.incoming_group_refreshes(account)?;
        if rows.is_empty() {
            return Ok(0);
        }
        if !allow_fetch {
            // Local repair never fetches, increments attempts or holds jobs; it
            // only finishes refreshes whose responses are already owned.
            let mut applied = 0usize;
            for (key, sealed) in rows {
                let job: Refresh =
                    serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
                if !job.has_owned_responses() {
                    continue;
                }
                let outcome = (|| -> Result<()> {
                    self.account_state(account)?;
                    ensure!(
                        job.epoch == self.store.runtime(account, "logout-epoch")?,
                        "authorization_generation_changed"
                    );
                    self.finish_incoming_group(account, &key, &job)
                })();
                if outcome.is_ok() {
                    applied += 1;
                }
            }
            return Ok(applied);
        }

        let now = chrono::Utc::now().timestamp();
        let current_epoch = self.store.runtime(account, "logout-epoch")?;
        let mut ready = Vec::new();
        for (key, sealed) in rows {
            let job: Refresh =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            if job.not_before <= now {
                ready.push((key, job));
            }
        }
        if ready.is_empty() {
            return Ok(0);
        }
        // Prefer current-epoch fresh jobs over retries while keeping FIFO order
        // within each tier so one failing group does not block healthy ones.
        ready.sort_by_key(|(_, job)| (job.epoch != current_epoch, job.attempts, job.not_before));

        let mut applied = 0usize;
        let mut hit_error = false;
        for (processed, (key, mut job)) in ready.into_iter().take(GROUPS_PER_PASS).enumerate() {
            if processed > 0 {
                if let Some(active) = session.as_mut() {
                    self.yield_sync_writer(account, active.permit, active.fence, active.desired)?;
                }
            } else if let Some(active) = session.as_ref() {
                self.check_sync_replay_session(account, active.fence, active.desired)?;
            }

            job.attempts = job.attempts.saturating_add(1);
            self.save_incoming_group(account, &key, &job)?;

            let outcome = (|| -> Result<()> {
                self.account_state(account)?;
                ensure!(
                    job.epoch == self.store.runtime(account, "logout-epoch")?,
                    "authorization_generation_changed"
                );
                let current = self.group_revision(account, &job.group)?;
                if job.contact_raw.is_some()
                    && current != job.contact_revision
                    && current.as_deref() != Some(key.as_bytes())
                {
                    // Keep the replaced bodies for investigation, outside
                    // the refresh queue: a key under `incoming-group-pending:`
                    // would be picked up as a new job that is always
                    // superseded again, refetching on every tick (#722).
                    let prior = superseded(&key, job.attempts);
                    self.store.put_runtime(
                        account,
                        &prior,
                        &sealed_receipt(self, account, &prior, &job)?,
                    )?;
                    job.contact_raw = None;
                    job.roster_raw = None;
                    job.detail_raw = None;
                    // A failed refetch must not save the same bodies again.
                    self.save_incoming_group(account, &key, &job)?;
                }
                if job.contact_raw.is_none() {
                    job.contact_revision = self.group_revision(account, &job.group)?;
                    let raw = self.group_read_with_permit(
                        account,
                        job.epoch.as_deref(),
                        session.as_mut(),
                        |upstream, state| {
                            upstream.lookup_contacts_raw(state, std::slice::from_ref(&job.group))
                        },
                    )?;
                    job.contact_raw = Some(raw);
                    self.save_incoming_group(account, &key, &job)?;
                }
                if job.members && job.roster_raw.is_none() {
                    let raw = self.group_read_with_permit(
                        account,
                        job.epoch.as_deref(),
                        session.as_mut(),
                        |upstream, state| upstream.lookup_room_raw(state, &job.group, 0),
                    )?;
                    job.roster_raw = Some(raw);
                    self.save_incoming_group(account, &key, &job)?;
                }
                if job.detail && job.detail_raw.is_none() {
                    let raw = self.group_read_with_permit(
                        account,
                        job.epoch.as_deref(),
                        session.as_mut(),
                        |upstream, state| upstream.group_detail_raw(state, &job.group),
                    )?;
                    job.detail_raw = Some(raw);
                    self.save_incoming_group(account, &key, &job)?;
                }
                self.finish_incoming_group(account, &key, &job)
            })();

            match outcome {
                Ok(()) => applied += 1,
                Err(error) => {
                    let auth_rejected = wechat_protocol::is_session_rejected(&error)
                        || super::session_auth::authentication_required(&error);
                    let session_ended = is_session_ended(&error)
                        || session.as_ref().is_some_and(|active| {
                            self.check_sync_replay_session(account, active.fence, active.desired)
                                .is_err()
                        });
                    if auth_rejected || session_ended {
                        job.attempts = job.attempts.saturating_sub(1);
                        let _ = self.save_incoming_group(account, &key, &job);
                        return Err(error);
                    }
                    hit_error = true;
                    if job.attempts >= MAX_ATTEMPTS {
                        // Keep any fetched bodies under `receipt:` for 7-day
                        // investigation, and store only the source receipt under
                        // `held:` so retention can match unfinished sync batches.
                        let retained_key = key.replace("pending:", "receipt:");
                        self.store.put_runtime(
                            account,
                            &retained_key,
                            &sealed_receipt(self, account, &retained_key, &job)?,
                        )?;
                        let held_key = key.replace("pending:", "held:");
                        self.store
                            .put_runtime(account, &held_key, job.source.as_bytes())?;
                        self.store.clear_runtime(account, &key)?;
                        self.store.append_event_once(
                            account,
                            &format!("group-refresh-held:{}", job.source),
                            "group.refresh_blocked",
                            &json!({
                                "groupId": job.group,
                                "source": "remote_sync",
                                "reason": "metadata_refresh_unavailable",
                                "retryable": false
                            }),
                        )?;
                    } else if error.to_string() != "group_refresh_superseded" {
                        let fail_now = chrono::Utc::now().timestamp();
                        job.not_before = fail_now + retry_backoff_seconds(job.attempts);
                        self.save_incoming_group(account, &key, &job)?;
                    }
                    if error.to_string() != "group_refresh_superseded" {
                        break;
                    }
                }
            }
        }

        if applied > 0
            && !hit_error
            && self.has_due_incoming_group_refreshes(account, chrono::Utc::now().timestamp())?
        {
            self.wake_account(account);
        }
        Ok(applied)
    }
    fn resume_incoming_group_source(&self, account: &str, job: &Refresh) -> Result<()> {
        let Some(source) = job.source.strip_prefix("projected:") else {
            // A direct metadata application has no enclosing sync batch.
            return Ok(());
        };
        let (batch_id, index) = source
            .rsplit_once(':')
            .context("invalid_group_source_receipt")?;
        let index: usize = index.parse().context("invalid_group_source_receipt")?;
        let Some((stream, sealed)) = self.store.retained_sync_batch(account, batch_id)? else {
            return Ok(());
        };
        let raw = self
            .vault
            .open_record(account, &format!("apply:{batch_id}"), &sealed)?;
        let commands = match stream.as_str() {
            "message" => serde_json::from_slice::<wechat_protocol::wire::SyncBatch>(&raw)?.commands,
            "contact" => {
                serde_json::from_slice::<wechat_protocol::contact_sync::ContactSyncBatch>(&raw)?
                    .commands
            }
            _ => bail!("invalid_group_source_stream"),
        };
        let command = commands
            .get(index)
            .context("invalid_group_source_receipt")?;
        ensure!(command.id == 2, "group_source_command_mismatch");
        ensure!(
            wechat_protocol::wire::decode_contact_record(&command.payload)?.username == job.group,
            "group_source_command_mismatch"
        );
        if self.store.runtime(account, &job.source)?.is_none() {
            if let Err(error) = self.apply_sync_command(account, command, &job.source, &job.origin)
            {
                if sync_projection::projection_blocker(&error).is_some() {
                    // The refreshed fields may coexist with unsupported controls.
                    // Keep their batch blocked until a separately requested repair.
                    return Ok(());
                }
                return Err(error);
            }
        }
        // Completion checks read receipts only. They never dispatch unrelated
        // commands in this batch or reopen the account's other blocked batches.
        for start in (0..commands.len()).step_by(500) {
            let receipts: Vec<_> = (start..commands.len().min(start + 500))
                .map(|index| format!("projected:{batch_id}:{index}"))
                .collect();
            if self.store.runtime_keys(account, &receipts)?.len() != receipts.len() {
                return Ok(());
            }
        }
        self.store.finish_sync(account, batch_id)
    }
    fn apply_incoming_group(&self, account: &str, intent: &str, job: &Refresh) -> Result<()> {
        let (location, keys) = self.profile_location_keys(account)?;
        let options = self.write_options(account, intent)?;
        let current = self.group_revision(account, &job.group)?;
        ensure!(
            current == job.contact_revision || current.as_deref() == Some(intent.as_bytes()),
            "group_refresh_superseded"
        );
        let reply = wechat_protocol::wire::decode_contacts(
            job.contact_raw
                .as_deref()
                .context("group_refresh_pending")?,
            std::slice::from_ref(&job.group),
        )?;
        let contact = reply
            .contacts
            .iter()
            .find(|c| c.username == job.group)
            .context("group_refresh_unavailable")?;
        wechat_profile::apply_contact_patch(
            &location,
            &keys,
            &super::metadata::contact_patch(contact)?,
            &options,
        )?;
        self.mark_group_revision(account, &job.group, intent)?;
        wechat_profile::apply_group_patch(
            &location,
            &keys,
            &GroupProjection {
                username: job.group.clone(),
                fields: RawFields::new(),
                detail_fields: None,
                members: vec![],
                complete_roster: false,
            },
            &options,
        )?;
        if let Some(raw) = &job.roster_raw {
            let roster = wechat_protocol::wire::decode_room(raw, &job.group)?;
            ensure!(
                roster.version >= job.member_version && roster.full_member_ids.is_some(),
                "group_roster_refresh_incomplete"
            );
            let version_key = format!(
                "group-roster-version:{:x}",
                Sha256::digest(job.group.as_bytes())
            );
            let old = self
                .store
                .runtime(account, &version_key)?
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or_default();
            if roster.version >= old {
                self.project_metadata_roster(
                    &location,
                    &keys,
                    &MetadataReply {
                        value: roster.clone(),
                        raw: raw.clone(),
                    },
                    &options,
                    true,
                )?;
                self.store.put_runtime(
                    account,
                    &version_key,
                    roster.version.to_string().as_bytes(),
                )?;
            }
        }
        if let Some(raw) = &job.detail_raw {
            self.project_group_detail(account, &job.group, raw, job.detail_version, intent)?;
        }
        Ok(())
    }
    pub(super) fn project_group_detail(
        &self,
        account: &str,
        group: &str,
        raw: &[u8],
        required_version: u32,
        intent: &str,
    ) -> Result<()> {
        let (location, keys) = self.profile_location_keys(account)?;
        let options = self.write_options(account, intent)?;

        let detail = wechat_protocol::decode_group_detail(raw, group)?;
        ensure!(
            detail.version >= required_version,
            "group_detail_refresh_incomplete"
        );
        let version_key = format!(
            "group-detail-version:{:x}",
            Sha256::digest(group.as_bytes())
        );
        let old = self
            .store
            .runtime(account, &version_key)?
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_default();
        if detail.version >= old {
            let fields = [
                ("username_", SqlValue::Text(group.to_owned())),
                ("announcement_", SqlValue::Text(detail.announcement)),
                ("announcement_editor_", SqlValue::Text(detail.editor)),
                (
                    "announcement_publish_time_",
                    SqlValue::Integer(detail.published_at.to_string()),
                ),
                (
                    "chat_room_status_",
                    SqlValue::Integer(detail.status.to_string()),
                ),
                ("xml_announcement_", SqlValue::Text(detail.xml)),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
            wechat_profile::apply_group_patch(
                &location,
                &keys,
                &GroupProjection {
                    username: group.to_owned(),
                    fields: RawFields::new(),
                    detail_fields: Some(fields),
                    members: vec![],
                    complete_roster: false,
                },
                &options,
            )?;
            self.store
                .put_runtime(account, &version_key, detail.version.to_string().as_bytes())?;
        }

        Ok(())
    }
}
fn sealed_receipt(core: &Core, account: &str, key: &str, job: &Refresh) -> Result<Vec<u8>> {
    core.vault.seal(account, key, &serde_json::to_vec(job)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::wire::SyncCommand;
    pub(super) const ROOM: &str = "synthetic@chatroom";
    pub(super) fn contact(name: &str, signals: bool) -> Vec<u8> {
        let mut f = vec![
            p::bytes(1, &p::bytes(1, ROOM.as_bytes())),
            p::bytes(2, &p::bytes(1, name.as_bytes())),
            p::bytes(3, b""),
            p::bytes(4, b""),
            p::uint(5, 0),
            p::bytes(6, &p::uint(1, 0)),
            p::uint(7, u64::from(u32::MAX)),
            p::uint(8, 2),
            p::uint(9, 3),
            p::uint(17, 0),
            p::uint(85, 1),
        ];
        if signals {
            f.extend([p::uint(53, 7), p::bytes(57, &p::uint(1, 2)), p::uint(64, 3)]);
            // A non-remark control: remark-only controls queue a contact lookup
            // instead of holding the update (see `contact_remarks`).
            let control = p::message(&[
                p::uint(1, 1),
                p::uint(2, 14),
                p::uint(3, 1),
                p::bytes(4, &p::uint(1, 27)),
            ])
            .unwrap();
            f.push(p::bytes(
                89,
                &p::message(&[p::uint(1, 1), p::bytes(2, &control)]).unwrap(),
            ));
        }
        p::message(&f).unwrap()
    }
    fn staged(core: &Core) -> (String, Refresh) {
        let command = SyncCommand {
            id: 2,
            payload: contact("Phone title", true),
        };
        // Acknowledged at once; the group refresh runs in the background.
        core.apply_sync_command("one", &command, "phone-change", "live")
            .unwrap();
        assert!(core.store.runtime("one", "phone-change").unwrap().is_some());
        ready_refresh(core, "phone-change")
    }
    pub(super) fn ready_refresh(core: &Core, source: &str) -> (String, Refresh) {
        let (key, mut job) = core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .into_iter()
            .map(|(key, sealed)| {
                let job: Refresh =
                    serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap())
                        .unwrap();
                (key, job)
            })
            .find(|(_, job)| job.source == source)
            .unwrap();
        job.contact_revision = core.group_revision("one", ROOM).unwrap();
        job.contact_raw = Some(
            p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(2, 1),
                p::bytes(3, &contact("Fresh phone title", false)),
                p::bytes(4, &[0]),
            ])
            .unwrap(),
        );
        let changed = p::message(&[
            p::bytes(1, b"synthetic_self"),
            p::bytes(3, b"Phone display"),
            p::uint(6, 1),
            p::bytes(7, b"opaque"),
        ])
        .unwrap();
        job.roster_raw = Some(
            p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(3, 7),
                p::bytes(
                    4,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &changed)]).unwrap(),
                ),
                p::bytes(5, b"peer"),
                p::uint(6, 2),
                p::bytes(7, &p::bytes(1, b"synthetic_self")),
                p::bytes(7, &p::bytes(1, b"peer")),
                p::uint(8, 1),
                p::bytes(9, &p::bytes(1, b"synthetic_self")),
            ])
            .unwrap(),
        );
        job.detail_raw = Some(
            p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::bytes(2, b"Phone announcement"),
                p::uint(3, 3),
                p::bytes(4, b"synthetic_self"),
                p::uint(5, 100),
                p::bytes(12, b"<announcement>Server XML</announcement>"),
            ])
            .unwrap(),
        );
        (key, job)
    }
    #[test]
    fn incoming_changes_commit_before_events_and_owned_delta_refresh_keeps_unchanged_members() {
        let (_temp, core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let (key, job) = staged(&core);
        let before = core.group_info("one", ROOM).unwrap();
        assert_eq!(before["name"], "Phone title");
        assert_eq!(before["muted"], true);
        assert_eq!(
            core.store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "group.updated" && e.data["source"] == "remote_sync")
                .count(),
            1
        );
        core.save_incoming_group("one", &key, &job).unwrap();
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 1);
        let updated = core.group_info("one", ROOM).unwrap();
        assert_eq!(updated["name"], "Fresh phone title");
        assert_eq!(updated["announcement"], "Phone announcement");
        assert_eq!(updated["myDisplayName"], "Phone display");
        assert_eq!(updated["ownerId"], "peer");
        assert_eq!(updated["administratorIds"], json!(["synthetic_self"]));
        assert_eq!(updated["canMentionAll"], true);
        assert_eq!(
            core.group_member("one", ROOM, "peer").unwrap()["state"],
            "present"
        );
        let newer = SyncCommand {
            id: 2,
            payload: contact("Later phone edit", false),
        };
        core.apply_sync_command("one", &newer, "newer-change", "live")
            .unwrap();
        let old = SyncCommand {
            id: 2,
            payload: contact("Phone title", true),
        };
        // Replaying the older update never overwrites the newer one.
        core.apply_sync_command("one", &old, "phone-change", "live")
            .unwrap();
        assert_eq!(
            core.group_info("one", ROOM).unwrap()["name"],
            "Later phone edit"
        );
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
    }
    #[test]
    fn a_refresh_superseded_once_refetches_once_and_leaves_the_queue() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Server {
            contact: Vec<u8>,
            roster: Vec<u8>,
            detail: Vec<u8>,
            calls: Arc<AtomicUsize>,
        }
        impl crate::upstream::Upstream for Server {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, _: &[String]) -> Result<Vec<u8>> {
                // The first refetch fails.
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    bail!("synthetic_network_error");
                }
                Ok(self.contact.clone())
            }
            fn lookup_room_raw(&self, _: &mut AccountState, _: &str, _: u32) -> Result<Vec<u8>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.roster.clone())
            }
            fn group_detail_raw(&self, _: &mut AccountState, _: &str) -> Result<Vec<u8>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.detail.clone())
            }
        }
        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let (key, job) = staged(&core);
        let calls = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(Server {
            contact: job.contact_raw.clone().unwrap(),
            roster: job.roster_raw.clone().unwrap(),
            detail: job.detail_raw.clone().unwrap(),
            calls: calls.clone(),
        });
        // The job already holds fetched bodies when another update to the same
        // group lands once, before the job applies them.
        core.save_incoming_group("one", &key, &job).unwrap();
        core.mark_group_revision("one", ROOM, "concurrent-update")
            .unwrap();
        // A copy an earlier release saved under the queue's names is not a job.
        let legacy = format!("{key}:superseded:1");
        core.store.put_runtime("one", &legacy, b"legacy").unwrap();

        // The superseded bodies are saved once; the refetch fails and backs off.
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let sealed = core.store.runtime("one", &key).unwrap().unwrap();
        let mut backed_off: Refresh =
            serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap()).unwrap();
        assert!(backed_off.not_before > 0);
        // While backed off, another immediate pass makes no upstream call.
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        backed_off.not_before = 0;
        core.save_incoming_group("one", &key, &backed_off).unwrap();
        // The retry refetches without saving the same bodies again, then applies.
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            core.group_info("one", ROOM).unwrap()["name"],
            "Fresh phone title"
        );
        // Later ticks find nothing to do and never ask the server again.
        for _ in 0..3 {
            assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        let saved = core
            .store
            .runtime_prefix("one", "incoming-group-superseded:", 10)
            .unwrap();
        let keys: Vec<_> = saved.into_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, [superseded(&key, 1)]);
        assert_eq!(
            core.store.runtime("one", &done("phone-change")).unwrap(),
            Some(b"phone-change".to_vec())
        );
    }
    #[test]
    fn held_group_refresh_stores_source_receipt_and_keeps_bodies_under_receipt() {
        struct FailingServer;
        impl crate::upstream::Upstream for FailingServer {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, _: &[String]) -> Result<Vec<u8>> {
                bail!("synthetic_network_error")
            }
        }
        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let (key, _job) = staged(&core);
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(FailingServer);
        for attempt in 0..3 {
            if attempt > 0 {
                let mut retry = load_pending_job(&core, &key);
                retry.not_before = 0;
                core.save_incoming_group("one", &key, &retry).unwrap();
            }
            assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        }
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        let held_key = key.replace("pending:", "held:");
        let receipt_key = key.replace("pending:", "receipt:");
        assert_eq!(
            core.store.runtime("one", &held_key).unwrap(),
            Some(b"phone-change".to_vec())
        );
        assert!(core.store.runtime("one", &receipt_key).unwrap().is_some());
    }
    #[test]
    fn local_repair_never_fetches_missing_data_or_overwrites_a_newer_change() {
        let (_temp, core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let (key, mut job) = staged(&core);
        job.contact_raw = None;
        core.save_incoming_group("one", &key, &job).unwrap();
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
        let (_, job) = staged(&core);
        core.save_incoming_group("one", &key, &job).unwrap();
        core.mark_group_revision("one", ROOM, "newer").unwrap();
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
        assert_eq!(core.group_info("one", ROOM).unwrap()["name"], "Phone title");
        core.store
            .put_runtime("one", "logout-epoch", b"changed")
            .unwrap();
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
    }

    #[test]
    fn incoming_group_refresh_clears_display_name_for_changed_members_and_keeps_unchanged_delta_members(
    ) {
        let (_temp, core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "roster-clear-test").unwrap();

        let initial_raw = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(3, 7),
            p::bytes(
                4,
                &p::message(&[
                    p::uint(1, 2),
                    p::bytes(
                        2,
                        &p::message(&[
                            p::bytes(1, b"synthetic_self"),
                            p::bytes(2, b"Self Nick"),
                            p::bytes(3, b"Self Room Name"),
                            p::uint(6, 1),
                            p::bytes(7, b"inviter-self"),
                        ])
                        .unwrap(),
                    ),
                    p::bytes(
                        2,
                        &p::message(&[
                            p::bytes(1, b"peer"),
                            p::bytes(2, b"Peer Nick"),
                            p::bytes(3, b"Peer Room Name"),
                            p::uint(6, 2),
                            p::bytes(7, b"inviter-peer"),
                        ])
                        .unwrap(),
                    ),
                ])
                .unwrap(),
            ),
            p::bytes(5, b"peer"),
            p::uint(6, 2),
            p::bytes(7, &p::bytes(1, b"synthetic_self")),
            p::bytes(7, &p::bytes(1, b"peer")),
        ])
        .unwrap();
        let initial_reply = MetadataReply {
            value: wechat_protocol::wire::decode_room(&initial_raw, ROOM).unwrap(),
            raw: initial_raw,
        };
        core.project_metadata_roster(&location, &keys, &initial_reply, &options, true)
            .unwrap();
        assert_eq!(
            core.group_member("one", ROOM, "synthetic_self").unwrap()["member"]["roomDisplayName"],
            "Self Room Name"
        );
        assert_eq!(
            core.group_member("one", ROOM, "peer").unwrap()["member"]["roomDisplayName"],
            "Peer Room Name"
        );

        // Delta CGI 551 reply: only `peer` changed and cleared its display name (field 3 omitted)
        // and opaque inviter (field 7 omitted). `synthetic_self` is omitted from f4 and kept via f7.
        // Also include an extra removed member in f4 absent from f7 to match official f7 filtering.
        let delta_clear_raw = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(3, 8),
            p::bytes(
                4,
                &p::message(&[
                    p::uint(1, 2),
                    p::bytes(
                        2,
                        &p::message(&[
                            p::bytes(1, b"peer"),
                            p::bytes(2, b"Peer Nick"),
                            p::uint(6, 0),
                        ])
                        .unwrap(),
                    ),
                    p::bytes(
                        2,
                        &p::message(&[
                            p::bytes(1, b"removed_member"),
                            p::bytes(2, b"Removed"),
                            p::uint(6, 0),
                        ])
                        .unwrap(),
                    ),
                ])
                .unwrap(),
            ),
            p::bytes(5, b"peer"),
            p::uint(6, 2),
            p::bytes(7, &p::bytes(1, b"synthetic_self")),
            p::bytes(7, &p::bytes(1, b"peer")),
        ])
        .unwrap();
        let delta_reply = MetadataReply {
            value: wechat_protocol::wire::decode_room(&delta_clear_raw, ROOM).unwrap(),
            raw: delta_clear_raw,
        };
        core.project_metadata_roster(&location, &keys, &delta_reply, &options, true)
            .unwrap();
        let peer_member = core.group_member("one", ROOM, "peer").unwrap();
        assert_eq!(peer_member["member"]["roomDisplayName"], Value::Null);
        assert_eq!(peer_member["member"]["displayName"], "Peer Nick");
        assert_eq!(peer_member["member"]["mentionName"], "Peer Nick");
        assert_eq!(
            core.group_member("one", ROOM, "synthetic_self").unwrap()["member"]["roomDisplayName"],
            "Self Room Name"
        );
        assert_eq!(
            core.group_member("one", ROOM, "removed_member").unwrap()["state"],
            "unconfirmed"
        );

        // Full incoming sync refresh clears `synthetic_self` too when wire field 3 is omitted.
        let (key, mut job) = staged(&core);
        job.roster_raw = Some(
            p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(3, 9),
                p::bytes(
                    4,
                    &p::message(&[
                        p::uint(1, 2),
                        p::bytes(
                            2,
                            &p::message(&[
                                p::bytes(1, b"synthetic_self"),
                                p::bytes(2, b"Self Nick"),
                                p::uint(6, 0),
                            ])
                            .unwrap(),
                        ),
                        p::bytes(
                            2,
                            &p::message(&[
                                p::bytes(1, b"peer"),
                                p::bytes(2, b"Peer Nick"),
                                p::uint(6, 0),
                            ])
                            .unwrap(),
                        ),
                    ])
                    .unwrap(),
                ),
                p::bytes(5, b"peer"),
                p::uint(6, 2),
                p::bytes(7, &p::bytes(1, b"synthetic_self")),
                p::bytes(7, &p::bytes(1, b"peer")),
            ])
            .unwrap(),
        );
        core.save_incoming_group("one", &key, &job).unwrap();
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 1);
        assert_eq!(
            core.group_info("one", ROOM).unwrap()["myDisplayName"],
            Value::Null
        );
        let self_member = core.group_member("one", ROOM, "synthetic_self").unwrap();
        assert_eq!(self_member["member"]["roomDisplayName"], Value::Null);
        assert_eq!(self_member["member"]["displayName"], "Self Nick");
    }

    fn room_contact(room: &str, name: &str, signals: bool) -> Vec<u8> {
        let mut f = vec![
            p::bytes(1, &p::bytes(1, room.as_bytes())),
            p::bytes(2, &p::bytes(1, name.as_bytes())),
            p::bytes(3, b""),
            p::bytes(4, b""),
            p::uint(5, 0),
            p::bytes(6, &p::uint(1, 0)),
            p::uint(7, u64::from(u32::MAX)),
            p::uint(8, 2),
            p::uint(9, 3),
            p::uint(17, 0),
            p::uint(85, 1),
        ];
        if signals {
            f.extend([p::uint(53, 7), p::bytes(57, &p::uint(1, 2)), p::uint(64, 3)]);
        }
        p::message(&f).unwrap()
    }

    fn room_contact_reply(room: &str, name: &str) -> Vec<u8> {
        p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 1),
            p::bytes(3, &room_contact(room, name, false)),
            p::bytes(4, &[0]),
        ])
        .unwrap()
    }

    fn room_roster_reply() -> Vec<u8> {
        let self_member = p::message(&[
            p::bytes(1, b"synthetic_self"),
            p::bytes(2, b"Self Nick"),
            p::bytes(3, b"Self Room Display"),
            p::uint(6, 1),
            p::bytes(7, b"opaque"),
        ])
        .unwrap();
        let peer_member = p::message(&[
            p::bytes(1, b"peer"),
            p::bytes(2, b"Peer Nick"),
            p::uint(6, 0),
        ])
        .unwrap();
        p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(3, 7),
            p::bytes(
                4,
                &p::message(&[
                    p::uint(1, 2),
                    p::bytes(2, &self_member),
                    p::bytes(2, &peer_member),
                ])
                .unwrap(),
            ),
            p::bytes(5, b"peer"),
            p::uint(6, 2),
            p::bytes(7, &p::bytes(1, b"synthetic_self")),
            p::bytes(7, &p::bytes(1, b"peer")),
            p::uint(8, 1),
            p::bytes(9, &p::bytes(1, b"synthetic_self")),
        ])
        .unwrap()
    }

    fn room_detail_reply(announcement: &str) -> Vec<u8> {
        p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::bytes(2, announcement.as_bytes()),
            p::uint(3, 3),
            p::bytes(4, b"synthetic_self"),
            p::uint(5, 100),
            p::bytes(12, b"<announcement>Synthetic XML</announcement>"),
        ])
        .unwrap()
    }

    fn queue_room(core: &Core, room: &str, receipt: &str) -> String {
        let command = SyncCommand {
            id: 2,
            payload: room_contact(room, &format!("Initial {room}"), true),
        };
        core.apply_sync_command("one", &command, receipt, "live")
            .unwrap();
        key(receipt)
    }

    fn load_pending_job(core: &Core, job_key: &str) -> Refresh {
        let sealed = core.store.runtime("one", job_key).unwrap().unwrap();
        serde_json::from_slice(&core.vault.open_record("one", job_key, &sealed).unwrap()).unwrap()
    }

    fn waker_fired(waker: &tokio::sync::Notify) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_millis(20), waker.notified())
                    .await
                    .is_ok()
            })
    }

    type RosterHook = Box<dyn Fn(&str) + Send + Sync>;

    struct BatchServer {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        looked_up: Arc<Mutex<Vec<String>>>,
        fail_room: Option<String>,
        on_roster: Option<RosterHook>,
    }
    impl crate::upstream::Upstream for BatchServer {
        fn kind(&self) -> crate::upstream::UpstreamKind {
            crate::upstream::UpstreamKind::Native
        }
        fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let room = &ids[0];
            self.looked_up.lock().unwrap().push(room.clone());
            if self.fail_room.as_deref() == Some(room.as_str()) {
                bail!("synthetic_network_error");
            }
            Ok(room_contact_reply(room, &format!("Fresh {room}")))
        }
        fn lookup_room_raw(&self, _: &mut AccountState, room: &str, _: u32) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = &self.on_roster {
                hook(room);
            }
            Ok(room_roster_reply())
        }
        fn group_detail_raw(&self, _: &mut AccountState, room: &str) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(room_detail_reply(&format!("Notice {room}")))
        }
    }

    #[test]
    fn enqueue_wakes_account_and_batch_refreshes_up_to_limit_then_finishes_rest() {
        use std::sync::atomic::AtomicUsize;

        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let waker = core.account_waker("one");
        let calls = Arc::new(AtomicUsize::new(0));
        let looked_up = Arc::new(Mutex::new(Vec::new()));
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(BatchServer {
            calls: calls.clone(),
            looked_up: looked_up.clone(),
            fail_room: None,
            on_roster: None,
        });

        let total = GROUPS_PER_PASS + 2;
        let rooms: Vec<String> = (0..total)
            .map(|i| format!("synthetic-batch-{i}@chatroom"))
            .collect();
        // First enqueue wakes the account loop; re-enqueuing the same receipt does not.
        queue_room(&core, &rooms[0], "batch-receipt-0");
        assert!(waker_fired(&waker));
        let record =
            wechat_protocol::wire::decode_contact_record(&room_contact(&rooms[0], "Dup", true))
                .unwrap();
        assert!(core
            .queue_incoming_group("one", &record, "batch-receipt-0", "live")
            .unwrap());
        assert!(!waker_fired(&waker));

        for (index, room) in rooms.iter().enumerate().skip(1) {
            queue_room(&core, room, &format!("batch-receipt-{index}"));
        }
        // Drain the enqueue wake before testing pass-level wakes.
        assert!(waker_fired(&waker));
        assert_eq!(
            core.store.incoming_group_refreshes("one").unwrap().len(),
            total
        );

        // Pass 1 applies GROUPS_PER_PASS groups and wakes because due jobs remain.
        assert_eq!(
            core.refresh_incoming_groups("one", true).unwrap(),
            GROUPS_PER_PASS
        );
        assert_eq!(calls.load(Ordering::SeqCst), GROUPS_PER_PASS * 3);
        assert_eq!(
            core.store.incoming_group_refreshes("one").unwrap().len(),
            total - GROUPS_PER_PASS
        );
        assert!(waker_fired(&waker));

        // Pass 2 finishes the remaining groups and does not wake once drained.
        assert_eq!(
            core.refresh_incoming_groups("one", true).unwrap(),
            total - GROUPS_PER_PASS
        );
        assert_eq!(calls.load(Ordering::SeqCst), total * 3);
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        assert!(!waker_fired(&waker));

        for room in &rooms {
            let info = core.group_info("one", room).unwrap();
            assert_eq!(info["name"], format!("Fresh {room}"));
            assert_eq!(info["announcement"], format!("Notice {room}"));
            assert_eq!(
                core.group_member("one", room, "peer").unwrap()["state"],
                "present"
            );
        }
    }

    #[test]
    fn network_error_stops_pass_backs_off_failing_group_and_holds_after_three_failures() {
        use std::sync::atomic::AtomicUsize;

        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let waker = core.account_waker("one");
        let calls = Arc::new(AtomicUsize::new(0));
        let looked_up = Arc::new(Mutex::new(Vec::new()));
        let failing = "synthetic-err-2@chatroom".to_owned();
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(BatchServer {
            calls: calls.clone(),
            looked_up: looked_up.clone(),
            fail_room: Some(failing.clone()),
            on_roster: None,
        });

        let keys: Vec<String> = (0..5)
            .map(|i| {
                queue_room(
                    &core,
                    &format!("synthetic-err-{i}@chatroom"),
                    &format!("err-{i}"),
                )
            })
            .collect();
        assert!(waker_fired(&waker));

        // Pass 1 applies groups 0 and 1, fails on group 2 and stops without touching 3 or 4.
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 2);
        assert_eq!(
            *looked_up.lock().unwrap(),
            vec![
                "synthetic-err-0@chatroom",
                "synthetic-err-1@chatroom",
                "synthetic-err-2@chatroom",
            ]
        );
        assert!(!waker_fired(&waker));
        let failed_once = load_pending_job(&core, &keys[2]);
        assert_eq!(failed_once.attempts, 1);
        assert!(failed_once.not_before > 0);
        assert_eq!(load_pending_job(&core, &keys[3]).attempts, 0);
        assert_eq!(load_pending_job(&core, &keys[4]).attempts, 0);

        // While group 2 is backed off, healthy groups 3 and 4 run without being blocked.
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 2);
        assert_eq!(core.store.incoming_group_refreshes("one").unwrap().len(), 1);
        assert!(!waker_fired(&waker));

        // Second failure keeps the job queued with a longer backoff and no held event yet.
        let mut retry2 = load_pending_job(&core, &keys[2]);
        let first_backoff = retry2.not_before;
        retry2.not_before = 0;
        core.save_incoming_group("one", &keys[2], &retry2).unwrap();
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        let failed_twice = load_pending_job(&core, &keys[2]);
        assert_eq!(failed_twice.attempts, 2);
        assert!(failed_twice.not_before >= first_backoff + RETRY_BACKOFF_BASE_SECONDS);
        assert!(!waker_fired(&waker));
        assert!(!core
            .store
            .events("one", 0, 200)
            .unwrap()
            .iter()
            .any(|event| event.kind == "group.refresh_blocked"));

        // Third failure moves the job to held: and emits group.refresh_blocked.
        let mut retry3 = failed_twice;
        retry3.not_before = 0;
        core.save_incoming_group("one", &keys[2], &retry3).unwrap();
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 0);
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        assert!(core
            .store
            .runtime("one", &keys[2].replace("pending:", "held:"))
            .unwrap()
            .is_some());
        let held_events: Vec<_> = core
            .store
            .events("one", 0, 200)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "group.refresh_blocked")
            .collect();
        assert_eq!(held_events.len(), 1);
        assert_eq!(held_events[0].data["groupId"], failing);
        assert!(!waker_fired(&waker));
    }

    #[test]
    fn allow_fetch_false_never_calls_upstream_and_finishes_owned_jobs() {
        use std::sync::atomic::AtomicUsize;

        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let calls = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(BatchServer {
            calls: calls.clone(),
            looked_up: Arc::new(Mutex::new(Vec::new())),
            fail_room: None,
            on_roster: None,
        });

        let k0 = queue_room(&core, "synthetic-local-0@chatroom", "local-0");
        let k1 = queue_room(&core, "synthetic-local-1@chatroom", "local-1");
        let k2 = queue_room(&core, "synthetic-local-2@chatroom", "local-2");
        let mut owned = load_pending_job(&core, &k1);
        owned.contact_revision = core
            .group_revision("one", "synthetic-local-1@chatroom")
            .unwrap();
        owned.contact_raw = Some(room_contact_reply(
            "synthetic-local-1@chatroom",
            "Owned title",
        ));
        owned.roster_raw = Some(room_roster_reply());
        owned.detail_raw = Some(room_detail_reply("Owned notice"));
        core.save_incoming_group("one", &k1, &owned).unwrap();

        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            core.group_info("one", "synthetic-local-1@chatroom")
                .unwrap()["name"],
            "Owned title"
        );
        assert_eq!(core.store.incoming_group_refreshes("one").unwrap().len(), 2);
        assert_eq!(load_pending_job(&core, &k0).attempts, 0);
        assert_eq!(load_pending_job(&core, &k2).attempts, 0);
    }

    #[test]
    fn supersede_handling_works_partway_through_a_batch() {
        use std::sync::atomic::AtomicUsize;

        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let calls = Arc::new(AtomicUsize::new(0));
        let looked_up = Arc::new(Mutex::new(Vec::new()));
        let core_slot: Arc<Mutex<Option<std::sync::Weak<Core>>>> = Arc::new(Mutex::new(None));
        let core_for_hook = core_slot.clone();
        let bumped = Arc::new(AtomicBool::new(false));
        let bumped_hook = bumped.clone();
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(BatchServer {
            calls: calls.clone(),
            looked_up: looked_up.clone(),
            fail_room: None,
            on_roster: Some(Box::new(move |room| {
                if room == "synthetic-sup-4@chatroom" && !bumped_hook.swap(true, Ordering::SeqCst) {
                    if let Some(core) = core_for_hook
                        .lock()
                        .unwrap()
                        .as_ref()
                        .and_then(std::sync::Weak::upgrade)
                    {
                        core.mark_group_revision("one", room, "concurrent-mid-fetch")
                            .unwrap();
                    }
                }
            })),
        });
        *core_slot.lock().unwrap() = Some(Arc::downgrade(&core));

        // Part A: pre-superseded job partway through a batch archives once and refetches cleanly.
        let _k0 = queue_room(&core, "synthetic-sup-0@chatroom", "sup-0");
        let k1 = queue_room(&core, "synthetic-sup-1@chatroom", "sup-1");
        let _k2 = queue_room(&core, "synthetic-sup-2@chatroom", "sup-2");
        let mut stale = load_pending_job(&core, &k1);
        stale.contact_revision = core
            .group_revision("one", "synthetic-sup-1@chatroom")
            .unwrap();
        stale.contact_raw = Some(room_contact_reply(
            "synthetic-sup-1@chatroom",
            "Stale title",
        ));
        stale.roster_raw = Some(room_roster_reply());
        stale.detail_raw = Some(room_detail_reply("Stale notice"));
        core.save_incoming_group("one", &k1, &stale).unwrap();
        core.mark_group_revision("one", "synthetic-sup-1@chatroom", "concurrent-before")
            .unwrap();

        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 3);
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        assert_eq!(
            core.group_info("one", "synthetic-sup-1@chatroom").unwrap()["name"],
            "Fresh synthetic-sup-1@chatroom"
        );
        let saved: Vec<_> = core
            .store
            .runtime_prefix("one", "incoming-group-superseded:", 10)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(saved, [superseded(&k1, 1)]);

        // Part B: mid-fetch supersede on group 4 does not block groups 3 and 5,
        // keeps group 4 queued without overwriting, and refetches on the next pass.
        let _k3 = queue_room(&core, "synthetic-sup-3@chatroom", "sup-3");
        let k4 = queue_room(&core, "synthetic-sup-4@chatroom", "sup-4");
        let _k5 = queue_room(&core, "synthetic-sup-5@chatroom", "sup-5");
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 2);
        assert_eq!(core.store.incoming_group_refreshes("one").unwrap().len(), 1);
        assert_eq!(
            core.group_info("one", "synthetic-sup-4@chatroom").unwrap()["name"],
            "Initial synthetic-sup-4@chatroom"
        );
        assert_eq!(core.refresh_incoming_groups("one", true).unwrap(), 1);
        assert!(core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        assert_eq!(
            core.group_info("one", "synthetic-sup-4@chatroom").unwrap()["name"],
            "Fresh synthetic-sup-4@chatroom"
        );
        let saved: Vec<_> = core
            .store
            .runtime_prefix("one", "incoming-group-superseded:", 10)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(saved.contains(&superseded(&k4, 2)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refresh_with_permit_releases_account_lock_during_fetch_and_aborts_on_disconnect() {
        let (began_tx, began_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        struct BlockingServer {
            began: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            resume: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl crate::upstream::Upstream for BlockingServer {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
                Ok(room_contact_reply(&ids[0], "Blocked title"))
            }
            fn lookup_room_raw(&self, _: &mut AccountState, _: &str, _: u32) -> Result<Vec<u8>> {
                if let Some(tx) = self.began.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(room_roster_reply())
            }
            fn group_detail_raw(&self, _: &mut AccountState, _: &str) -> Result<Vec<u8>> {
                Ok(room_detail_reply("Blocked notice"))
            }
        }

        let (_temp, mut core) = super::super::groups::tests::fixture();
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(BlockingServer {
            began: Mutex::new(Some(began_tx)),
            resume: Mutex::new(resume_rx),
        });
        let job_key = queue_room(&core, "synthetic-lock@chatroom", "lock-0");
        let guard = core.lock_account("one").await.unwrap();
        let fence = core.session_fence("one").unwrap();
        let worker = core.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut permit = crate::account_network::AccountNetworkPermit {
                guard: Some(guard),
                runtime: tokio::runtime::Handle::current(),
            };
            worker.refresh_incoming_groups_with_permit(
                "one",
                true,
                Some(&mut permit),
                Some(&fence),
                Some(b"connected"),
            )
        });

        began_rx.await.unwrap();
        // Account lock must be released during blocking metadata RPCs.
        let concurrent_guard =
            tokio::time::timeout(std::time::Duration::from_secs(1), core.lock_account("one"))
                .await
                .unwrap()
                .unwrap();
        core.store
            .put_runtime("one", "desired", b"disconnected")
            .unwrap();
        drop(concurrent_guard);

        resume_tx.send(()).unwrap();
        assert!(task.await.unwrap().is_err());
        // Disconnecting mid-fetch must not hold the job or burn its retry budget.
        let remaining = load_pending_job(&core, &job_key);
        assert_eq!(remaining.attempts, 0);
        assert!(core
            .store
            .runtime("one", &job_key.replace("pending:", "held:"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn retry_delay_tracks_earliest_backed_off_current_epoch_job() {
        let (_temp, core) = super::super::groups::tests::fixture();
        assert_eq!(
            core.next_incoming_group_retry_delay("one", 5).unwrap(),
            None
        );

        // Ready jobs (not_before <= now) do not shorten the normal tick interval.
        let k0 = queue_room(&core, "synthetic-delay-0@chatroom", "delay-0");
        assert_eq!(
            core.next_incoming_group_retry_delay("one", 5).unwrap(),
            None
        );

        // Backed-off jobs clamp to [1, max_seconds].
        let now = chrono::Utc::now().timestamp();
        let mut far = load_pending_job(&core, &k0);
        far.not_before = now + 20;
        core.save_incoming_group("one", &k0, &far).unwrap();
        assert_eq!(
            core.next_incoming_group_retry_delay("one", 5).unwrap(),
            Some(5)
        );

        let k1 = queue_room(&core, "synthetic-delay-1@chatroom", "delay-1");
        let mut soon = load_pending_job(&core, &k1);
        soon.not_before = now + 2;
        core.save_incoming_group("one", &k1, &soon).unwrap();
        let delay = core
            .next_incoming_group_retry_delay("one", 5)
            .unwrap()
            .unwrap();
        assert!((1..=2).contains(&delay), "unexpected delay: {delay}");

        // Old-epoch backed-off jobs are ignored.
        core.store
            .put_runtime("one", "logout-epoch", b"later-epoch")
            .unwrap();
        assert_eq!(
            core.next_incoming_group_retry_delay("one", 5).unwrap(),
            None
        );
    }

    #[test]
    fn auth_rejected_propagates_without_holding_and_current_epoch_runs_before_old_epoch() {
        struct RejectAfterFirst {
            reject_room: String,
        }
        impl crate::upstream::Upstream for RejectAfterFirst {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn lookup_contacts_raw(&self, _: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
                if ids[0] == self.reject_room {
                    return Err(wechat_protocol::ServiceRejected::new(
                        wechat_protocol::Service::Contact,
                        wechat_protocol::ServiceRejected::SESSION_REJECTED,
                    )
                    .into());
                }
                Ok(room_contact_reply(&ids[0], &format!("Fresh {}", ids[0])))
            }
            fn lookup_room_raw(&self, _: &mut AccountState, _: &str, _: u32) -> Result<Vec<u8>> {
                Ok(room_roster_reply())
            }
            fn group_detail_raw(&self, _: &mut AccountState, room: &str) -> Result<Vec<u8>> {
                Ok(room_detail_reply(&format!("Notice {room}")))
            }
        }

        let (_temp, mut core) = super::super::groups::tests::fixture();
        super::super::groups::tests::make_group(&core);
        let rejected_room = "synthetic-reject@chatroom";
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = Arc::new(RejectAfterFirst {
            reject_room: rejected_room.into(),
        });

        // Queue an old-epoch job first (lower rowid), rotate logout-epoch, then
        // queue a healthy current-epoch job and one that triggers -13 rejection.
        let old_key = queue_room(&core, "synthetic-old-epoch@chatroom", "old-epoch-0");
        core.store
            .put_runtime("one", "logout-epoch", b"current-epoch")
            .unwrap();
        let healthy_room = "synthetic-current-ok@chatroom";
        let _healthy_key = queue_room(&core, healthy_room, "current-ok");
        let reject_key = queue_room(&core, rejected_room, "current-reject");

        // Current-epoch healthy job runs before the older-rowid old-epoch job;
        // then -13 propagates without burning attempts or moving the job to held:.
        let err = core.refresh_incoming_groups("one", true).unwrap_err();
        assert!(wechat_protocol::is_session_rejected(&err));
        assert_eq!(
            core.group_info("one", healthy_room).unwrap()["name"],
            format!("Fresh {healthy_room}")
        );
        assert_eq!(load_pending_job(&core, &reject_key).attempts, 0);
        assert_eq!(load_pending_job(&core, &old_key).attempts, 0);
        assert!(core
            .store
            .runtime("one", &reject_key.replace("pending:", "held:"))
            .unwrap()
            .is_none());
        assert!(!core
            .store
            .events("one", 0, 200)
            .unwrap()
            .iter()
            .any(|event| event.kind == "group.refresh_blocked"));
    }
}

#[cfg(test)]
#[path = "incoming_group_projection_tests.rs"]
mod projection_tests;
