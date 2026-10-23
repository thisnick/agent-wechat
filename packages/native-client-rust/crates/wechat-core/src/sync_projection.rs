//! Projection of sealed native sync commands into exclusively owned local files.
//! Original protocol bytes remain in the encrypted apply journal. Ordinary row
//! policy is deliberately separate from an exhaustive native status enum.
use super::*;
use anyhow::ensure;
use wechat_profile::{
    ContactProjection, MessageProjection, MessageSessionProjection, RawFields, SqlValue,
};
use wechat_protocol::wire::{AddMessage, SyncCommand};

/// A safe, typed obligation. The sealed batch retains the original command;
/// reporting this value never constitutes applying its native side effects.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectionBlocked {
    command_id: i32,
    message_type: Option<i32>,
    classification: &'static str,
    operation_ids: Vec<u32>,
    system_kind: Option<&'static str>,
    reason: &'static str,
}
impl std::fmt::Display for ProjectionBlocked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason)
    }
}
impl std::error::Error for ProjectionBlocked {}
pub(crate) fn projection_blocker(error: &anyhow::Error) -> Option<Value> {
    error
        .downcast_ref::<ProjectionBlocked>()
        .and_then(|value| serde_json::to_value(value).ok())
}
fn command_blocker(command: &SyncCommand) -> ProjectionBlocked {
    // Linux 4.1.13.26 dispatch at 0x6226f90 (table 0x1f201bc) calls:
    // 1 -> 0x6227930 -> ModUserInfo constructor 0x7ca80a0;
    // 4 -> 0x6228370 -> DelContact constructor 0x7bfb450;
    // 35 -> 0x62280d0 -> ModUserImg constructor 0x7cac120;
    // 44 -> 0x6227d50 -> UserInfoExt constructor 0x7c9b410.
    // These generated consumers establish types; complete SQL maps remain open.
    let classification = match command.id {
        1 => "mod_user_info",
        4 => "delete_contact",
        35 => "mod_user_image",
        44 => "user_info_ext",
        _ => "unknown_native_command",
    };
    ProjectionBlocked {
        command_id: command.id,
        message_type: None,
        classification,
        operation_ids: vec![],
        system_kind: None,
        reason: "unsupported_sync_control",
    }
}
fn message_blocker(message: &AddMessage) -> ProjectionBlocked {
    let mut result = ProjectionBlocked {
        command_id: 5,
        message_type: Some(message.message_type),
        classification: match message.message_type {
            51 => "status_notification",
            10002 => "system_control",
            _ => "unsupported_message_variant",
        },
        operation_ids: vec![],
        system_kind: None,
        reason: "unsupported_message_projection_type",
    };
    if !matches!(message.message_type, 51 | 10002) {
        return result;
    }
    result.reason = "unsupported_sync_control";
    let parse = || -> Result<(Vec<u32>, Option<&'static str>)> {
        let content = std::str::from_utf8(&message.content)?;
        ensure!(
            content.len() <= 1024 * 1024
                && !content.to_ascii_uppercase().contains("<!DOCTYPE")
                && !content.to_ascii_uppercase().contains("<!ENTITY"),
            "invalid_control_xml"
        );
        let doc = roxmltree::Document::parse(content)?;
        ensure!(doc.descendants().count() <= 4096, "invalid_control_xml");
        let root = doc.root_element();
        if message.message_type == 51 {
            ensure!(
                root.tag_name().name() == "msg" && root.tag_name().namespace().is_none(),
                "invalid_control_xml"
            );
            let mut ids = vec![];
            for op in root.children().filter(|node| node.is_element()) {
                ensure!(
                    op.tag_name().name() == "op" && op.tag_name().namespace().is_none(),
                    "invalid_control_xml"
                );
                let raw = op.attribute("id").context("invalid_control_xml")?;
                ensure!(
                    !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()),
                    "invalid_control_xml"
                );
                ids.push(raw.parse()?);
                ensure!(ids.len() <= 64, "invalid_control_xml");
            }
            ensure!(!ids.is_empty(), "invalid_control_xml");
            Ok((ids, None))
        } else {
            // Only known discriminator labels may enter an event. Arbitrary XML
            // names/attributes can contain private data and stay sealed.
            let kind = if root.tag_name().name() == "sysmsg"
                && root.tag_name().namespace().is_none()
            {
                match root.attribute("type") {
                    Some("functionmsg") => Some("functionmsg"),
                    Some("dynacfg") => Some("dynacfg"),
                    Some("WeChatOut") => Some("WeChatOut"),
                    Some("ClientCheckGetExtInfo") => Some("ClientCheckGetExtInfo"),
                    Some("revokemsg") => Some("revokemsg"),
                    _ => Some("unknown"),
                }
            } else if root.tag_name().name() == "appmsg" && root.tag_name().namespace().is_none() {
                Some("appmsg")
            } else {
                Some("unknown")
            };
            Ok((vec![], kind))
        }
    };
    match parse() {
        Ok((ids, kind)) => {
            result.operation_ids = ids;
            result.system_kind = kind;
        }
        Err(_) => result.reason = "invalid_sync_control_metadata",
    }
    result
}

fn deleted_contact_username(payload: &[u8]) -> Result<String> {
    use wechat_protocol::protobuf as p;
    let fields = p::decode(payload)?;
    let nested = p::decode(p::data(&fields, 1)?)?;
    let username = std::str::from_utf8(p::data(&nested, 1)?)?.to_string();
    identity(&username)?;
    for number in [2, 3] {
        if let Some(value) = p::optional(&fields, number)? {
            let p::Value::Varint(value) = value else {
                bail!("invalid_delete_contact_metadata")
            };
            u32::try_from(*value)?;
        }
    }
    Ok(username)
}
fn contact_absent(reader: &wechat_profile::ProfileReader, username: &str) -> Result<bool> {
    let mut query = wechat_profile::ListQuery {
        search: Some(username.into()),
        limit: Some(200),
        ..Default::default()
    };
    // The official contacts reader queries every Contact row, including groups
    // and nonfriend categories. Absence is established only after a complete
    // exact-name search over one coherent snapshot, never from a friend list.
    for _ in 0..100 {
        let page = reader.contacts(&query)?;
        if page
            .items
            .iter()
            .any(|contact| contact.username == username)
        {
            return Ok(false);
        }
        match page.next_cursor {
            Some(cursor) => query.cursor = Some(cursor),
            None => return Ok(true),
        }
    }
    bail!("contact_absence_scan_bound")
}

fn identity(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        bail!("invalid_message_identity")
    }
    Ok(())
}
fn envelope(message: &AddMessage) -> Result<(String, &[u8])> {
    if message.from.ends_with("@chatroom") && !message.outgoing && message.message_type != 10000 {
        let end = message
            .content
            .windows(2)
            .take(513)
            .position(|p| p == b":\n")
            .context("unsupported_group_sender_envelope")?;
        let sender = std::str::from_utf8(&message.content[..end])?;
        identity(sender)?;
        if sender.ends_with("@chatroom") {
            bail!("invalid_group_sender_envelope")
        }
        Ok((sender.into(), &message.content[end + 2..]))
    } else {
        Ok((
            message
                .sender_id
                .clone()
                .unwrap_or_else(|| message.from.clone()),
            &message.content,
        ))
    }
}
fn app_fields(content: &[u8]) -> Result<(u32, Option<String>)> {
    let text = std::str::from_utf8(content)?;
    if text.len() > 4 * 1024 * 1024
        || text.to_ascii_uppercase().contains("<!DOCTYPE")
        || text.to_ascii_uppercase().contains("<!ENTITY")
    {
        bail!("invalid_app_message_metadata")
    }
    let doc = roxmltree::Document::parse(text)?;
    let root = doc.root_element();
    let app = if root.tag_name().name() == "appmsg" {
        root
    } else {
        let mut apps = root
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "appmsg");
        let app = apps.next().context("missing_app_message_metadata")?;
        if apps.next().is_some() {
            bail!("ambiguous_app_message_metadata")
        }
        app
    };
    let mut types = app
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "type");
    let subtype = types
        .next()
        .and_then(|n| n.text())
        .context("missing_app_message_type")?
        .parse::<u32>()?;
    if types.next().is_some() || subtype > i32::MAX as u32 {
        bail!("invalid_app_message_type")
    }
    let mut titles = app
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "title");
    let title = titles.next().and_then(|n| n.text()).map(str::to_owned);
    if titles.next().is_some() {
        bail!("ambiguous_app_message_title")
    }
    Ok((subtype, title))
}
fn fields(message: &AddMessage, body: &[u8], sort: i64) -> Result<(RawFields, String)> {
    if message.content.len() > 4 * 1024 * 1024 {
        bail!("message_content_too_large")
    }
    if ![1, 3, 34, 43, 47, 49, 10000].contains(&message.message_type) {
        bail!("unsupported_message_projection_type")
    }
    let (subtype, title) = if message.message_type == 49 {
        app_fields(body)?
    } else {
        (0, None)
    };
    let local_type = (u64::from(subtype) << 32) | message.message_type as u64;
    let mut fields = RawFields::new();
    for (key, value) in [
        ("local_type", local_type as i64),
        ("sort_seq", sort),
        ("create_time", message.created_at as i64),
        ("status", if message.message_type == 10000 { 4 } else { 3 }),
        ("upload_status", 0),
        (
            "download_status",
            if message.message_type == 34 && !message.inline_data.is_empty() {
                1
            } else {
                0
            },
        ),
        (
            "server_seq",
            message
                .server_sequence
                .context("missing_verified_message_sequence")? as i64,
        ),
        ("origin_source", 2),
        (
            "WCDB_CT_message_content",
            if message.message_type == 1 || message.message_type >= 10000 {
                0
            } else {
                4
            },
        ),
        ("WCDB_CT_source", 4),
    ] {
        fields.insert(key.into(), SqlValue::Integer(value.to_string()));
    }
    // Uncached media starts pending (download_status0); caching later has its own
    // exact resource transition. These rows are not represented as downloaded.
    fields.insert(
        "message_content".into(),
        if message.message_type == 1 || message.message_type >= 10000 {
            SqlValue::Text(std::str::from_utf8(&message.content)?.to_string())
        } else {
            wechat_profile::compressed_profile_field(&message.content)?
        },
    );
    fields.insert(
        "source".into(),
        wechat_profile::compressed_profile_field(message.msgsource.as_deref().unwrap_or_default())?,
    );
    fields.insert("compress_content".into(), SqlValue::Text(String::new()));
    let summary = match message.message_type {
        1 | 10000 => std::str::from_utf8(body)?.to_string(),
        49 => title.unwrap_or_default(),
        _ => String::new(),
    };
    Ok((fields, summary))
}
impl Core {
    fn retain_control_raw(
        &self,
        account: &str,
        command: &SyncCommand,
        receipt: &str,
    ) -> Result<()> {
        let key = format!("sync-control:{receipt}");
        if let Some(sealed) = self.store.runtime(account, &key)? {
            ensure!(
                *self.vault.open_record(account, &key, &sealed)? == command.payload,
                "sync_control_receipt_conflict"
            );
        } else {
            self.store.put_runtime(
                account,
                &key,
                &self.vault.seal(account, &key, &command.payload)?,
            )?;
        }
        Ok(())
    }
    fn retain_ignored_sync_control(
        &self,
        account: &str,
        command: &SyncCommand,
        receipt: &str,
        origin: &str,
    ) -> Result<()> {
        ensure!(command.id == 23, "unknown_sync_control_cannot_be_ignored");
        self.retain_control_raw(account, command, receipt)?;
        self.store.append_event_once(account, &format!("control-observed:{receipt}"), "account.control_observed", &json!({"commandId":23,"classification":"native_dispatcher_ignored","projection":"not_applicable_current_build","policy":"linux_4_1_13_26","origin":origin}))?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(())
    }
    pub(crate) fn apply_sync_command(
        &self,
        account: &str,
        command: &SyncCommand,
        receipt: &str,
        origin: &str,
    ) -> Result<()> {
        self.apply_sync_command_with_media(account, command, receipt, origin, false)
    }
    pub(crate) fn apply_received_sync_command(
        &self,
        account: &str,
        command: &SyncCommand,
        receipt: &str,
        origin: &str,
    ) -> Result<()> {
        self.apply_sync_command_with_media(account, command, receipt, origin, true)
    }
    fn apply_sync_command_with_media(
        &self,
        account: &str,
        command: &SyncCommand,
        receipt: &str,
        origin: &str,
        download_voice: bool,
    ) -> Result<()> {
        if command.id == 4 {
            // Native DelContact handler 0x6228370 calls 0x786f830. Lookup
            // 0x786e260 returning no Contact jumps to 0x786fe54 without mutation.
            // Existing-row transitions
            // involve category/flag/group state and remain blocked here.
            self.account_state(account)?;
            let username = deleted_contact_username(&command.payload)?;
            if !contact_absent(&self.profile_reader(account)?, &username)? {
                let mut blocked = command_blocker(command);
                blocked.reason = "unsupported_existing_contact_deletion";
                return Err(blocked.into());
            }
            self.retain_control_raw(account, command, receipt)?;
            self.store.append_event_once(account, &format!("control-observed:{receipt}"), "account.control_observed", &json!({"commandId":4,"classification":"delete_contact_absent","projection":"no_change","policy":"linux_4_1_13_26","origin":origin}))?;
            self.store.put_runtime(account, receipt, b"1")?;
            return Ok(());
        }
        if command.id == 23 {
            // The pinned NewSync dispatcher 0x61e0180 (mask 0x100800000016)
            // and metadata dispatcher 0x622719d log command 23 and continue
            // without parsing or forwarding it.
            // Retain it explicitly; this is not a general unknown-command rule.
            self.account_state(account)?;
            self.retain_ignored_sync_control(account, command, receipt, origin)?;
            return Ok(());
        }
        if !matches!(command.id, 2 | 5) {
            return Err(command_blocker(command).into());
        }
        let state = self.account_state(account)?;
        let parsed = if command.id == 5 {
            let message =
                wechat_protocol::wire::decode_add_message(&command.payload, &state.own_username)?;
            if let Some(recall) = crate::recalls::incoming_recall(&message, &state.own_username)? {
                self.retain_control_raw(account, command, receipt)?;
                self.apply_confirmed_recall_with_notice(
                    account,
                    &recall.thread,
                    &recall.server,
                    recall.recalled_at,
                    recall.replacement_text.as_deref(),
                    origin,
                    receipt,
                )?;
                self.store.put_runtime(account, receipt, b"1")?;
                return Ok(());
            }
            if let Some(read) =
                super::read_control::parse_remote_thread_read(&message, &state.own_username)
            {
                let options = self.write_options(account, receipt)?;
                let (location, keys) = self.profile_location_keys(account)?;
                self.retain_control_raw(account, command, receipt)?;
                let projection = wechat_profile::mark_read_thread(
                    &location,
                    &keys,
                    &read.thread,
                    read.through_time,
                    &options,
                )?;
                self.store.append_event_once(
                    account,
                    &format!("remote-thread-read:{receipt}"),
                    "thread.read-state",
                    &json!({"threadId":read.thread,"readScope":"thread","origin":"remote_read","throughTime":read.through_time,"projection":projection}),
                )?;
                self.store.put_runtime(account, receipt, b"1")?;
                return Ok(());
            }
            if let Some(read) =
                super::read_control::parse_remote_thread_entry_read(&message, &state.own_username)
            {
                self.retain_control_raw(account, command, receipt)?;
                self.apply_partial_thread_read(
                    account,
                    &read,
                    receipt,
                    super::read_control::PartialThreadRead::Entry,
                )?;
                // Entry also prepares sessions and notifies observers. Keep
                // the full control blocked after its once-only unread effect.
            }
            if let Some(read) =
                super::read_control::parse_remote_thread_exit_read(&message, &state.own_username)
            {
                self.retain_control_raw(account, command, receipt)?;
                self.apply_partial_thread_read(
                    account,
                    &read,
                    receipt,
                    super::read_control::PartialThreadRead::Exit,
                )?;
                // Session exit has additional observer and auto-dismiss
                // behavior. Retain the full command as blocked below, even
                // after its independently journaled unread component is done.
            }
            if let Some(control) =
                super::status_control::parse_no_local_effect_status(&message, &state.own_username)
            {
                self.retain_control_raw(account, command, receipt)?;
                self.store.append_event_once(
                    account,
                    &format!("control-no-local-effect:{receipt}"),
                    "account.control_observed",
                    &json!({"commandId":5,"messageType":51,"operationIds":[control.operation_id()],"classification":control.classification(),"projection":"no_local_effect","policy":"linux_4_1_13_26","origin":origin}),
                )?;
                self.store.put_runtime(account, receipt, b"1")?;
                return Ok(());
            }
            if let Some(control) =
                super::system_control::parse_no_local_effect_system(&message, &state.own_username)
            {
                self.retain_control_raw(account, command, receipt)?;
                self.store.append_event_once(
                    account,
                    &format!("control-no-local-effect:{receipt}"),
                    "account.control_observed",
                    &json!({"commandId":5,"messageType":10002,"classification":control.classification(),"projection":"no_local_effect","policy":"linux_4_1_13_26","origin":origin}),
                )?;
                self.store.put_runtime(account, receipt, b"1")?;
                return Ok(());
            }
            if ![1, 3, 34, 43, 47, 49, 10000].contains(&message.message_type) {
                let blocked = message_blocker(&message);
                if matches!(message.message_type, 51 | 10002) {
                    self.store.append_event_once(
                        account,
                        &format!("control-observed:{receipt}"),
                        "account.control_observed",
                        &json!({"control":blocked,"origin":origin,"projection":"blocked"}),
                    )?;
                }
                return Err(blocked.into());
            }
            Some(message)
        } else {
            None
        };
        let options = self.write_options(account, receipt)?;
        let (location, keys) = self.profile_location_keys(account)?;
        if command.id == 2 {
            let contact = wechat_protocol::wire::decode_contact_record(&command.payload)?;
            let fields_applied = format!("contact-fields:{receipt}");
            let controlled = wechat_protocol::controlled_contact_fields(&contact.raw)?;
            let wire = wechat_protocol::protobuf::decode(&contact.raw)?;
            let partial_mask = match wechat_protocol::protobuf::optional(&wire, 7)? {
                Some(wechat_protocol::protobuf::Value::Varint(n)) => *n != u64::from(u32::MAX),
                None => false,
                _ => true,
            };
            if self.store.runtime(account, &fields_applied)?.is_none() {
                wechat_profile::apply_contact_patch(
                    &location,
                    &keys,
                    &self.upstream.contact_patch(&contact)?,
                    &options,
                )?;
                if contact.username.ends_with("@chatroom") {
                    // Explicit native contact metadata establishes catalog presence.
                    // Missing owner/roster fields remain NULL/preserved; a group seen
                    // only in an AddMsg does not create this catalog row.
                    wechat_profile::apply_group_patch(
                        &location,
                        &keys,
                        &wechat_profile::GroupProjection {
                            username: contact.username.clone(),
                            fields: RawFields::new(),
                            detail_fields: None,
                            members: vec![],
                            complete_roster: false,
                        },
                        &options,
                    )?;
                }
                self.store.append_event_once(
                    account,
                    &format!("contact:{receipt}"),
                    "contacts.updated",
                    &json!({"contactId":contact.username,"origin":origin}),
                )?;
                if contact.username.ends_with("@chatroom") {
                    self.mark_group_revision(account, &contact.username, receipt)?;
                    self.store.append_event_once(account,&format!("group-fields:{receipt}"),"group.updated",&json!({"groupId":contact.username,"origin":origin,"source":"remote_sync","profileProjection":"partial"}))?;
                }
                self.store.put_runtime(account, &fields_applied, b"1")?;
            }
            let pending = contact.username.ends_with("@chatroom")
                && self.queue_incoming_group(account, &contact, receipt, origin)?;
            if pending || !controlled.is_empty() || partial_mask {
                let mut blocked = command_blocker(command);
                blocked.classification = "contact_update";
                blocked.reason = if pending {
                    "group_metadata_refresh_pending"
                } else {
                    "contact_update_control_unresolved"
                };
                return Err(blocked.into());
            }
            self.store.put_runtime(account, receipt, b"1")?;
            return Ok(());
        }
        let message = parsed.context("missing_parsed_message")?;
        let (sender, body) = envelope(&message)?;
        identity(&sender)?;
        let sid = message
            .server_message_id
            .as_deref()
            .context("missing_message_server_identity")?;
        let recall = self.store.recall_marker(account, &message.peer, sid)?;
        if let Some(marker) = recall.as_ref() {
            if wechat_profile::find_projected_message(
                &location,
                &keys,
                &message.peer,
                sid,
                &options,
            )?
            .is_some()
            {
                self.project_recall_marker(account, marker, receipt)?;
                self.store.put_runtime(account, receipt, b"1")?;
                return Ok(());
            }
        }
        let planned_sort = wechat_profile::choose_message_sort_sequence(
            &location,
            &keys,
            &message.peer,
            sid,
            message.created_at as i64,
            &options,
        )?;
        let (stored_fields, summary) = fields(&message, body, planned_sort)?;
        // Minimal cached identities do not manufacture friendship flags, owner,
        // nickname, membership or a contact/group catalog entry from remote names.
        for name in [&message.peer, &sender, &message.from, &message.to] {
            identity(name)?;
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &ContactProjection {
                    username: name.clone(),
                    fields: RawFields::new(),
                },
                &options,
            )?;
        }
        let existing =
            wechat_profile::find_projected_message(&location, &keys, &message.peer, sid, &options)?;
        let own_phase = format!("{receipt}:message-started");
        let started = self.store.runtime(account, &own_phase)?.is_some();
        let (source, created_by_intent) = if let Some(id) = existing {
            let stored = self
                .profile_reader(account)?
                .message(&id, Some(&state.own_username))?;
            let expected = std::str::from_utf8(&message.content)?;
            if stored.create_time != message.created_at as i64
                || stored.content.text.as_deref() != Some(expected)
                || stored.local_type
                    != match stored_fields.get("local_type") {
                        Some(SqlValue::Integer(n)) => n.clone(),
                        _ => bail!("invalid_stored_message_type"),
                    }
            {
                bail!("message_identity_conflict")
            }
            if stored.sender.as_deref().is_some_and(|s| s != sender) {
                bail!("message_sender_conflict")
            }
            (id, started)
        } else {
            // Persist before the first profile transaction. A retry can distinguish its
            // own interrupted row from an independently imported native observation.
            self.store.put_runtime(account, &own_phase, b"1")?;
            let applied = wechat_profile::apply_message_projection(
                &location,
                &keys,
                &MessageProjection {
                    peer: message.peer.clone(),
                    sender: sender.clone(),
                    from: message.from.clone(),
                    to: message.to.clone(),
                    server_message_id: sid.into(),
                    fields: stored_fields,
                    packed_other_fields: vec![88, 0],
                },
                &options,
            )?;
            (applied.source_id, true)
        };
        if let Some(marker) = recall.as_ref() {
            self.project_recall_marker(account, marker, receipt)?;
            wechat_profile::apply_message_session_projection(
                &location,
                &keys,
                &MessageSessionProjection {
                    source: source.clone(),
                    peer: message.peer.clone(),
                    summary: self.recall_preview(account, marker)?,
                    outgoing: message.outgoing,
                    unread: false,
                    sender: None,
                    sender_display: None,
                    created_by_this_intent: created_by_intent,
                },
                &options,
            )?;
            let id = crate::message_id(&source)?;
            self.store.append_event_once(account,&format!("message:{id}"),"message.committed",&json!({"messageId":id,"threadId":message.peer,"origin":origin,"direction":if message.outgoing{"outgoing"}else{"incoming"}}))?;
            self.store.put_runtime(account, receipt, b"1")?;
            return Ok(());
        }
        if message.message_type == 34 && !message.inline_data.is_empty() {
            wechat_profile::cache_voice_projection(
                &location,
                &keys,
                &source,
                &message.peer,
                &message.inline_data,
                &options,
            )?;
        }
        wechat_profile::apply_message_session_projection(
            &location,
            &keys,
            &MessageSessionProjection {
                source: source.clone(),
                peer: message.peer.clone(),
                summary,
                outgoing: message.outgoing,
                unread: origin != "profile_import",
                sender: Some(sender),
                sender_display: None,
                created_by_this_intent: created_by_intent,
            },
            &options,
        )?;
        let id = crate::message_id(&source)?;
        if download_voice && message.message_type == 34 && origin != "profile_import" {
            self.queue_received_voice(account, &id)?;
        }
        self.store.append_event_once(account,&format!("message:{id}"),"message.committed",&json!({"messageId":id,"threadId":message.peer,"origin":origin,"direction":if message.outgoing{"outgoing"}else{"incoming"}}))?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_delete_contact_only_proves_absence_in_complete_all_category_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("profile");
        let path = root.join("db_storage/contact/contact.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!(
            "../../wechat-profile/src/layout/contact.db.sql"
        ))
        .unwrap();
        db.execute_batch("INSERT INTO contact(id,username,local_type)VALUES(1,'peer-long',1),(2,'room@chatroom',2),(3,'other',4)").unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let reader = wechat_profile::ProfileReader::open(
            wechat_profile::ProfileLocation::new(root).unwrap(),
            wechat_profile::KeyFile::empty("profile"),
            wechat_profile::ReadOptions {
                allow_plaintext: true,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(contact_absent(&reader, "peer").unwrap());
        for name in ["peer-long", "room@chatroom", "other"] {
            assert!(!contact_absent(&reader, name).unwrap());
        }
        assert_eq!(std::fs::read(path).unwrap(), before);
        use wechat_protocol::protobuf as p;
        let payload = p::message(&[
            p::bytes(1, &p::bytes(1, b"synthetic")),
            p::uint(2, 0),
            p::uint(3, 1),
        ])
        .unwrap();
        assert_eq!(deleted_contact_username(&payload).unwrap(), "synthetic");
        assert!(deleted_contact_username(&p::bytes(1, b"not nested")).is_err());
    }
    #[test]
    fn control_ignored_policy_seals_raw_once_and_never_extends_to_other_commands() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let command = SyncCommand {
            id: 23,
            payload: b"synthetic private opaque control".to_vec(),
        };
        for _ in 0..2 {
            core.retain_ignored_sync_control("one", &command, "receipt", "catchup")
                .unwrap();
        }
        assert_eq!(
            store
                .events("one", 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "account.control_observed")
                .count(),
            1
        );
        let raw = store
            .runtime("one", "sync-control:receipt")
            .unwrap()
            .unwrap();
        assert!(!raw
            .windows(command.payload.len())
            .any(|v| v == command.payload));
        assert_eq!(
            *core
                .vault
                .open_record("one", "sync-control:receipt", &raw)
                .unwrap(),
            command.payload
        );
        assert!(core
            .retain_ignored_sync_control(
                "one",
                &SyncCommand {
                    id: 4,
                    payload: vec![]
                },
                "other",
                "catchup"
            )
            .is_err());
        assert!(core
            .retain_ignored_sync_control(
                "one",
                &SyncCommand {
                    id: 23,
                    payload: b"conflicting".to_vec()
                },
                "receipt",
                "catchup"
            )
            .is_err());
    }
    #[test]
    fn control_diagnostics_keep_unknown_commands_blocked_and_never_serialize_payloads() {
        for (id, expected) in [
            (1, "mod_user_info"),
            (4, "delete_contact"),
            (35, "mod_user_image"),
            (44, "user_info_ext"),
            (23, "unknown_native_command"),
            (999, "unknown_native_command"),
        ] {
            let blocked = command_blocker(&SyncCommand {
                id,
                payload: b"secret credential material".to_vec(),
            });
            let error = anyhow::Error::new(blocked).context("projection unavailable");
            let value = projection_blocker(&error).unwrap();
            assert_eq!(value["commandId"], id);
            assert_eq!(value["classification"], expected);
            assert!(!value.to_string().contains("secret"));
        }
    }
    #[test]
    fn control_diagnostics_type51_is_not_a_message_or_read_receipt() {
        let mut message = example();
        message.message_type = 51;
        message.content = b"<msg><op id=\"11\"><name>secret</name><arg>private</arg></op><op id=\"4\"><username>secret-user</username></op></msg>".to_vec();
        let blocked = message_blocker(&message);
        assert_eq!(blocked.operation_ids, vec![11, 4]);
        assert_eq!(blocked.classification, "status_notification");
        assert_eq!(blocked.reason, "unsupported_sync_control");
        let value = serde_json::to_value(blocked).unwrap().to_string();
        assert!(!value.contains("secret") && !value.contains("private"));
        message.content =
            b"<!DOCTYPE msg [<!ENTITY e SYSTEM 'file:///private'>]><msg><op id=\"1\"/></msg>"
                .to_vec();
        assert_eq!(
            message_blocker(&message).reason,
            "invalid_sync_control_metadata"
        );
    }
    #[test]
    fn control_diagnostics_system_configuration_and_revocation_remain_explicit() {
        let mut message = example();
        message.message_type = 10002;
        for (raw, kind) in [
            (
                "<sysmsg type=\"functionmsg\"><private>secret</private></sysmsg>",
                "functionmsg",
            ),
            ("<sysmsg type=\"revokemsg\"/>", "revokemsg"),
            ("<sysmsg type=\"private-secret\"/>", "unknown"),
        ] {
            message.content = raw.as_bytes().to_vec();
            let blocked = message_blocker(&message);
            assert_eq!(blocked.system_kind, Some(kind));
            assert_eq!(blocked.reason, "unsupported_sync_control");
            assert!(!serde_json::to_string(&blocked).unwrap().contains("secret"));
        }
    }
    fn example() -> AddMessage {
        AddMessage {
            from: "room@chatroom".into(),
            to: "self".into(),
            peer: "room@chatroom".into(),
            sender_id: None,
            outgoing: false,
            server_message_id: Some("9".into()),
            numbered1: 0,
            message_type: 49,
            content:
                b"member:\n<msg><appmsg><type>6</type><title>synthetic.txt</title></appmsg></msg>"
                    .to_vec(),
            content_text: None,
            numbered6: 0,
            numbered7: 0,
            inline_data: vec![],
            created_at: 100,
            msgsource: Some(b"<msgsource/>".to_vec()),
            opaque11: None,
            numbered13: Some(7),
            server_sequence: Some(7),
            raw: vec![],
        }
    }
    #[test]
    fn group_envelope_and_app_subtype_are_mapped_without_dropping_stored_prefix() {
        let message = example();
        let (sender, body) = envelope(&message).unwrap();
        assert_eq!(sender, "member");
        let (fields, summary) = fields(&message, body, 100000).unwrap();
        assert_eq!(
            fields.get("local_type"),
            Some(&SqlValue::Integer(((6u64 << 32) | 49).to_string()))
        );
        assert_eq!(summary, "synthetic.txt");
        assert_eq!(
            fields.get("download_status"),
            Some(&SqlValue::Integer("0".into()))
        );
    }
    #[test]
    fn malformed_group_or_app_metadata_and_unmapped_sequence_fail() {
        let mut message = example();
        message.content = b"<msg/>".to_vec();
        assert!(envelope(&message).is_err());
        message.content =
            b"member:\n<msg><appmsg><type>6</type><type>5</type></appmsg></msg>".to_vec();
        let (_, body) = envelope(&message).unwrap();
        assert!(fields(&message, body, 100000).is_err());
        let mut message = example();
        message.server_sequence = None;
        let (_, body) = envelope(&message).unwrap();
        assert!(fields(&message, body, 100000).is_err());
    }
}

impl Core {
    /// Local post-ack text insertion. The observed native outgoing row has a
    /// nonzero accepted server ID with server_seq0/status2/origin1; it does not wait
    /// for an absent NewSync echo or manufacture a network field13 sequence.
    pub(crate) fn project_accepted_text(
        &self,
        account: &str,
        destination: &str,
        text: &str,
        created_at: u64,
        server_message_id: &str,
        receipt: &str,
    ) -> Result<wechat_profile::SourceMessageId> {
        identity(destination)?;
        if text.is_empty() || text.len() > 64 * 1024 {
            bail!("invalid_accepted_text")
        }
        let options = self.write_options(account, receipt)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let state = self.account_state(account)?;
        let time = i64::try_from(created_at)?;
        for name in [&state.own_username, destination] {
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &ContactProjection {
                    username: name.to_string(),
                    fields: RawFields::new(),
                },
                &options,
            )?;
        }
        let source = if let Some(source) = wechat_profile::find_projected_message(
            &location,
            &keys,
            destination,
            server_message_id,
            &options,
        )? {
            let existing = self
                .profile_reader(account)?
                .message(&source, Some(&state.own_username))?;
            if self
                .store
                .recall_marker(account, destination, server_message_id)?
                .is_none()
                && (existing.message_type != 1
                    || existing.sender.as_deref() != Some(&state.own_username)
                    || existing.content.text.as_deref() != Some(text))
            {
                bail!("accepted_message_identity_conflict")
            }
            source
        } else {
            let sort = wechat_profile::choose_message_sort_sequence(
                &location,
                &keys,
                destination,
                server_message_id,
                time,
                &options,
            )?;
            let mut fields = RawFields::new();
            for (k, n) in [
                ("local_type", 1),
                ("sort_seq", sort),
                ("create_time", time),
                ("status", 2),
                ("upload_status", 0),
                ("download_status", 0),
                ("server_seq", 0),
                ("origin_source", 1),
                ("WCDB_CT_message_content", 0),
                ("WCDB_CT_source", 0),
            ] {
                fields.insert(k.into(), SqlValue::Integer(n.to_string()));
            }
            fields.insert("message_content".into(), SqlValue::Text(text.into()));
            fields.insert(
                "source".into(),
                SqlValue::Text("<msgsource><alnode><fr>1</fr></alnode></msgsource>".into()),
            );
            fields.insert("compress_content".into(), SqlValue::Text(String::new()));
            wechat_profile::apply_message_projection(
                &location,
                &keys,
                &MessageProjection {
                    peer: destination.into(),
                    sender: state.own_username.clone(),
                    from: state.own_username.clone(),
                    to: destination.into(),
                    server_message_id: server_message_id.into(),
                    fields,
                    packed_other_fields: vec![],
                },
                &options,
            )?
            .source_id
        };
        let recalled = if let Some(marker) =
            self.store
                .recall_marker(account, destination, server_message_id)?
        {
            self.project_recall_marker(account, &marker, receipt)?;
            self.store
                .recall_marker(account, destination, server_message_id)?
        } else {
            None
        };
        wechat_profile::apply_message_session_projection(
            &location,
            &keys,
            &MessageSessionProjection {
                source: source.clone(),
                peer: destination.into(),
                summary: recalled
                    .as_ref()
                    .map(|marker| self.recall_preview(account, marker))
                    .transpose()?
                    .unwrap_or_else(|| text.into()),
                outgoing: true,
                unread: false,
                sender: recalled.is_none().then(|| state.own_username.clone()),
                sender_display: None,
                created_by_this_intent: true,
            },
            &options,
        )?;
        let id = crate::message_id(&source)?;
        self.store.append_event_once(account,&format!("message:{id}"),"message.committed",&json!({"messageId":id,"threadId":destination,"origin":"local_send","direction":"outgoing"}))?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(source)
    }
}

#[cfg(test)]
mod encrypted_roundtrip {
    use super::*;
    use wechat_protocol::protobuf as p;
    pub(super) fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![b'A'; 16],
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 7,
            own_username: "synthetic_self".into(),
            cookie: vec![1; 15],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: p::message(&[
                p::bytes(1, &p::buffer(b"synthetic-a")),
                p::bytes(2, &p::buffer(b"synthetic-b")),
            ])
            .unwrap(),
            key_info: vec![4; 180],
            database_password: vec![42; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    pub(super) fn add(
        kind: u32,
        from: &str,
        to: &str,
        body: &[u8],
        sid: u64,
        time: u32,
    ) -> SyncCommand {
        let inline = if kind == 34 {
            b"\x02#!SILK_V3\x02\x00xy".as_slice()
        } else {
            b"".as_slice()
        };
        SyncCommand {
            id: 5,
            payload: p::message(&[
                p::uint(1, 1),
                p::bytes(2, &p::bytes(1, from.as_bytes())),
                p::bytes(3, &p::bytes(1, to.as_bytes())),
                p::uint(4, kind as u64),
                p::bytes(5, &p::bytes(1, body)),
                p::uint(6, 0),
                p::uint(7, 0),
                p::bytes(8, &p::buffer(inline)),
                p::uint(9, time as u64),
                p::bytes(10, b"<msgsource/>"),
                p::uint(12, sid),
                p::uint(13, 9),
            ])
            .unwrap(),
        }
    }
    #[test]
    fn supported_status_controls_recover_blocked_batch_without_session_mutation() {
        use base64::Engine;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("data");
        let store = Arc::new(Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state(), None).unwrap();
        core.ensure_managed_profile("one", &state()).unwrap();
        let key_file: Value = serde_json::from_slice(
            &std::fs::read(store.keys_file("one").unwrap().unwrap()).unwrap(),
        )
        .unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(
                key_file["databases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|key| key["relativePath"] == "session/session.db")
                    .unwrap()["rawPageKeyBase64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let sessions =
            rusqlite::Connection::open(location.root().join("db_storage/session/session.db"))
                .unwrap();
        sessions.execute_batch(&format!("PRAGMA key=\"x'{hex}'\";PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;")).unwrap();
        sessions.execute_batch("INSERT INTO Name2Id(rowid,user_name)VALUES(1,'peer'),(2,'synthetic_self');
            INSERT INTO SessionTable(username,unread_count,is_hidden,last_timestamp,last_clear_unread_timestamp,draft)VALUES('peer',3,0,120,50,'preserved draft'),('synthetic_self',3,0,120,50,'preserved draft');
            INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(1,9,1,95),(2,10,2,95);").unwrap();
        let control = add(
            51,
            "synthetic_self",
            "synthetic_self",
            br#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="4" seq="7"/></arg></op></msg>"#,
            900,
            100,
        );
        let packages = add(
            51,
            "synthetic_self",
            "peer",
            br#"<msg><op id="23"><name>packagesUpdated</name><arg><![CDATA[{"op":"newPackage","packageId":1}]]></arg></op></msg>"#,
            901,
            100,
        );
        let new_controls = vec![
            add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="3"><username>synthetic_self</username></op></msg>"#, 902, 100),
            add(51, "synthetic_self", "peer", br#"<msg><op id="15"><name>lastMessage</name><arg>{"messageSvrId":"0","MsgCreateTime":"0"}</arg></op></msg>"#, 903, 100),
            add(51, "synthetic_self", "peer", br#"<msg><op id="16"><name>lastMessage</name><arg>{"messageSvrId":"99","MsgCreateTime":"1700000000123"}</arg></op></msg>"#, 904, 100),
            add(10002, "weixin", "synthetic_self", br#"<sysmsg type="ClientCheckGetExtInfo"><ClientCheckGetExtInfo><ReportContext>private-check</ReportContext><Basic>1</Basic><Cellular>0</Cellular></ClientCheckGetExtInfo></sysmsg>"#, 905, 100),
            add(10002, "synthetic_self", "synthetic_self", br#"<appmsg appid="" sdkver="0"><type>35</type><msgcontent>private-card</msgcontent></appmsg>"#, 906, 100),
            add(10002, "synthetic_self", "room@chatroom", br#"<sysmsg type="mmchatroombarannouncememt"><mmchatroombarannouncememt><content>private-bar</content><xmlcontent>private-xml</xmlcontent><announcement_id>123</announcement_id></mmchatroombarannouncememt></sysmsg>"#, 907, 100),
        ];
        let batch = wechat_protocol::wire::SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands: [control.clone(), packages.clone()]
                .into_iter()
                .chain(new_controls.clone())
                .collect(),
            continuation: false,
        };
        let receipt = "projected:empty-handoff:0";
        store
            .stage_sync(
                "one",
                "empty-handoff",
                &core
                    .vault
                    .seal(
                        "one",
                        "apply:empty-handoff",
                        &serde_json::to_vec(&batch).unwrap(),
                    )
                    .unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &batch.cursor)
                    .unwrap(),
            )
            .unwrap();
        // An upgrade must be able to finish an original batch that the older
        // implementation retained, without deleting or replacing its bytes.
        store.block_sync("one", "empty-handoff").unwrap();
        store.append_event_once("one", &format!("control-observed:{receipt}"), "account.control_observed", &json!({"control":{"commandId":5,"messageType":51,"operationIds":[11]},"projection":"blocked"})).unwrap();
        assert!(store.pending_cursor_projection("one").unwrap());
        let result = core.recover_projection("one").unwrap();
        assert_eq!(result["projectedCommands"], 8);
        assert_eq!(result["pendingCommands"], 0);
        assert_eq!(store.pending_sync_count("one").unwrap(), 0);
        assert!(!store.pending_cursor_projection("one").unwrap());
        for (index, command) in batch.commands.iter().enumerate() {
            let receipt = format!("projected:empty-handoff:{index}");
            assert!(store.runtime("one", &receipt).unwrap().is_some());
            let key = format!("sync-control:{receipt}");
            let sealed = store.runtime("one", &key).unwrap().unwrap();
            assert_eq!(
                *core.vault.open_record("one", &key, &sealed).unwrap(),
                command.payload
            );
            assert!(!sealed
                .windows(b"HandOffMaster".len())
                .any(|bytes| bytes == b"HandOffMaster"));
            assert!(!sealed
                .windows(b"packageId".len())
                .any(|bytes| bytes == b"packageId"));
        }
        for peer in ["peer", "synthetic_self"] {
            let row = sessions.query_row("SELECT unread_count,is_hidden,last_timestamp,last_clear_unread_timestamp,draft FROM SessionTable WHERE username=?1", [peer], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,String>(4)?))).unwrap();
            assert_eq!(row, (3, 0, 120, 50, "preserved draft".into()));
        }
        assert_eq!(
            sessions
                .query_row("SELECT COUNT(*) FROM SessionUnreadListTable_1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        drop(sessions);
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&root).unwrap());
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
        // Replaying the command itself cannot duplicate its event or introduce
        // a profile effect, even outside the batch's normal receipt fast path.
        core.apply_sync_command("one", &control, receipt, "catchup")
            .unwrap();
        core.apply_sync_command("one", &packages, "projected:empty-handoff:1", "catchup")
            .unwrap();
        for (index, command) in new_controls.iter().enumerate() {
            core.apply_sync_command(
                "one",
                command,
                &format!("projected:empty-handoff:{}", index + 2),
                "catchup",
            )
            .unwrap();
        }
        let events = store.events("one", 0, 100).unwrap();
        let observed: Vec<_> = events
            .iter()
            .filter(|event| {
                event.kind == "account.control_observed"
                    && event.data["projection"] == "no_local_effect"
            })
            .collect();
        assert_eq!(observed.len(), 8);
        assert_eq!(observed[0].data["operationIds"], json!([11]));
        assert_eq!(
            observed[0].data["classification"],
            "empty_handoff_master_list"
        );
        assert_eq!(observed[1].data["operationIds"], json!([23]));
        for event in observed {
            let value = event.data.to_string();
            assert!(
                !value.contains("synthetic_self")
                    && !value.contains("packageId")
                    && !value.contains("synthetic-device")
                    && !value.contains("private-")
                    && !value.contains("<arg")
            );
        }
        assert!(!events.iter().any(|event| matches!(
            event.kind.as_str(),
            "message.committed" | "thread.read-state"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "sync.committed")
                .count(),
            1
        );
        let conflicting = add(
            51,
            "synthetic_self",
            "synthetic_self",
            br#"<msg><op id="11"><name>HandOffMaster</name><arg/></op></msg>"#,
            901,
            100,
        );
        assert!(core
            .apply_sync_command("one", &conflicting, receipt, "catchup")
            .unwrap_err()
            .to_string()
            .contains("sync_control_receipt_conflict"));
    }

    #[test]
    fn known_status_controls_do_not_complete_unknown_controls_or_starve_later_messages() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state(), None).unwrap();
        core.ensure_managed_profile("one", &state()).unwrap();
        let batch = wechat_protocol::wire::SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands: vec![
                add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="4" seq="7"/></arg></op></msg>"#, 900, 100),
                add(51, "synthetic_self", "peer", br#"<msg><op id="23"><name>packagesUpdated</name><arg>{"op":"newPackage","packageId":1}</arg></op></msg>"#, 905, 100),
                add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="2"><name>unknown</name><arg/></op></msg>"#, 901, 100),
                add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="4" seq="7"><handoffitem/></handofflist></arg></op></msg>"#, 902, 100),
                add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="5" seq="7"/></arg></op></msg>"#, 908, 100),
                add(51, "synthetic_self", "synthetic_self", br#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="4" seq="7" currenthandoffcount="0"/></arg></op></msg>"#, 909, 100),
                add(51, "synthetic_self", "peer", br#"<msg><op id="23"><name>packagesUpdated</name><arg>{"op":"update","packageId":1}</arg></op></msg>"#, 903, 100),
                add(10002, "peer", "synthetic_self", br#"<sysmsg type="dynacfg"/>"#, 904, 100),
                add(10002, "peer", "synthetic_self", br#"<sysmsg type="functionmsg"><private>sensitive-function-data</private></sysmsg>"#, 906, 100),
                add(10002, "peer", "synthetic_self", b"\xff\xfe", 907, 100),
                add(10002, "synthetic_self", "synthetic_self", br#"<appmsg appid="" sdkver="0"><type>35</type></appmsg>"#, 910, 100),
                add(1, "peer", "synthetic_self", b"First retained message", 77, 101),
            ],
            continuation: false,
        };
        let committed_before = store.runtime("one", "projected-cursor").unwrap();
        let result = core
            .commit_sync_batch("one", batch.clone(), "fixture", "fixture")
            .unwrap();
        assert_eq!(result["projection"]["projectedCommands"], 4);
        assert_eq!(result["projection"]["pendingCommands"], 8);
        assert_eq!(store.pending_sync_count("one").unwrap(), 1);
        assert!(store.pending_cursor_projection("one").unwrap());
        let later = wechat_protocol::wire::SyncBatch {
            commands: vec![add(
                1,
                "peer",
                "synthetic_self",
                b"Later retained message",
                78,
                102,
            )],
            ..batch
        };
        let result = core
            .commit_sync_batch("one", later, "fixture", "fixture")
            .unwrap();
        assert_eq!(result["projection"]["projectedCommands"], 1);
        assert_eq!(result["projection"]["pendingCommands"], 0);
        assert_eq!(
            core.recover_projection("one").unwrap()["pendingCommands"],
            8
        );
        assert_eq!(store.pending_sync_count("one").unwrap(), 1);
        assert_eq!(
            store.runtime("one", "projected-cursor").unwrap(),
            committed_before
        );
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "message.committed")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "account.control_observed"
                    && event.data["projection"] == "no_local_effect")
                .count(),
            3
        );
        assert!(!events.iter().any(|event| event.kind == "sync.committed"));
        assert!(!serde_json::to_string(&events)
            .unwrap()
            .contains("sensitive-function-data"));
    }

    #[tokio::test]
    async fn reordered_unread_columns_recover_message_only_commit_and_exact_read_once() {
        use base64::Engine;
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let state = state();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        let keys_path = store.keys_file("one").unwrap().unwrap();
        let key_file: Value = serde_json::from_slice(&std::fs::read(keys_path).unwrap()).unwrap();
        let key = key_file["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|key| key["relativePath"] == "session/session.db")
            .unwrap()["rawPageKeyBase64"]
            .as_str()
            .unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(key)
            .unwrap();
        let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let sessions =
            rusqlite::Connection::open(location.root().join("db_storage/session/session.db"))
                .unwrap();
        sessions
            .execute_batch(&format!(
                "PRAGMA key=\"x'{hex}'\";PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;"
            ))
            .unwrap();
        sessions.execute_batch("DROP TABLE SessionUnreadListTable_1;
            CREATE TABLE SessionUnreadListTable_1(username_id INTEGER,server_id INTEGER,create_time INTEGER,local_id INTEGER,CONSTRAINT _NameId_ServerId PRIMARY KEY(username_id,server_id));
            CREATE INDEX SessionUnreadListTable_1_NameId_CreateTime ON SessionUnreadListTable_1(username_id ASC,create_time ASC);
            CREATE INDEX unsupported_fixture_index ON SessionUnreadListTable_1(local_id);
            INSERT INTO SessionTable(username,unread_count,unread_first_msg_srv_id,draft,last_timestamp,last_clear_unread_timestamp)VALUES('peer',5,99,'preserved draft',50,0);").unwrap();
        let batch = wechat_protocol::wire::SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands: vec![add(1, "peer", "synthetic_self", b"Synthetic ping", 77, 100)],
            continuation: false,
        };
        let raw = serde_json::to_vec(&batch).unwrap();
        store
            .stage_sync(
                "one",
                "unread-order",
                &core.vault.seal("one", "apply:unread-order", &raw).unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &batch.cursor)
                    .unwrap(),
            )
            .unwrap();
        store
            .put_runtime("one", "apply-origin:unread-order", b"live")
            .unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["pendingCommands"],
            1
        );
        assert_eq!(
            core.messages("one", "peer", None, None).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .any(|event| event.kind == "message.committed"));
        sessions
            .execute_batch("DROP INDEX unsupported_fixture_index")
            .unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            1
        );
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
        let events = store.events("one", 0, 100).unwrap();
        let committed: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "message.committed")
            .collect();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].data["origin"], "live");
        assert_eq!(
            sessions
                .query_row(
                    "SELECT unread_count FROM SessionTable WHERE username='peer'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            6
        );
        let threads = core.threads("one", &Default::default()).unwrap();
        let version = threads["items"][0]["readStateVersion"].as_str().unwrap();
        let id = committed[0].data["messageId"].as_str().unwrap().to_string();
        core.mark_read_set("one", "peer", vec![id], version)
            .await
            .unwrap();
        let kept: (i64, String) = sessions
            .query_row(
                "SELECT unread_count,draft FROM SessionTable WHERE username='peer'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(kept, (5, "preserved draft".into()));
        assert_eq!(
            sessions
                .query_row("SELECT count(*) FROM SessionUnreadListTable_1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        // Finish a whole-thread profile commit whose sidecar completion was
        // interrupted. Recovery must remain local and reconcile stale counters.
        let threads = core.threads("one", &Default::default()).unwrap();
        let expected = threads["items"][0]["readStateVersion"].as_str().unwrap();
        let intent = json!({"threadReadTime":100,"expectedVersion":expected});
        store
            .begin_read(
                "one",
                "peer",
                &store.read_version("one", "peer").unwrap(),
                &intent,
            )
            .unwrap();
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core
            .write_options("one", "interrupted-thread-read")
            .unwrap();
        wechat_profile::mark_read_thread(&location, &keys, "peer", 100, &options).unwrap();
        assert_eq!(store.pending_reads("one").unwrap(), 1);
        assert_eq!(core.recover_read_projections("one").await.unwrap(), 1);
        assert_eq!(store.pending_reads("one").unwrap(), 0);
        assert_eq!(
            sessions
                .query_row(
                    "SELECT unread_count FROM SessionTable WHERE username='peer'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let control = add(51, "synthetic_self", "peer", br#"<msg><op id="1"><username>peer</username><name>lastMessage</name><arg><![CDATA[{"messageSvrId":"77","MsgCreateTime":"100"}]]></arg></op></msg>"#, 900, 101);
        for _ in 0..2 {
            core.apply_sync_command("one", &control, "remote-read-fixture", "live")
                .unwrap();
        }
        assert_eq!(
            sessions
                .query_row(
                    "SELECT last_clear_unread_timestamp FROM SessionTable WHERE username='peer'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            101
        );
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "message.committed")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "thread.read-state"
                    && event.data["origin"] == "remote_read")
                .count(),
            1
        );
    }
    #[test]
    fn exit_control_partial_read_is_guarded_once_and_never_completes_the_full_control() {
        partial_read_fixture(5, "peer", "partial-thread-exit-read", "remote_exit_read");
    }

    #[test]
    fn entry_control_partial_read_is_guarded_once_for_direct_and_group_sessions() {
        for peer in ["peer", "room@chatroom"] {
            partial_read_fixture(2, peer, "partial-thread-entry-read", "remote_entry_read");
        }
    }

    fn partial_read_fixture(
        operation: u32,
        ordinary_peer: &str,
        marker_prefix: &str,
        origin: &str,
    ) {
        use base64::Engine;
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state(), None).unwrap();
        core.ensure_managed_profile("one", &state()).unwrap();
        let key_file: Value = serde_json::from_slice(
            &std::fs::read(store.keys_file("one").unwrap().unwrap()).unwrap(),
        )
        .unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(
                key_file["databases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|key| key["relativePath"] == "session/session.db")
                    .unwrap()["rawPageKeyBase64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let hex: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let sessions =
            rusqlite::Connection::open(location.root().join("db_storage/session/session.db"))
                .unwrap();
        sessions.execute_batch(&format!("PRAGMA key=\"x'{hex}'\";PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;")).unwrap();
        sessions
            .execute(
                "INSERT INTO Name2Id(rowid,user_name)VALUES(1,?1)",
                [ordinary_peer],
            )
            .unwrap();
        sessions.execute("INSERT INTO SessionTable(username,unread_count,is_hidden,last_timestamp,last_clear_unread_timestamp,draft,sort_timestamp,status)VALUES(?1,3,0,120,0,'preserved draft',130,7)", [ordinary_peer]).unwrap();
        sessions.execute_batch("INSERT INTO SessionTable(username,unread_count,is_hidden,last_timestamp,last_clear_unread_timestamp)VALUES('hidden',4,1,100,0),('brandsessionholder',5,0,100,0),('uncertain',8,0,100,0);
            INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(1,9,1,95),(1,2,2,120);").unwrap();
        let peers = [
            ordinary_peer,
            "hidden",
            "absent",
            "brandsessionholder",
            "uncertain",
        ];
        let commands: Vec<_> = peers.iter().enumerate().map(|(index, peer)| {
            add(51, "synthetic_self", peer,
                format!("<msg><op id=\"{operation}\"><username>{peer}</username><name>lastMessage</name><arg><![CDATA[{{\"messageSvrId\":\"9\",\"MsgCreateTime\":\"1700000000123\"}}]]></arg></op></msg>").as_bytes(),
                900 + index as u64, 100)
        }).collect();
        // Simulate a crash after durable admission and before its outcome was
        // recorded. The profile may or may not have committed; never reapply.
        store
            .put_runtime(
                "one",
                &format!("{marker_prefix}:projected:partial-read-fixture:4"),
                br#"{"outcome":"needs_review","completeControl":false}"#,
            )
            .unwrap();
        for (index, command) in commands.iter().enumerate() {
            let receipt = format!("projected:partial-read-fixture:{index}");
            let error = core
                .apply_sync_command("one", command, &receipt, "live")
                .unwrap_err();
            assert_eq!(
                projection_blocker(&error).unwrap()["operationIds"],
                json!([operation])
            );
            assert!(store.runtime("one", &receipt).unwrap().is_none());
            let marker = store
                .runtime("one", &format!("{marker_prefix}:{receipt}"))
                .unwrap()
                .unwrap();
            let marker: Value = serde_json::from_slice(&marker).unwrap();
            assert_eq!(
                marker["outcome"],
                [
                    "applied",
                    "hidden_session",
                    "missing_session",
                    "excluded_peer",
                    "needs_review"
                ][index]
            );
        }
        let row = sessions.query_row("SELECT unread_count,last_clear_unread_timestamp FROM SessionTable WHERE username=?1", [ordinary_peer], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,i64>(1)?))).unwrap();
        assert_eq!(row, (2, 100));
        let preserved = sessions.query_row("SELECT draft,sort_timestamp,status,is_hidden,last_timestamp FROM SessionTable WHERE username=?1", [ordinary_peer], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?))).unwrap();
        assert_eq!(preserved, ("preserved draft".into(), 130, 7, 0, 120));
        for (index, command) in commands.iter().enumerate() {
            let key = format!("sync-control:projected:partial-read-fixture:{index}");
            let sealed = store.runtime("one", &key).unwrap().unwrap();
            assert_eq!(
                *core.vault.open_record("one", &key, &sealed).unwrap(),
                command.payload
            );
        }
        assert_eq!(
            sessions
                .query_row(
                    "SELECT COUNT(*) FROM SessionUnreadListTable_1 WHERE create_time=120",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        // Local changes after evaluation must survive recovery of this still
        // blocked batch, including a previously missing/hidden session.
        sessions.execute("UPDATE SessionTable SET unread_count=7,last_clear_unread_timestamp=50 WHERE username=?1", [ordinary_peer]).unwrap();
        sessions.execute_batch("
            INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(1,10,3,96);
            UPDATE SessionTable SET is_hidden=0 WHERE username='hidden';
            INSERT INTO SessionTable(username,unread_count,is_hidden,last_timestamp,last_clear_unread_timestamp)VALUES('absent',6,0,100,0);").unwrap();
        let batch = wechat_protocol::wire::SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands,
            continuation: false,
        };
        store
            .stage_sync(
                "one",
                "partial-read-fixture",
                &core
                    .vault
                    .seal(
                        "one",
                        "apply:partial-read-fixture",
                        &serde_json::to_vec(&batch).unwrap(),
                    )
                    .unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &batch.cursor)
                    .unwrap(),
            )
            .unwrap();
        drop(core);
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let projected_before = store.runtime("one", "projected-cursor").unwrap();
        for _ in 0..2 {
            let result = core.recover_projection("one").unwrap();
            assert_eq!(result["pendingCommands"], 5);
            assert_eq!(result["projectedCommands"], 0);
        }
        assert_eq!(store.pending_sync_count("one").unwrap(), 1);
        assert_eq!(
            store.runtime("one", "projected-cursor").unwrap(),
            projected_before
        );
        assert_eq!(sessions.query_row("SELECT unread_count,last_clear_unread_timestamp FROM SessionTable WHERE username=?1", [ordinary_peer], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,i64>(1)?))).unwrap(), (7, 50));
        for (peer, count) in [
            ("hidden", 4),
            ("absent", 6),
            ("brandsessionholder", 5),
            ("uncertain", 8),
        ] {
            assert_eq!(
                sessions
                    .query_row(
                        "SELECT unread_count FROM SessionTable WHERE username=?1",
                        [peer],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                count
            );
        }
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "thread.read-state"
                    && event.data["origin"] == origin
                    && event.data["completeControl"] == false)
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "message.committed")
                .count(),
            0
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "sync.committed")
                .count(),
            0
        );
    }
    #[test]
    fn encrypted_all_families_new_groups_outgoing_u64_and_replay_have_durable_rows_before_events() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let state = state();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        let mut commands = vec![];
        for (name, flag) in [
            ("peer", 1),
            ("member", 1),
            ("synthetic_self", 1),
            ("room@chatroom", 2),
        ] {
            commands.push(SyncCommand {
                id: 2,
                payload: p::message(&[
                    p::bytes(1, &p::bytes(1, name.as_bytes())),
                    p::bytes(2, &p::bytes(1, format!("Synthetic {name}").as_bytes())),
                    p::uint(7, u64::from(u32::MAX)),
                    p::uint(8, flag),
                ])
                .unwrap(),
            });
        }
        let cases = [
            (1, b"Synthetic text \xf0\x9f\x99\x82".as_slice()),
            (
                3,
                b"<msg><img md5=\"00112233445566778899aabbccddeeff\"/></msg>".as_slice(),
            ),
            (34, b"<msg><voicemsg voiceformat=\"4\"/></msg>".as_slice()),
            (43, b"<msg><videomsg/></msg>".as_slice()),
            (47, b"<msg><emoji/></msg>".as_slice()),
            (
                49,
                b"<msg><appmsg><type>6</type><title>synthetic.txt</title></appmsg></msg>"
                    .as_slice(),
            ),
        ];
        let mut index = 0;
        for (scope, outgoing) in [
            ("peer", false),
            ("peer", true),
            ("room@chatroom", false),
            ("room@chatroom", true),
        ] {
            for (kind, plain) in cases {
                index += 1;
                let body = if scope.ends_with("@chatroom") && !outgoing {
                    [b"member:\n".as_slice(), plain].concat()
                } else {
                    plain.to_vec()
                };
                let from = if outgoing { "synthetic_self" } else { scope };
                let to = if outgoing { scope } else { "synthetic_self" };
                commands.push(add(
                    kind,
                    from,
                    to,
                    &body,
                    u64::MAX - index,
                    100 + index as u32,
                ));
            }
        }
        index += 1;
        commands.push(add(
            10000,
            "room@chatroom",
            "synthetic_self",
            b"Synthetic system event",
            u64::MAX - index,
            100 + index as u32,
        ));
        let count = commands.len();
        let batch = wechat_protocol::wire::SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands,
            continuation: false,
        };
        let raw = serde_json::to_vec(&batch).unwrap();
        store
            .stage_sync(
                "one",
                "all-families",
                &core.vault.seal("one", "apply:all-families", &raw).unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &batch.cursor)
                    .unwrap(),
            )
            .unwrap();
        store
            .put_runtime("one", "apply-origin:all-families", b"live")
            .unwrap();
        let result = core.recover_projection("one").unwrap();
        assert_eq!(result["projectedCommands"], count);
        assert_eq!(result["pendingCommands"], 0);
        assert!(store.pending_sync("one").unwrap().is_empty());
        let reader = core.profile_reader("one").unwrap();
        let messages = reader
            .messages(&wechat_profile::MessageQuery {
                own_username: Some("synthetic_self".into()),
                ..Default::default()
            })
            .unwrap()
            .items;
        assert_eq!(messages.len(), 25);
        assert_eq!(
            messages.iter().filter(|m| m.outgoing == Some(true)).count(),
            12
        );
        assert_eq!(
            messages
                .iter()
                .filter(|m| m.sender.as_deref() == Some("member"))
                .count(),
            6
        );
        assert!(messages.iter().all(|m| m
            .server_message_id
            .as_deref()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > i64::MAX as u64));
        let voice = messages
            .iter()
            .find(|m| {
                m.peer.as_deref() == Some("peer") && m.message_type == 34 && !m.outgoing.unwrap()
            })
            .unwrap();
        assert_eq!(
            reader
                .cached_voice(
                    "message/media_0.db",
                    "peer",
                    &voice.id.local_id,
                    voice.server_message_id.as_deref()
                )
                .unwrap()
                .bytes,
            b"\x02#!SILK_V3\x02\x00xy"
        );
        drop(reader);
        let event_count = store
            .events("one", 0, 200)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "message.committed")
            .count();
        assert_eq!(event_count, 25);
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            25
        );
        let source = core
            .project_accepted_text(
                "one",
                "peer",
                "Synthetic accepted without sync echo",
                200,
                "77",
                "send:synthetic",
            )
            .unwrap();
        assert_eq!(
            source,
            core.project_accepted_text(
                "one",
                "peer",
                "Synthetic accepted without sync echo",
                200,
                "77",
                "send:synthetic"
            )
            .unwrap()
        );
        let stored = core
            .profile_reader("one")
            .unwrap()
            .message(&source, Some("synthetic_self"))
            .unwrap();
        assert_eq!(
            stored.raw_fields.get("server_seq"),
            Some(&SqlValue::Integer("0".into()))
        );
        assert_eq!(
            stored.raw_fields.get("origin_source"),
            Some(&SqlValue::Integer("1".into()))
        );
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            26
        );
        // An adopted duplicate with a different native status/codec is not rewritten.
        core.apply_sync_command(
            "one",
            &add(
                1,
                "synthetic_self",
                "peer",
                b"Synthetic accepted without sync echo",
                77,
                200,
            ),
            "late-sync-echo",
            "catchup",
        )
        .unwrap();
        let stored = core
            .profile_reader("one")
            .unwrap()
            .message(&source, None)
            .unwrap();
        assert_eq!(
            stored.raw_fields.get("origin_source"),
            Some(&SqlValue::Integer("1".into()))
        );
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .into_iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            26
        );
        for (i, (kind, xml)) in [
            (3, "<msg><img/></msg>"),
            (34, "<msg><voicemsg/></msg>"),
            (43, "<msg><videomsg/></msg>"),
            (
                49,
                "<msg><appmsg><type>6</type><title>synthetic</title></appmsg></msg>",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let server = (80 + i).to_string();
            let receipt = format!("send-media:{i}");
            let source = core
                .project_accepted_media(
                    "one",
                    "peer",
                    kind,
                    xml,
                    "Synthetic media",
                    210 + i as u64,
                    &server,
                    &receipt,
                )
                .unwrap();
            assert_eq!(
                source,
                core.project_accepted_media(
                    "one",
                    "peer",
                    kind,
                    xml,
                    "Synthetic media",
                    210 + i as u64,
                    &server,
                    &receipt
                )
                .unwrap()
            );
            let stored = core
                .profile_reader("one")
                .unwrap()
                .message(&source, Some("synthetic_self"))
                .unwrap();
            assert_eq!(stored.message_type, kind as i64);
            assert_eq!(stored.content.text.as_deref(), Some(xml));
            assert_eq!(
                stored.raw_fields.get("server_seq"),
                Some(&SqlValue::Integer("0".into()))
            );
            assert_eq!(
                stored.raw_fields.get("status"),
                Some(&SqlValue::Integer("2".into()))
            );
            assert!(core
                .project_accepted_media(
                    "one",
                    "peer",
                    kind,
                    "conflict",
                    "Synthetic media",
                    210 + i as u64,
                    &server,
                    &receipt
                )
                .is_err());
        }
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            30
        );
        let share = crate::ShareSendRequest::Share {
            thread_id: "peer".into(),
            title: "Shared & accepted".into(),
            url: "https://example.invalid/article?a=1&b=2".into(),
            description: Some("Description".into()),
            thumbnail_url: Some("https://example.invalid/preview.jpg".into()),
        };
        let payload = super::SendPayload {
            thread_id: "peer".into(),
            text: "".into(),
            created_at: 300,
            client_id: 123,
            share: Some(share),
            quote: None,
        };
        let sealed = core
            .vault
            .seal(
                "one",
                "send-payload",
                &serde_json::to_vec(&payload).unwrap(),
            )
            .unwrap();
        let (intent, _) = store
            .create_send(
                "one",
                "share-projection",
                "peer",
                "synthetic-share-hash",
                &sealed,
                "123",
            )
            .unwrap();
        store
            .update_send(
                "one",
                &intent.id,
                "accepted",
                Some("18446744073709551615"),
                None,
            )
            .unwrap();
        core.project_message_send("one", &intent.id).unwrap();
        let projected = store
            .send("one", &intent.id)
            .unwrap()
            .local_message_id
            .unwrap();
        core.project_message_send("one", &intent.id).unwrap();
        assert_eq!(
            store
                .send("one", &intent.id)
                .unwrap()
                .local_message_id
                .as_deref(),
            Some(projected.as_str())
        );
        let message = core.message("one", &projected).unwrap();
        assert_eq!(message["type"], 49);
        assert_eq!(message["appMessageType"], 5);
        assert_eq!(message["serverId"], "18446744073709551615");
        assert_eq!(message["direction"], "outgoing");
        assert_eq!(message["link"]["title"], "Shared & accepted");
        assert!(message["body"]
            .as_str()
            .unwrap()
            .contains("https://example.invalid/article?a=1&b=2"));
    }
}

impl Core {
    /// Commit one accepted outgoing media acknowledgement before publishing its
    /// message event. Voice/video local flags have bounded native examples. Image
    /// and application-file status2/origin1/sequence0 are explicit local insertion
    /// policy; their exact native local transition still needs a same-build check.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn project_accepted_media(
        &self,
        account: &str,
        destination: &str,
        message_type: u32,
        content: &str,
        summary: &str,
        created_at: u64,
        server_message_id: &str,
        receipt: &str,
    ) -> Result<wechat_profile::SourceMessageId> {
        identity(destination)?;
        if ![3, 34, 43, 49].contains(&message_type)
            || content.is_empty()
            || content.len() > 4 * 1024 * 1024
            || summary.len() > 65536
        {
            bail!("invalid_accepted_media")
        }
        let options = self.write_options(account, receipt)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let state = self.account_state(account)?;
        let time = i64::try_from(created_at)?;
        let subtype = if message_type == 49 {
            app_fields(content.as_bytes())?.0
        } else {
            0
        };
        let local_type = (u64::from(subtype) << 32) | message_type as u64;
        for name in [&state.own_username, destination] {
            wechat_profile::apply_contact_patch(
                &location,
                &keys,
                &ContactProjection {
                    username: name.to_string(),
                    fields: RawFields::new(),
                },
                &options,
            )?;
        }
        let source = if let Some(source) = wechat_profile::find_projected_message(
            &location,
            &keys,
            destination,
            server_message_id,
            &options,
        )? {
            let existing = self
                .profile_reader(account)?
                .message(&source, Some(&state.own_username))?;
            if self
                .store
                .recall_marker(account, destination, server_message_id)?
                .is_none()
                && (existing.local_type != local_type.to_string()
                    || existing.sender.as_deref() != Some(&state.own_username)
                    || existing.content.text.as_deref() != Some(content))
            {
                bail!("accepted_media_identity_conflict")
            }
            source
        } else {
            let sort = wechat_profile::choose_message_sort_sequence(
                &location,
                &keys,
                destination,
                server_message_id,
                time,
                &options,
            )?;
            let source_compressed = message_type == 43;
            let mut fields = RawFields::new();
            for (k, n) in [
                ("local_type", local_type as i64),
                ("sort_seq", sort),
                ("create_time", time),
                ("status", 2),
                ("upload_status", if message_type == 43 { 3 } else { 0 }),
                ("download_status", 0),
                ("server_seq", 0),
                ("origin_source", 1),
                ("WCDB_CT_message_content", 4),
                ("WCDB_CT_source", if source_compressed { 4 } else { 0 }),
            ] {
                fields.insert(k.into(), SqlValue::Integer(n.to_string()));
            }
            let local_source = b"<msgsource><alnode><fr>1</fr></alnode></msgsource>";
            fields.insert(
                "message_content".into(),
                wechat_profile::compressed_profile_field(content.as_bytes())?,
            );
            fields.insert(
                "source".into(),
                if source_compressed {
                    wechat_profile::compressed_profile_field(local_source)?
                } else {
                    SqlValue::Text(std::str::from_utf8(local_source)?.into())
                },
            );
            fields.insert("compress_content".into(), SqlValue::Text(String::new()));
            wechat_profile::apply_message_projection(
                &location,
                &keys,
                &MessageProjection {
                    peer: destination.into(),
                    sender: state.own_username.clone(),
                    from: state.own_username.clone(),
                    to: destination.into(),
                    server_message_id: server_message_id.into(),
                    fields,
                    packed_other_fields: vec![],
                },
                &options,
            )?
            .source_id
        };
        let recalled = if let Some(marker) =
            self.store
                .recall_marker(account, destination, server_message_id)?
        {
            self.project_recall_marker(account, &marker, receipt)?;
            self.store
                .recall_marker(account, destination, server_message_id)?
        } else {
            None
        };
        wechat_profile::apply_message_session_projection(
            &location,
            &keys,
            &MessageSessionProjection {
                source: source.clone(),
                peer: destination.into(),
                summary: recalled
                    .as_ref()
                    .map(|marker| self.recall_preview(account, marker))
                    .transpose()?
                    .unwrap_or_else(|| summary.into()),
                outgoing: true,
                unread: false,
                sender: recalled.is_none().then(|| state.own_username.clone()),
                sender_display: None,
                created_by_this_intent: true,
            },
            &options,
        )?;
        let id = crate::message_id(&source)?;
        self.store.append_event_once(account,&format!("message:{id}"),"message.committed",&json!({"messageId":id,"threadId":destination,"origin":"local_send","direction":"outgoing"}))?;
        self.store.put_runtime(account, receipt, b"1")?;
        Ok(source)
    }
}

#[cfg(test)]
#[path = "incoming_recall_tests.rs"]
mod recall_tests;

#[cfg(test)]
mod automatic_voice_tests {
    use super::encrypted_roundtrip::{add, state};
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<Store>, Arc<Core>) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state(), None).unwrap();
        core.ensure_managed_profile("one", &state()).unwrap();
        (temporary, store, core)
    }
    fn voice(server: u64) -> SyncCommand {
        add(
            34,
            "peer",
            "synthetic_self",
            b"<msg><voicemsg length='32' voicelength='1000'/></msg>",
            server,
            100,
        )
    }
    #[test]
    fn received_voice_queues_one_bound_playback_download_without_a_client() {
        let (_temporary, store, core) = fixture();
        core.apply_received_sync_command("one", &voice(200), "voice-first", "live")
            .unwrap();
        core.apply_received_sync_command("one", &voice(200), "voice-replay", "catchup")
            .unwrap();
        let queued = store.runtime_prefix("one", "received-voice:", 100).unwrap();
        assert_eq!(queued.len(), 1);
        let value: Value = serde_json::from_slice(&queued[0].1).unwrap();
        assert_eq!(value["request"]["quality"], "normal");
        let id = value["request"]["assetId"].as_str().unwrap();
        let asset = core.media_asset("one", id).unwrap();
        assert_eq!(asset.kind, crate::MediaKind::Voice);
        assert_eq!(asset.variant, crate::MediaVariant::Normal);
        assert!(store.queued_media_jobs("one").unwrap().is_empty());
    }
    #[test]
    fn local_repair_profile_import_and_non_voice_messages_do_not_queue_network_work() {
        let (_temporary, store, core) = fixture();
        core.apply_sync_command("one", &voice(200), "voice-repair", "live")
            .unwrap();
        core.apply_received_sync_command("one", &voice(201), "voice-import", "profile_import")
            .unwrap();
        core.apply_received_sync_command(
            "one",
            &add(1, "peer", "synthetic_self", b"Text", 202, 100),
            "text",
            "live",
        )
        .unwrap();
        assert!(store
            .runtime_prefix("one", "received-voice:", 100)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn automatic_voice_queue_survives_restart_and_cancelled_jobs_are_not_restarted() {
        let (temporary, store, core) = fixture();
        core.apply_received_sync_command("one", &voice(200), "voice-first", "live")
            .unwrap();
        let queued = store.runtime_prefix("one", "received-voice:", 100).unwrap();
        let value: Value = serde_json::from_slice(&queued[0].1).unwrap();
        let asset = value["request"]["assetId"].as_str().unwrap().to_owned();
        let key = format!("download:{asset}");
        let (mut row, _) = store
            .create_media_job(
                "one",
                "cancelled-voice",
                &key,
                &format!("{:x}", Sha256::digest(asset.as_bytes())),
                "download",
                &json!({"assetId":asset}),
                b"",
            )
            .unwrap();
        row.state = "cancelled".into();
        row.cancel_requested = true;
        store.save_media_job("one", &row).unwrap();
        drop(core);
        drop(store);
        let reopened = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        let core = Core::new(reopened.clone(), None).unwrap();
        assert_eq!(
            reopened
                .runtime_prefix("one", "received-voice:", 100)
                .unwrap()
                .len(),
            1
        );
        reopened
            .put_runtime("one", "desired", b"connected")
            .unwrap();
        reopened
            .put_runtime(
                "one",
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )
            .unwrap();
        assert_eq!(core.admit_received_voice("one").unwrap(), 1);
        assert_eq!(
            reopened.media_job("one", "cancelled-voice").unwrap().state,
            "cancelled"
        );
        assert!(reopened
            .runtime_prefix("one", "received-voice:", 100)
            .unwrap()
            .is_empty());
    }
    #[tokio::test]
    async fn automatic_voice_waits_for_queue_capacity_and_keeps_the_pending_receipt() {
        let (_temporary, store, core) = fixture();
        core.apply_received_sync_command("one", &voice(200), "voice-first", "live")
            .unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .put_runtime(
                "one",
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )
            .unwrap();
        for index in 0..100 {
            store
                .create_media_job(
                    "one",
                    &format!("full-{index}"),
                    &format!("full-{index}"),
                    &"0".repeat(64),
                    "download",
                    &Value::Null,
                    b"",
                )
                .unwrap();
        }
        assert_eq!(core.admit_received_voice("one").unwrap(), 0);
        assert_eq!(
            store
                .runtime_prefix("one", "received-voice:", 100)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.queued_media_jobs("one").unwrap().len(), 100);
    }
    #[test]
    fn recalled_voice_and_disconnected_accounts_never_start_automatic_transfers() {
        let (_temporary, store, core) = fixture();
        core.apply_received_sync_command("one", &voice(200), "voice-first", "live")
            .unwrap();
        store
            .put_runtime("one", "desired", b"disconnected")
            .unwrap();
        assert_eq!(core.admit_received_voice("one").unwrap(), 0);
        assert_eq!(
            store
                .runtime_prefix("one", "received-voice:", 100)
                .unwrap()
                .len(),
            1
        );
        core.apply_sync_command("one", &add(10002, "peer", "synthetic_self", b"<sysmsg type='revokemsg'><revokemsg><session>peer</session><msgid>12</msgid><newmsgid>200</newmsgid><replacemsg>Recalled</replacemsg><revoketime>0</revoketime></revokemsg></sysmsg>", 999, 110), "voice-recall", "live").unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .put_runtime(
                "one",
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )
            .unwrap();
        for _ in 0..3 {
            assert_eq!(core.admit_received_voice("one").unwrap(), 0);
        }
        assert!(store.queued_media_jobs("one").unwrap().is_empty());
        assert!(store
            .runtime_prefix("one", "received-voice:", 100)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn malformed_voice_metadata_does_not_block_projection_or_create_network_work() {
        let (_temporary, store, core) = fixture();
        core.apply_received_sync_command(
            "one",
            &add(
                34,
                "peer",
                "synthetic_self",
                b"<msg><voicemsg length='0'/></msg>",
                200,
                100,
            ),
            "bad-voice",
            "live",
        )
        .unwrap();
        assert!(store
            .runtime_prefix("one", "received-voice:", 100)
            .unwrap()
            .is_empty());
        assert!(store.runtime("one", "bad-voice").unwrap().is_some());
        assert!(store
            .events("one", 0, 200)
            .unwrap()
            .iter()
            .any(
                |event| event.kind == "media.auto-download" && event.data["state"] == "unavailable"
            ));
    }
}
