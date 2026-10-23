//! Group version notifications schedule read-only reconciliation. Unknown
//! controls stay in the sync journal; refresh completion never resubmits edits.
use super::*;
use anyhow::ensure;
use wechat_profile::{GroupProjection, RawFields, SqlValue};
use wechat_protocol::{protobuf as p, ContactRecord, MetadataReply};

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
}
fn key(receipt: &str) -> String {
    format!(
        "incoming-group-pending:{:x}",
        Sha256::digest(receipt.as_bytes())
    )
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
            };
            self.save_incoming_group(account, &key, &job)?;
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
    /// A normal worker may fetch missing metadata. Local repair passes false
    /// and uses only responses already owned by the journal.
    pub(super) fn refresh_incoming_groups(
        &self,
        account: &str,
        allow_fetch: bool,
    ) -> Result<usize> {
        for (key, sealed) in self
            .store
            .incoming_group_refreshes(account)?
            .into_iter()
            .take(1)
        {
            let mut job: Refresh =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            if allow_fetch {
                job.attempts = job.attempts.saturating_add(1);
                self.save_incoming_group(account, &key, &job)?;
            }
            let outcome = (|| -> Result<bool> {
                self.account_state(account)?;
                ensure!(
                    job.epoch == self.store.runtime(account, "logout-epoch")?,
                    "authorization_generation_changed"
                );
                if allow_fetch {
                    let current = self.group_revision(account, &job.group)?;
                    if job.contact_raw.is_some()
                        && current != job.contact_revision
                        && current.as_deref() != Some(key.as_bytes())
                    {
                        let prior = format!("{}:superseded:{}", key, job.attempts);
                        self.store.put_runtime(
                            account,
                            &prior,
                            &sealed_receipt(self, account, &prior, &job)?,
                        )?;
                        job.contact_raw = None;
                        job.roster_raw = None;
                        job.detail_raw = None;
                    }
                }
                if job.contact_raw.is_none() {
                    if !allow_fetch {
                        return Ok(false);
                    }
                    job.contact_revision = self.group_revision(account, &job.group)?;
                    job.contact_raw =
                        Some(self.group_read(account, job.epoch.as_deref(), |state| {
                            client()?.lookup_contacts_raw(state, std::slice::from_ref(&job.group))
                        })?);
                    self.save_incoming_group(account, &key, &job)?;
                }
                if job.members && job.roster_raw.is_none() {
                    if !allow_fetch {
                        return Ok(false);
                    }
                    job.roster_raw =
                        Some(self.group_read(account, job.epoch.as_deref(), |state| {
                            client()?.lookup_room_raw(state, &job.group, 0)
                        })?);
                    self.save_incoming_group(account, &key, &job)?;
                }
                if job.detail && job.detail_raw.is_none() {
                    if !allow_fetch {
                        return Ok(false);
                    }
                    job.detail_raw =
                        Some(self.group_read(account, job.epoch.as_deref(), |state| {
                            client()?.group_detail_raw(state, &job.group)
                        })?);
                    self.save_incoming_group(account, &key, &job)?;
                }
                self.apply_incoming_group(account, &key, &job)?;
                // Retain all verified response bodies for offline local repair.
                let retained_key = key.replace("pending:", "receipt:");
                self.store.put_runtime(
                    account,
                    &retained_key,
                    &sealed_receipt(self, account, &retained_key, &job)?,
                )?;
                self.store.append_event_once(account,&format!("group-refreshed:{}",job.source),"group.updated",&json!({"groupId":job.group,"origin":job.origin,"source":"remote_sync_refresh","membersRefreshed":job.members,"announcementRefreshed":job.detail,"profileProjection":"partial"}))?;
                self.store.put_runtime(account, &done(&job.source), b"1")?;
                self.store.clear_runtime(account, &key)?;
                Ok(true)
            })();
            match outcome {
                Ok(true) => return Ok(1),
                Ok(false) => return Ok(0),
                Err(_) if job.attempts >= 3 && allow_fetch => {
                    let held_key = key.replace("pending:", "held:");
                    self.store.put_runtime(
                        account,
                        &held_key,
                        &sealed_receipt(self, account, &held_key, &job)?,
                    )?;
                    self.store.clear_runtime(account, &key)?;
                    self.store.append_event_once(account,&format!("group-refresh-held:{}",job.source),"group.refresh_blocked",&json!({"groupId":job.group,"source":"remote_sync","reason":"metadata_refresh_unavailable","retryable":false}))?;
                }
                Err(_) => return Ok(0),
            }
        }
        Ok(0)
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
    const ROOM: &str = "synthetic@chatroom";
    fn contact(name: &str, signals: bool) -> Vec<u8> {
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
            let control = p::message(&[
                p::uint(1, 1),
                p::uint(2, 14),
                p::uint(3, 1),
                p::bytes(4, &p::uint(1, 10)),
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
        assert_eq!(
            core.apply_sync_command("one", &command, "phone-change", "live")
                .unwrap_err()
                .to_string(),
            "group_metadata_refresh_pending"
        );
        let (key, sealed) = core
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .remove(0);
        let mut job: Refresh =
            serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap()).unwrap();
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
                p::bytes(5, b"synthetic_self"),
                p::uint(6, 2),
                p::bytes(7, &p::bytes(1, b"synthetic_self")),
                p::bytes(7, &p::bytes(1, b"peer")),
                p::uint(8, 0),
                p::bytes(9, b""),
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
        assert_eq!(
            core.apply_sync_command("one", &old, "phone-change", "live")
                .unwrap_err()
                .to_string(),
            "contact_update_control_unresolved"
        );
        assert_eq!(
            core.group_info("one", ROOM).unwrap()["name"],
            "Later phone edit"
        );
        assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
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
}
