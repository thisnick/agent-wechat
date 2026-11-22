//! Background expansion for oversized application-message stubs (type 49, subtype 40).
//!
//! When an application message's XML (such as a large Combine-and-Forward chat
//! history, subtype 19) exceeds the server's inline threshold, NewSync delivers a
//! subtype-40 stub carrying `<xmlfulllen>` and `<realinnertype>` without the
//! inner record. Like the official client's `AppBigXmlMessageHandler`, this
//! worker downloads the full XML via chunked CGI 221 (`downloadappattach`) and
//! updates the existing official `Msg_*` row in place (`message_content`,
//! `local_type`, `download_status`).
//!
//! Operational queue entries live in the encrypted sidecar runtime store and
//! are deleted once settled; canonical message content stays in the official
//! profile.
use super::*;
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use wechat_profile::AppMessageExpansion;
use wechat_protocol::{AppAttachRequest, ServiceRejected};

const BIG_XML_PREFIX: &str = "big-xml-expand:";
const MAX_STUB_XML_BYTES: usize = 256 * 1024;
const MAX_EXPANDED_XML_BYTES: usize = 1024 * 1024;
const MAX_CHUNKS: usize = 256;
const MAX_ATTEMPTS: u8 = 5;
const EXPANSIONS_PER_PASS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BigXmlStub {
    pub app_id: String,
    pub sdk_version: u32,
    pub media_id: String,
    pub advertised_total_len: u32,
    pub real_inner_type: Option<i32>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub has_inline_record: bool,
}

#[derive(Serialize, Deserialize)]
struct BigXmlJob {
    peer: String,
    sender: String,
    outgoing: bool,
    server_id: String,
    message_id: String,
    origin: String,
    direction: String,
    app_id: String,
    sdk_version: u32,
    media_id: String,
    advertised_total_len: u32,
    real_inner_type: Option<i32>,
    epoch: Option<Vec<u8>>,
    attempts: u8,
    #[serde(default)]
    not_before: i64,
    #[serde(default)]
    committed_emitted: bool,
    #[serde(default)]
    expanded_raw: Option<Vec<u8>>,
}

struct BigXmlSession<'a> {
    permit: &'a mut crate::account_network::AccountNetworkPermit,
    fence: &'a SessionFence,
    desired: Option<&'a [u8]>,
}

fn job_key(peer: &str, server_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(peer.as_bytes());
    hasher.update(b"\0");
    hasher.update(server_id.as_bytes());
    format!("{BIG_XML_PREFIX}{:x}", hasher.finalize())
}

fn parse_positive_i32(value: Option<String>) -> Option<i32> {
    value
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|&v| v > 0)
}

fn is_session_superseded(error: &anyhow::Error) -> bool {
    wechat_protocol::AccountInterrupted::find(error).is_some()
        || error.chain().any(|cause| {
            matches!(
                cause.to_string().as_str(),
                "session_superseded"
                    | "push_session_superseded"
                    | "authorization_generation_changed"
                    | "account runtime stopped"
            )
        })
}

fn strip_xml_prolog(text: &str) -> Result<&str> {
    let mut current = text.trim_start_matches('\u{feff}').trim_start();
    if let Some(rest) = current
        .strip_prefix("<?xml")
        .filter(|rest| rest.starts_with(char::is_whitespace))
    {
        let end = rest.find("?>").context("invalid_big_xml_payload")?;
        current = rest[end + 2..].trim_start();
    }
    while let Some(rest) = current.strip_prefix("<!--") {
        let end = rest.find("-->").context("invalid_big_xml_payload")?;
        current = rest[end + 3..].trim_start();
    }
    Ok(current)
}

/// Parse a subtype-40 stub XML (without any leading `<sender>:\n` group prefix).
pub(crate) fn parse_stub(content: &str) -> Option<BigXmlStub> {
    if content.len() > MAX_STUB_XML_BYTES
        || content.contains('\0')
        || content.to_ascii_uppercase().contains("<!DOCTYPE")
        || content.to_ascii_uppercase().contains("<!ENTITY")
    {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        content,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    if doc.descendants().count() > 4096
        || doc
            .descendants()
            .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    let root = doc.root_element();
    if root.tag_name().name() != "msg" || root.tag_name().namespace().is_some() {
        return None;
    }
    let app = crate::quotes::child(root, "appmsg")?;
    let subtype = crate::quotes::leaf(crate::quotes::child(app, "type")?)?
        .trim()
        .parse::<i32>()
        .ok()?;
    if subtype != 40 {
        return None;
    }
    let app_id = app
        .attribute("appid")
        .filter(|s| s.len() <= 512 && !s.contains('\0'))
        .unwrap_or("")
        .to_owned();
    let sdk_version = app
        .attribute("sdkver")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let attach = crate::quotes::child(app, "appattach");
    let media_id = attach
        .and_then(|a| crate::quotes::child(a, "attachid"))
        .and_then(crate::quotes::leaf)
        .filter(|s| s.len() <= 2048 && !s.contains('\0'))
        .unwrap_or_default();
    let advertised_total_len = attach
        .and_then(|a| crate::quotes::child(a, "totallen"))
        .and_then(crate::quotes::leaf)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| (n as usize) <= MAX_EXPANDED_XML_BYTES)
        .unwrap_or(0);
    let real_inner_type = parse_positive_i32(
        crate::quotes::child(app, "realinnertype").and_then(crate::quotes::leaf),
    );
    let title = crate::quotes::child(app, "title")
        .and_then(crate::quotes::leaf)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.contains('\0'));
    let description = crate::quotes::child(app, "des")
        .and_then(crate::quotes::leaf)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && s.len() <= 16384 && !s.contains('\0'));
    let has_inline_record = crate::quotes::child(app, "recorditem").is_some_and(|record| {
        crate::quotes::child(record, "recordinfo").is_some()
            || crate::quotes::leaf(record).is_some_and(|text| !text.trim().is_empty())
    });
    Some(BigXmlStub {
        app_id,
        sdk_version,
        media_id,
        advertised_total_len,
        real_inner_type,
        title,
        description,
        has_inline_record,
    })
}

fn assemble_expanded_xml(
    raw: &[u8],
    peer: &str,
    sender: &str,
    outgoing: bool,
    expected_inner_type: Option<i32>,
) -> Result<(u32, String, String)> {
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_EXPANDED_XML_BYTES,
        "invalid_big_xml_length"
    );
    let text = std::str::from_utf8(raw).context("invalid_big_xml_utf8")?;
    ensure!(
        !text.contains('\0')
            && !text.to_ascii_uppercase().contains("<!DOCTYPE")
            && !text.to_ascii_uppercase().contains("<!ENTITY"),
        "invalid_big_xml_payload"
    );
    let after_prolog = strip_xml_prolog(text)?;
    let has_msg_root = after_prolog
        .strip_prefix("<msg")
        .is_some_and(|rest| rest.starts_with('>') || rest.starts_with(char::is_whitespace));
    let wrapped = if has_msg_root {
        after_prolog.to_owned()
    } else {
        format!("<msg>{after_prolog}</msg>")
    };
    let (subtype, title) = super::sync_projection::app_fields(wrapped.as_bytes())?;
    ensure!(subtype > 0 && subtype != 40, "invalid_big_xml_inner_type");
    if let Some(expected) = expected_inner_type {
        ensure!(
            expected > 0 && subtype == expected as u32,
            "big_xml_inner_type_mismatch"
        );
    }
    // Verify the expanded payload fits our bounded readers so expansion can
    // never replace a readable unavailable stub with `body: null`.
    if subtype == 19 {
        ensure!(
            crate::forwarded::render(&wrapped).is_some(),
            "invalid_big_xml_payload"
        );
    } else {
        ensure!(
            crate::quotes::app_message(&wrapped).is_some(),
            "invalid_big_xml_payload"
        );
    }
    let stored = if peer.ends_with("@chatroom") && !outgoing && !sender.is_empty() {
        format!("{sender}:\n{wrapped}")
    } else {
        wrapped
    };
    ensure!(
        stored.len() <= MAX_EXPANDED_XML_BYTES,
        "invalid_big_xml_length"
    );
    Ok((subtype, stored, title.unwrap_or_default()))
}

impl Core {
    /// Queue a background CGI 221 download for a newly projected subtype-40 stub.
    /// Returns `true` when an expansion job is queued and will emit `message.committed`.
    pub(super) fn queue_big_xml_expansion(
        &self,
        account: &str,
        message: &wechat_protocol::AddMessage,
        body: &[u8],
        sender: &str,
        message_id: &str,
        origin: &str,
    ) -> Result<bool> {
        let Some(server_id) = message.server_message_id.as_deref() else {
            return Ok(false);
        };
        if server_id.parse::<u64>().ok().filter(|&id| id > 0).is_none() {
            return Ok(false);
        }
        let Ok(body_text) = std::str::from_utf8(body) else {
            return Ok(false);
        };
        let Some(stub) = parse_stub(body_text) else {
            return Ok(false);
        };
        if stub.has_inline_record {
            return Ok(false);
        }
        let key = job_key(&message.peer, server_id);
        if self.store.runtime(account, &key)?.is_none() {
            let job = BigXmlJob {
                peer: message.peer.clone(),
                sender: sender.to_owned(),
                outgoing: message.outgoing,
                server_id: server_id.to_owned(),
                message_id: message_id.to_owned(),
                origin: origin.to_owned(),
                direction: if message.outgoing {
                    "outgoing".into()
                } else {
                    "incoming".into()
                },
                app_id: stub.app_id,
                sdk_version: stub.sdk_version,
                media_id: stub.media_id,
                advertised_total_len: stub.advertised_total_len,
                real_inner_type: stub.real_inner_type,
                epoch: self.store.runtime(account, "logout-epoch")?,
                attempts: 0,
                not_before: 0,
                committed_emitted: false,
                expanded_raw: None,
            };
            self.save_big_xml_job(account, &key, &job)?;
        }
        self.wake_account(account);
        Ok(true)
    }

    fn save_big_xml_job(&self, account: &str, key: &str, job: &BigXmlJob) -> Result<()> {
        self.store.put_runtime(
            account,
            key,
            &self.vault.seal(account, key, &serde_json::to_vec(job)?)?,
        )
    }

    fn emit_big_xml_committed(&self, account: &str, job: &BigXmlJob) -> Result<()> {
        self.store.append_event_once(
            account,
            &format!("message:{}", job.message_id),
            "message.committed",
            &json!({
                "messageId": job.message_id,
                "threadId": job.peer,
                "origin": job.origin,
                "direction": job.direction,
            }),
        )?;
        Ok(())
    }

    fn emit_big_xml_updated(&self, account: &str, job: &BigXmlJob) -> Result<()> {
        self.store.append_event_once(
            account,
            &format!("message-expanded:{}", job.message_id),
            "message.updated",
            &json!({
                "messageId": job.message_id,
                "threadId": job.peer,
                "origin": job.origin,
                "direction": job.direction,
            }),
        )?;
        Ok(())
    }

    fn big_xml_row_state(&self, account: &str, message_id: &str) -> Result<Option<String>> {
        let source = crate::source_message_id(message_id)?;
        let own = self
            .account_state(account)
            .ok()
            .map(|state| state.own_username.clone());
        match self
            .profile_reader(account)?
            .message(&source, own.as_deref())
        {
            Ok(msg) => Ok(Some(msg.local_type)),
            Err(wechat_profile::ProfileError::NotCached) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn download_big_xml_bytes(
        &self,
        account: &str,
        job: &BigXmlJob,
        mut session: Option<&mut BigXmlSession<'_>>,
    ) -> Result<Vec<u8>> {
        let server_message_id = job
            .server_id
            .parse::<u64>()
            .context("invalid_big_xml_server_id")?;
        let mut total_length = job.advertised_total_len;
        let mut start_pos = 0u32;
        let mut assembled = Vec::new();
        for _ in 0..MAX_CHUNKS {
            let request = AppAttachRequest {
                app_id: job.app_id.clone(),
                sdk_version: job.sdk_version,
                media_id: job.media_id.clone(),
                username: job.peer.clone(),
                total_length,
                start_pos,
                app_type: 40,
                server_message_id,
            };
            let chunk = self.big_xml_rpc(
                account,
                job.epoch.as_deref(),
                session.as_deref_mut(),
                |upstream, state| upstream.download_app_attach(state, &request),
            )?;
            ensure!(
                assembled.len().saturating_add(chunk.bytes.len()) <= MAX_EXPANDED_XML_BYTES,
                "big_xml_too_large"
            );
            assembled.extend_from_slice(&chunk.bytes);
            total_length = chunk.total_length;
            start_pos = chunk.next_pos;
            if chunk.complete {
                ensure!(
                    assembled.len() == total_length as usize,
                    "big_xml_incomplete"
                );
                return Ok(assembled);
            }
        }
        bail!("big_xml_chunk_limit_exceeded")
    }

    fn big_xml_rpc<T, F>(
        &self,
        account: &str,
        epoch: Option<&[u8]>,
        session: Option<&mut BigXmlSession<'_>>,
        read: F,
    ) -> Result<T>
    where
        F: FnOnce(&Arc<dyn crate::upstream::Upstream>, &mut AccountState) -> Result<T>,
    {
        let (active_fence, active_desired, permit) = match session {
            Some(s) => (
                s.fence.clone(),
                s.desired.map(ToOwned::to_owned),
                Some(&mut *s.permit),
            ),
            None => (
                self.session_fence(account)?,
                self.store.runtime(account, "desired")?,
                None,
            ),
        };
        ensure!(
            self.store.runtime(account, "logout-epoch")?.as_deref() == epoch,
            "authorization_generation_changed"
        );
        let mut state = self.session_state(account, &active_fence)?;
        let expected = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let before_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&state)?);
        let upstream = self.account_upstream(account)?;
        let value = match permit {
            Some(permit) => permit.transfer(self, account, || read(&upstream, &mut state))?,
            None => read(&upstream, &mut state)?,
        };
        ensure!(
            self.store.runtime(account, "logout-epoch")?.as_deref() == epoch,
            "authorization_generation_changed"
        );
        let returned_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&state)?);
        if returned_bytes.as_slice() == before_bytes.as_slice() {
            self.session_state(account, &active_fence)?;
            ensure!(
                self.store.runtime(account, "desired")?.as_deref() == active_desired.as_deref(),
                "session_superseded"
            );
            self.check_state_profile(account, &state)?;
        } else {
            self.save_network_state(
                account,
                &state,
                &expected,
                &active_fence,
                active_desired.as_deref(),
            )?;
        }
        Ok(value)
    }

    fn apply_big_xml_update(
        &self,
        account: &str,
        intent: &str,
        job: &BigXmlJob,
        update: &AppMessageExpansion,
    ) -> Result<Option<wechat_profile::SourceMessageId>> {
        let (location, keys) = self.profile_location_keys(account)?;
        let options = self.write_options(account, intent)?;
        Ok(wechat_profile::apply_app_message_expansion(
            &location,
            &keys,
            &job.peer,
            &job.server_id,
            update,
            &options,
        )?)
    }

    fn settle_expanded_big_xml(
        &self,
        account: &str,
        key: &str,
        job: &BigXmlJob,
        row_state: Option<&str>,
    ) -> Result<bool> {
        let Some(raw) = job.expanded_raw.as_deref() else {
            return Ok(false);
        };
        let (subtype, content, summary) = match assemble_expanded_xml(
            raw,
            &job.peer,
            &job.sender,
            job.outgoing,
            job.real_inner_type,
        ) {
            Ok(parsed) => parsed,
            Err(_) => {
                let _ = self.apply_big_xml_update(account, key, job, &AppMessageExpansion::Failed);
                if !job.committed_emitted {
                    self.emit_big_xml_committed(account, job)?;
                }
                self.store.clear_runtime(account, key)?;
                return Ok(false);
            }
        };
        let expected_local_type = (((u64::from(subtype)) << 32) | 49).to_string();
        let applied = self.apply_big_xml_update(
            account,
            key,
            job,
            &AppMessageExpansion::Expanded {
                subtype,
                content,
                summary,
            },
        )?;
        let already_expanded = row_state == Some(expected_local_type.as_str());
        if applied.is_some() || already_expanded {
            if !job.committed_emitted {
                self.emit_big_xml_committed(account, job)?;
            } else {
                self.emit_big_xml_updated(account, job)?;
            }
            if applied.is_some() && job.origin != "profile_import" {
                let _ = self.queue_received_media(account, &job.message_id);
            }
        }
        self.store.clear_runtime(account, key)?;
        Ok(applied.is_some())
    }

    /// Expand pending subtype-40 BigXml messages.
    ///
    /// Local repair passes `allow_fetch = false` and only applies responses
    /// already stored in the operational receipt, never contacting the network.
    pub(super) fn refresh_big_xml(&self, account: &str, allow_fetch: bool) -> Result<usize> {
        self.refresh_big_xml_with_permit(account, allow_fetch, None, None, None)
    }

    pub(super) fn refresh_big_xml_with_permit(
        &self,
        account: &str,
        allow_fetch: bool,
        permit: Option<&mut crate::account_network::AccountNetworkPermit>,
        fence: Option<&SessionFence>,
        desired: Option<&[u8]>,
    ) -> Result<usize> {
        let mut session = match (permit, fence) {
            (Some(permit), Some(fence)) => Some(BigXmlSession {
                permit,
                fence,
                desired,
            }),
            _ => None,
        };
        let now = chrono::Utc::now().timestamp();
        let stub_local_type = ((40u64 << 32) | 49).to_string();
        let mut settled = 0usize;
        let mut ready: Vec<(String, BigXmlJob)> = Vec::new();

        // Phase 1: settle stale rows and finish local writeback of any already-owned
        // verified bytes first so backoff jobs can never starve local repair or new arrivals.
        for (key, sealed) in self.store.big_xml_jobs(account)? {
            let job: BigXmlJob =
                serde_json::from_slice(&self.vault.open_record(account, &key, &sealed)?)?;
            let row_state = self.big_xml_row_state(account, &job.message_id)?;
            match row_state.as_deref() {
                None | Some("10000") => {
                    // Missing or already recalled; never overwrite or emit a stale commit.
                    self.store.clear_runtime(account, &key)?;
                    continue;
                }
                Some(current) if current != stub_local_type && job.expanded_raw.is_none() => {
                    // Already expanded by another path (for example adopted official profile).
                    if !job.committed_emitted {
                        self.emit_big_xml_committed(account, &job)?;
                    }
                    self.store.clear_runtime(account, &key)?;
                    continue;
                }
                _ => {}
            }

            if job.expanded_raw.is_some() {
                // Already-owned verified bytes finish local writeback even after logout or offline.
                if self.settle_expanded_big_xml(account, &key, &job, row_state.as_deref())? {
                    settled += 1;
                }
                continue;
            }

            if job.epoch != self.store.runtime(account, "logout-epoch")? {
                if !job.committed_emitted {
                    self.emit_big_xml_committed(account, &job)?;
                }
                self.store.clear_runtime(account, &key)?;
                continue;
            }

            if allow_fetch && job.not_before <= now {
                ready.push((key, job));
            }
        }

        if !allow_fetch || ready.is_empty() || self.media_network_suspended(account)? {
            return Ok(settled);
        }

        // Prefer fresh arrivals (attempts == 0) over retries while preserving FIFO within each tier.
        ready.sort_by_key(|(_, job)| (job.attempts, job.not_before));

        // Phase 2: fetch up to EXPANSIONS_PER_PASS ready jobs, releasing the account
        // writer permit across each blocking chunk request and revalidating row state after.
        for (key, mut job) in ready.into_iter().take(EXPANSIONS_PER_PASS) {
            if job.epoch != self.store.runtime(account, "logout-epoch")? {
                if !job.committed_emitted {
                    self.emit_big_xml_committed(account, &job)?;
                }
                self.store.clear_runtime(account, &key)?;
                continue;
            }
            let before_state = self.big_xml_row_state(account, &job.message_id)?;
            match before_state.as_deref() {
                None | Some("10000") => {
                    self.store.clear_runtime(account, &key)?;
                    continue;
                }
                Some(current) if current != stub_local_type => {
                    if !job.committed_emitted {
                        self.emit_big_xml_committed(account, &job)?;
                    }
                    self.store.clear_runtime(account, &key)?;
                    continue;
                }
                _ => {}
            }

            match self.download_big_xml_bytes(account, &job, session.as_mut()) {
                Ok(raw) => {
                    let after_state = self.big_xml_row_state(account, &job.message_id)?;
                    match after_state.as_deref() {
                        None | Some("10000") => {
                            self.store.clear_runtime(account, &key)?;
                            continue;
                        }
                        Some(current) if current != stub_local_type => {
                            if !job.committed_emitted {
                                self.emit_big_xml_committed(account, &job)?;
                            }
                            self.store.clear_runtime(account, &key)?;
                            continue;
                        }
                        _ => {}
                    }
                    match assemble_expanded_xml(
                        &raw,
                        &job.peer,
                        &job.sender,
                        job.outgoing,
                        job.real_inner_type,
                    ) {
                        Ok(_) => {
                            // Persist verified bytes before writing the official profile so
                            // local repair can finish after an interruption without refetching.
                            job.expanded_raw = Some(raw);
                            self.save_big_xml_job(account, &key, &job)?;
                            if self.settle_expanded_big_xml(
                                account,
                                &key,
                                &job,
                                after_state.as_deref(),
                            )? {
                                settled += 1;
                            }
                        }
                        Err(_) => {
                            let _ = self.apply_big_xml_update(
                                account,
                                &key,
                                &job,
                                &AppMessageExpansion::Failed,
                            );
                            if !job.committed_emitted {
                                self.emit_big_xml_committed(account, &job)?;
                            }
                            self.store.clear_runtime(account, &key)?;
                        }
                    }
                }
                Err(error) => {
                    let auth_rejected = wechat_protocol::is_session_rejected(&error)
                        || super::session_auth::authentication_required(&error);
                    let after_state = self.big_xml_row_state(account, &job.message_id)?;
                    if matches!(after_state.as_deref(), None | Some("10000")) {
                        self.store.clear_runtime(account, &key)?;
                        if auth_rejected {
                            return Err(error);
                        }
                        continue;
                    }
                    if auth_rejected {
                        if !job.committed_emitted {
                            self.emit_big_xml_committed(account, &job)?;
                            job.committed_emitted = true;
                            self.save_big_xml_job(account, &key, &job)?;
                        }
                        return Err(error);
                    }
                    if is_session_superseded(&error) {
                        return Err(error);
                    }
                    let service_rejected =
                        ServiceRejected::find(&error).is_some_and(|r| !r.is_session_rejected());
                    job.attempts = job.attempts.saturating_add(1);
                    let terminal = service_rejected || job.attempts >= MAX_ATTEMPTS;
                    if terminal {
                        let _ = self.apply_big_xml_update(
                            account,
                            &key,
                            &job,
                            &AppMessageExpansion::Failed,
                        );
                        if !job.committed_emitted {
                            self.emit_big_xml_committed(account, &job)?;
                        }
                        self.store.clear_runtime(account, &key)?;
                        continue;
                    }
                    if !job.committed_emitted {
                        self.emit_big_xml_committed(account, &job)?;
                        job.committed_emitted = true;
                    }
                    job.not_before = now + (15i64 << (job.attempts - 1)).min(600);
                    self.save_big_xml_job(account, &key, &job)?;
                    break;
                }
            }
        }
        Ok(settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wechat_protocol::{protobuf as p, wire::SyncCommand, AppAttachChunk, Service};

    const ROOM: &str = "room@chatroom";
    const STUB_XML: &str = "wxid_alice:\n<msg><appmsg><title>Chat History for Team</title><des>Alice: preview excerpt</des><url>https://example.invalid/upgrade</url><type>40</type><xmlfulllen>210</xmlfulllen><realinnertype>19</realinnertype></appmsg><extcommoninfo><media_expire_at>2000000000</media_expire_at></extcommoninfo></msg>";
    const FULL_APPMSG: &str = "<?xml version=\"1.0\"?>\n<appmsg appid=\"\" sdkver=\"0\"><title>Chat History for Team</title><des>Alice: preview excerpt</des><type>19</type><recorditem>&lt;recordinfo&gt;&lt;datalist&gt;&lt;dataitem datatype=&quot;1&quot;&gt;&lt;sourcename&gt;Alice&lt;/sourcename&gt;&lt;datadesc&gt;Full message one&lt;/datadesc&gt;&lt;/dataitem&gt;&lt;dataitem datatype=&quot;1&quot;&gt;&lt;sourcename&gt;Bob&lt;/sourcename&gt;&lt;datadesc&gt;Full message two&lt;/datadesc&gt;&lt;/dataitem&gt;&lt;/datalist&gt;&lt;/recordinfo&gt;</recorditem></appmsg>";

    fn add_msg_command(peer: &str, content: &str, server_id: u64, created_at: u64) -> SyncCommand {
        add_msg_command_dir(peer, "account", content, server_id, created_at)
    }

    fn add_msg_command_dir(
        from: &str,
        to: &str,
        content: &str,
        server_id: u64,
        created_at: u64,
    ) -> SyncCommand {
        let payload = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::bytes(1, from.as_bytes())),
            p::bytes(3, &p::bytes(1, to.as_bytes())),
            p::uint(4, 49),
            p::bytes(5, &p::bytes(1, content.as_bytes())),
            p::uint(6, 3),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, created_at),
            p::bytes(10, b"<msgsource/>"),
            p::uint(12, server_id),
            p::uint(13, 100),
        ])
        .unwrap();
        SyncCommand { id: 5, payload }
    }

    struct ChunkedUpstream {
        payload: Vec<u8>,
        chunk_size: usize,
        fail_first: AtomicUsize,
        reject: Option<(Service, i32)>,
        rotate_cookie: bool,
        calls: AtomicUsize,
    }

    impl ChunkedUpstream {
        fn new(payload: &[u8], chunk_size: usize) -> Self {
            Self {
                payload: payload.to_vec(),
                chunk_size,
                fail_first: AtomicUsize::new(0),
                reject: None,
                rotate_cookie: false,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl crate::upstream::Upstream for ChunkedUpstream {
        fn kind(&self) -> crate::upstream::UpstreamKind {
            crate::upstream::UpstreamKind::Native
        }
        fn download_app_attach(
            &self,
            state: &mut AccountState,
            request: &AppAttachRequest,
        ) -> Result<AppAttachChunk> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some((service, code)) = self.reject {
                return Err(ServiceRejected::new(service, code).into());
            }
            if self.fail_first.load(Ordering::SeqCst) > 0 {
                self.fail_first.fetch_sub(1, Ordering::SeqCst);
                bail!("transient_network_error");
            }
            if self.rotate_cookie {
                state.cookie = vec![(call as u8).wrapping_add(10); 15];
            }
            assert_eq!(request.app_type, 40);
            let start = request.start_pos as usize;
            let end = (start + self.chunk_size).min(self.payload.len());
            let total = self.payload.len() as u32;
            Ok(AppAttachChunk {
                start_pos: request.start_pos,
                next_pos: end as u32,
                total_length: total,
                complete: end == self.payload.len(),
                bytes: self.payload[start..end].to_vec(),
            })
        }
    }

    fn test_core(upstream: Arc<dyn crate::upstream::Upstream>) -> (tempfile::TempDir, Arc<Core>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let mut core = Core::new(store, None).unwrap();
        Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
            .unwrap()
            .upstream = upstream;
        let state = AccountState {
            schema: 1,
            device: wechat_protocol::DeviceProfile::generate("linux".into()).unwrap(),
            uin: 7,
            own_username: "account".into(),
            cookie: vec![1; 15],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: wechat_protocol::protobuf::message(&[
                wechat_protocol::protobuf::bytes(
                    1,
                    &wechat_protocol::protobuf::buffer(b"synthetic-a"),
                ),
                wechat_protocol::protobuf::bytes(
                    2,
                    &wechat_protocol::protobuf::buffer(b"synthetic-b"),
                ),
            ])
            .unwrap(),
            key_info: vec![4; 180],
            database_password: vec![42u8; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };
        core.test_managed_profile("one", &state).unwrap();
        core.store
            .put_runtime("one", "desired", b"connected")
            .unwrap();
        (dir, core)
    }

    #[test]
    fn big_xml_downloads_in_chunks_expands_official_row_and_allows_sync_replay() {
        let mut up = ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 90);
        up.rotate_cookie = true;
        let upstream = Arc::new(up);
        let (_dir, core) = test_core(upstream.clone());
        let stub_with_different_title = "wxid_alice:\n<msg><appmsg><title>Upgrade Client</title><des>Alice: preview excerpt</des><url>https://example.invalid/upgrade</url><type>40</type><xmlfulllen>210</xmlfulllen><realinnertype>19</realinnertype></appmsg></msg>";
        let cmd = add_msg_command(ROOM, stub_with_different_title, 7001, 1700000100);

        core.apply_sync_command("one", &cmd, "r-bigxml-1", "live")
            .unwrap();

        // Sync acknowledged at once and deferred `message.committed` until attempt 1 finishes.
        assert!(core.store.runtime("one", "r-bigxml-1").unwrap().is_some());
        let events_before = core.store.events("one", 0, 50).unwrap();
        assert!(!events_before.iter().any(|e| e.kind == "message.committed"));

        // Even before expansion, API returns explicit unavailable text (never body: null)
        // and does not expose the upgrade URL as a link card.
        let before_list = core.messages("one", ROOM, Some(10), None).unwrap();
        let before_msg = &before_list["items"][0];
        assert_eq!(before_msg["type"], 49);
        assert_eq!(before_msg["appMessageType"], 40);
        assert!(before_msg["link"].is_null());
        assert_eq!(
            before_msg["body"],
            "[Chat History] Upgrade Client\nAlice: preview excerpt\n[Chat History unavailable]"
        );

        // Local repair (allow_fetch = false) makes zero network calls.
        assert_eq!(core.refresh_big_xml("one", false).unwrap(), 0);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);

        // Background fetch downloads all chunks (with cookie rotation on every chunk),
        // expands Msg_* to 49:19, and emits message.committed.
        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 1);
        assert!(upstream.calls.load(Ordering::SeqCst) > 1);

        let after_msg = core
            .message("one", before_msg["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(after_msg["type"], 49);
        assert_eq!(after_msg["appMessageType"], 19);
        assert_eq!(
            after_msg["body"],
            "[Chat History] Chat History for Team\nAlice: Full message one\nBob: Full message two"
        );

        let events_after = core.store.events("one", 0, 50).unwrap();
        assert_eq!(
            events_after
                .iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            1
        );
        assert!(!events_after.iter().any(|e| e.kind == "message.updated"));

        // Replaying the original 49:40 AddMsg against the expanded 49:19 row must not conflict
        // and must preserve the expanded SessionTable.summary rather than restoring the stub title.
        core.apply_sync_command("one", &cmd, "r-bigxml-replay", "live")
            .unwrap();
        assert!(core
            .store
            .runtime("one", "r-bigxml-replay")
            .unwrap()
            .is_some());
        let threads = core
            .profile_reader("one")
            .unwrap()
            .threads(&wechat_profile::ListQuery::default())
            .unwrap()
            .items;
        let session = threads.iter().find(|t| t.username == ROOM).unwrap();
        assert_eq!(
            session.raw_fields.get("summary"),
            Some(&wechat_profile::SqlValue::Text(
                "Chat History for Team".into()
            ))
        );
        assert_eq!(
            session.raw_fields.get("last_msg_sub_type"),
            Some(&wechat_profile::SqlValue::Integer("19".into()))
        );
    }

    #[test]
    fn manual_sync_with_desired_none_succeeds_without_resealing_unchanged_state() {
        let upstream = Arc::new(ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 120));
        let (_dir, core) = test_core(upstream.clone());
        // Simulate manual sync while disconnected (`desired == None`).
        core.store.clear_runtime("one", "desired").unwrap();
        let before_state = core.store.runtime("one", "account-state").unwrap().unwrap();
        let cmd = add_msg_command(ROOM, STUB_XML, 7010, 1700000110);
        core.apply_sync_command("one", &cmd, "r-bigxml-desired-none", "live")
            .unwrap();

        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 1);
        assert!(upstream.calls.load(Ordering::SeqCst) > 1);
        // Unchanged credentials/cookie must not churn account-state ciphertext.
        assert_eq!(
            core.store.runtime("one", "account-state").unwrap().unwrap(),
            before_state
        );
    }

    #[test]
    fn transient_failure_commits_unavailable_stub_and_later_retry_emits_updated() {
        let mut up = ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 200);
        up.fail_first = AtomicUsize::new(1);
        let upstream = Arc::new(up);
        let (_dir, core) = test_core(upstream.clone());
        let cmd = add_msg_command(ROOM, STUB_XML, 7002, 1700000200);
        core.apply_sync_command("one", &cmd, "r-bigxml-2", "live")
            .unwrap();

        // First attempt fails transiently: emits message.committed so UI/API aren't blocked.
        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 0);
        let events = core.store.events("one", 0, 50).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            1
        );

        // Clear backoff timestamp to simulate elapsed retry window.
        let key = job_key(ROOM, "7002");
        let sealed = core.store.runtime("one", &key).unwrap().unwrap();
        let mut job: BigXmlJob =
            serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap()).unwrap();
        assert!(job.committed_emitted);
        job.not_before = 0;
        core.save_big_xml_job("one", &key, &job).unwrap();

        // Retry succeeds and emits message.updated.
        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 1);
        let events = core.store.events("one", 0, 50).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "message.updated")
                .count(),
            1
        );
        let msg = &core.messages("one", ROOM, Some(10), None).unwrap()["items"][0];
        assert_eq!(msg["appMessageType"], 19);
        assert!(msg["body"].as_str().unwrap().contains("Full message one"));
    }

    #[test]
    fn service_rejection_marks_failed_and_keeps_explicit_unavailable_body() {
        let mut up = ChunkedUpstream::new(b"", 64);
        up.reject = Some((Service::AppAttachment, -22));
        let upstream = Arc::new(up);
        let (_dir, core) = test_core(upstream.clone());
        let cmd = add_msg_command(ROOM, STUB_XML, 7003, 1700000300);
        core.apply_sync_command("one", &cmd, "r-bigxml-3", "live")
            .unwrap();

        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 0);
        assert!(core.store.big_xml_jobs("one").unwrap().is_empty());
        let events = core.store.events("one", 0, 50).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            1
        );
        let msg = &core.messages("one", ROOM, Some(10), None).unwrap()["items"][0];
        assert_eq!(msg["appMessageType"], 40);
        assert_eq!(
            msg["body"],
            "[Chat History] Chat History for Team\nAlice: preview excerpt\n[Chat History unavailable]"
        );
        let source = crate::profile::source_message_id(msg["id"].as_str().unwrap()).unwrap();
        let stored = core
            .profile_reader("one")
            .unwrap()
            .message(&source, Some("account"))
            .unwrap();
        assert_eq!(
            stored.raw_fields.get("download_status"),
            Some(&wechat_profile::SqlValue::Integer("3".into()))
        );
    }

    #[tokio::test]
    async fn session_rejection_propagates_to_logout_without_marking_download_failed() {
        for service in [Service::AppAttachment, Service::NativeEnvelope] {
            let mut up = ChunkedUpstream::new(b"", 64);
            up.reject = Some((service, -13));
            let upstream = Arc::new(up);
            let (_dir, core) = test_core(upstream);
            let cmd = add_msg_command(ROOM, STUB_XML, 7020, 1700000350);
            core.apply_sync_command("one", &cmd, "r-bigxml-auth", "live")
                .unwrap();

            let err = core
                .session_request("one", "polling", |_| async {
                    core.refresh_big_xml("one", true).map(|_| ())
                })
                .await
                .unwrap_err();
            assert_eq!(err.to_string(), "auth_required");
            assert_eq!(core.account_status("one").unwrap()["authenticated"], false);
            // Message was committed with unavailable stub and download_status was NOT marked 3.
            let msg = &core.messages("one", ROOM, Some(10), None).unwrap()["items"][0];
            assert_eq!(msg["appMessageType"], 40);
            let source = crate::profile::source_message_id(msg["id"].as_str().unwrap()).unwrap();
            let stored = core
                .profile_reader("one")
                .unwrap()
                .message(&source, Some("account"))
                .unwrap();
            assert_ne!(
                stored.raw_fields.get("download_status"),
                Some(&wechat_profile::SqlValue::Integer("3".into()))
            );
            let events = core.store.events("one", 0, 50).unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e.kind == "message.committed")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn recall_before_expansion_cancels_job_without_overwriting_recall() {
        let upstream = Arc::new(ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 200));
        let (_dir, core) = test_core(upstream.clone());
        let cmd = add_msg_command(ROOM, STUB_XML, 7004, 1700000400);
        core.apply_sync_command("one", &cmd, "r-bigxml-4", "live")
            .unwrap();
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "recall-4").unwrap();
        wechat_profile::apply_recall_projection(
            &location,
            &keys,
            ROOM,
            "7004",
            Some(1700000410),
            &options,
        )
        .unwrap();

        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 0);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);
        assert!(core.store.big_xml_jobs("one").unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn network_fetch_releases_account_lock_and_respects_concurrent_recall() {
        let (began_tx, began_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        struct BlockingUpstream {
            began: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl crate::upstream::Upstream for BlockingUpstream {
            fn kind(&self) -> crate::upstream::UpstreamKind {
                crate::upstream::UpstreamKind::Native
            }
            fn download_app_attach(
                &self,
                _state: &mut AccountState,
                request: &AppAttachRequest,
            ) -> Result<AppAttachChunk> {
                if let Some(tx) = self.began.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                let bytes = FULL_APPMSG.as_bytes().to_vec();
                Ok(AppAttachChunk {
                    start_pos: request.start_pos,
                    next_pos: bytes.len() as u32,
                    total_length: bytes.len() as u32,
                    complete: true,
                    bytes,
                })
            }
        }
        let upstream = Arc::new(BlockingUpstream {
            began: std::sync::Mutex::new(Some(began_tx)),
            resume: std::sync::Mutex::new(resume_rx),
        });
        let (_dir, core) = test_core(upstream);
        let cmd = add_msg_command(ROOM, STUB_XML, 7030, 1700000450);
        core.apply_sync_command("one", &cmd, "r-bigxml-lock", "live")
            .unwrap();

        let guard = core.lock_account("one").await.unwrap();
        let fence = core.session_fence("one").unwrap();
        let worker = core.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut permit = crate::account_network::AccountNetworkPermit {
                guard: Some(guard),
                runtime: tokio::runtime::Handle::current(),
            };
            worker.refresh_big_xml_with_permit(
                "one",
                true,
                Some(&mut permit),
                Some(&fence),
                Some(b"connected"),
            )
        });

        began_rx.await.unwrap();
        // Account lock must be released during blocking CGI 221 I/O.
        let concurrent_guard =
            tokio::time::timeout(std::time::Duration::from_secs(1), core.lock_account("one"))
                .await
                .unwrap()
                .unwrap();
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "recall-concurrent").unwrap();
        wechat_profile::apply_recall_projection(
            &location,
            &keys,
            ROOM,
            "7030",
            Some(1700000460),
            &options,
        )
        .unwrap();
        drop(concurrent_guard);

        resume_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap().unwrap(), 0);
        assert!(core.store.big_xml_jobs("one").unwrap().is_empty());
        let msg = &core.messages("one", ROOM, Some(10), None).unwrap()["items"][0];
        assert_eq!(msg["type"], 10000);
    }

    #[test]
    fn backoff_jobs_do_not_starve_local_repair_or_new_arrivals() {
        let upstream = Arc::new(ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 4096));
        let (_dir, core) = test_core(upstream.clone());
        let far_future = chrono::Utc::now().timestamp() + 3600;
        // Queue 10 jobs and put them in retry backoff.
        for i in 0..10u64 {
            let sid = 7100 + i;
            let cmd = add_msg_command(ROOM, STUB_XML, sid, 1700001000 + i);
            core.apply_sync_command("one", &cmd, &format!("r-backoff-{i}"), "live")
                .unwrap();
            let key = job_key(ROOM, &sid.to_string());
            let sealed = core.store.runtime("one", &key).unwrap().unwrap();
            let mut job: BigXmlJob =
                serde_json::from_slice(&core.vault.open_record("one", &key, &sealed).unwrap())
                    .unwrap();
            job.attempts = 2;
            job.not_before = far_future;
            core.save_big_xml_job("one", &key, &job).unwrap();
        }
        // 11th job already has persisted verified bytes (crash recovery) and survives logout epoch change.
        let repair_cmd = add_msg_command(ROOM, STUB_XML, 7198, 1700001098);
        core.apply_sync_command("one", &repair_cmd, "r-repair", "live")
            .unwrap();
        let repair_key = job_key(ROOM, "7198");
        let sealed = core.store.runtime("one", &repair_key).unwrap().unwrap();
        let mut repair_job: BigXmlJob =
            serde_json::from_slice(&core.vault.open_record("one", &repair_key, &sealed).unwrap())
                .unwrap();
        repair_job.expanded_raw = Some(FULL_APPMSG.as_bytes().to_vec());
        repair_job.epoch = Some(b"older-epoch".to_vec());
        core.save_big_xml_job("one", &repair_key, &repair_job)
            .unwrap();

        // 12th job is a fresh arrival ready to fetch immediately.
        let fresh_cmd = add_msg_command(ROOM, STUB_XML, 7199, 1700001099);
        core.apply_sync_command("one", &fresh_cmd, "r-fresh", "live")
            .unwrap();

        // Local repair (allow_fetch = false) settles job 7198 with zero network calls despite 10 backoff jobs.
        assert_eq!(core.refresh_big_xml("one", false).unwrap(), 1);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);
        assert!(core.store.runtime("one", &repair_key).unwrap().is_none());

        // Online pass fetches and expands job 7199 despite 10 backoff jobs ahead of it.
        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 1);
        assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
        let fresh_key = job_key(ROOM, "7199");
        assert!(core.store.runtime("one", &fresh_key).unwrap().is_none());
    }

    #[test]
    fn xml_edge_cases_outgoing_group_empty_recorditem_and_reader_bounds() {
        // Empty <recorditem/> must NOT count as an inline record and must still queue expansion.
        let empty_record_stub = "<msg><appmsg><title>Empty Record</title><type>40</type><realinnertype>19</realinnertype><recorditem/></appmsg></msg>";
        let parsed = parse_stub(empty_record_stub).unwrap();
        assert!(!parsed.has_inline_record);

        // Leading XML declaration + comment + <msg\n...> must not double-wrap.
        let multiline_msg = format!(
            "<?xml version=\"1.0\"?>\n<!-- notice -->\n<msg\n  attr=\"1\">{}</msg>",
            FULL_APPMSG.split_once('\n').unwrap().1
        );
        let (subtype, stored, title) =
            assemble_expanded_xml(multiline_msg.as_bytes(), ROOM, "account", true, Some(19))
                .unwrap();
        assert_eq!(subtype, 19);
        assert_eq!(title, "Chat History for Team");
        // Outgoing group message must NOT carry `<sender>:\n` prefix.
        assert!(stored.starts_with("<msg\n"));
        assert!(!stored.starts_with("account:\n"));

        // Non-19 payload exceeding quotes::app_message's 256 KiB bound is rejected rather than producing body: null.
        let oversized_non19 = format!(
            "<msg><appmsg><title>{}</title><type>5</type><url>https://example.invalid</url></appmsg></msg>",
            "a".repeat(300 * 1024)
        );
        assert!(assemble_expanded_xml(
            oversized_non19.as_bytes(),
            "wxid_peer",
            "wxid_peer",
            false,
            Some(5)
        )
        .is_err());

        // Outgoing group 49:40 expands end-to-end without a sender prefix.
        let upstream = Arc::new(ChunkedUpstream::new(FULL_APPMSG.as_bytes(), 512));
        let (_dir, core) = test_core(upstream);
        let cmd = add_msg_command_dir("account", ROOM, empty_record_stub, 7200, 1700002000);
        core.apply_sync_command("one", &cmd, "r-outgoing-group", "live")
            .unwrap();
        assert_eq!(core.refresh_big_xml("one", true).unwrap(), 1);
        let msg = &core.messages("one", ROOM, Some(10), None).unwrap()["items"][0];
        assert_eq!(msg["appMessageType"], 19);
        assert_eq!(msg["direction"], "outgoing");
        assert!(msg["body"].as_str().unwrap().contains("Full message one"));
    }

    #[test]
    fn stored_zero_subtype_40_over_256k_reports_app_message_type_40() {
        let upstream = Arc::new(ChunkedUpstream::new(b"", 64));
        let (_dir, core) = test_core(upstream);
        let pad = "x".repeat(270 * 1024);
        let large_stub = format!(
            "<msg><appmsg><title>Large Stub</title><des>Preview</des><type>40</type><realinnertype>19</realinnertype><extra>{pad}</extra></appmsg></msg>"
        );
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "legacy-zero").unwrap();
        let mut fields = wechat_profile::RawFields::new();
        for (k, v) in [
            ("local_type", 49i64),
            ("sort_seq", 1700003000000),
            ("create_time", 1700003000),
            ("status", 3),
            ("upload_status", 0),
            ("download_status", 0),
            ("server_seq", 10),
            ("origin_source", 2),
            ("WCDB_CT_message_content", 4),
            ("WCDB_CT_source", 4),
        ] {
            fields.insert(k.into(), wechat_profile::SqlValue::Integer(v.to_string()));
        }
        fields.insert(
            "message_content".into(),
            wechat_profile::compressed_profile_field(large_stub.as_bytes()).unwrap(),
        );
        fields.insert(
            "source".into(),
            wechat_profile::compressed_profile_field(b"<msgsource/>").unwrap(),
        );
        fields.insert(
            "compress_content".into(),
            wechat_profile::SqlValue::Text(String::new()),
        );
        wechat_profile::apply_message_projection(
            &location,
            &keys,
            &wechat_profile::MessageProjection {
                peer: "wxid_legacy".into(),
                sender: "wxid_legacy".into(),
                from: "wxid_legacy".into(),
                to: "account".into(),
                server_message_id: "7201".into(),
                fields,
                packed_other_fields: vec![88, 0],
            },
            &options,
        )
        .unwrap();

        let msg = &core.messages("one", "wxid_legacy", Some(10), None).unwrap()["items"][0];
        assert_eq!(msg["type"], 49);
        assert_eq!(msg["appMessageType"], 40);
        assert_eq!(
            msg["body"],
            "[Chat History] Large Stub\nPreview\n[Chat History unavailable]"
        );
    }
}
