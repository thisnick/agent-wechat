//! Recall controls refer to a target identity, not their AddMsg envelope ID.
use crate::{message_id, Core};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use wechat_protocol::wire::AddMessage;
use wechat_store::RecallMarker;

pub const RECALLED_BODY: &str = "[Message recalled]";
pub(crate) struct IncomingRecall {
    pub thread: String,
    pub server: String,
    pub recalled_at: Option<i64>,
    pub replacement_text: Option<String>,
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    let mut found = node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == name);
    let first = found.next()?;
    (found.next().is_none() && first.tag_name().namespace().is_none()).then_some(first)
}
fn leaf(node: roxmltree::Node<'_, '_>) -> Option<String> {
    (!node.children().any(|n| n.is_element())).then(|| {
        node.children()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect::<String>()
    })
}
fn exact_server(value: &str) -> Option<String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| n.to_string())
        .filter(|n| n == value)
}
fn recall_xml(text: &str) -> Option<roxmltree::Document<'_>> {
    if text.len() > 128 * 1024 {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 256,
            ..Default::default()
        },
    )
    .ok()?;
    let root = doc.root_element();
    (root.tag_name().name() == "sysmsg"
        && root.tag_name().namespace().is_none()
        && root.attribute("type") == Some("revokemsg"))
    .then_some(doc)
}
fn occurrence(revoke: roxmltree::Node<'_, '_>) -> Option<i64> {
    child(revoke, "revoketime")
        .and_then(leaf)
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0 && chrono::DateTime::from_timestamp(*n, 0).is_some())
}
fn notice(revoke: roxmltree::Node<'_, '_>, name: &str) -> Result<Option<String>> {
    let text = child(revoke, name)
        .map(|node| leaf(node).context("unsupported_recall_notice"))
        .transpose()?
        .filter(|text| !text.is_empty());
    if text.as_deref().is_some_and(|text| !valid_notice(text)) {
        bail!("unsupported_recall_notice");
    }
    Ok(text)
}
fn valid_notice(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16 * 1024
        && !text.chars().any(|c| {
            (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
}
pub(crate) fn incoming_recall(message: &AddMessage, own: &str) -> Result<Option<IncomingRecall>> {
    if message.message_type == 10000
        && std::str::from_utf8(&message.content)
            .ok()
            .and_then(recall_xml)
            .is_some()
    {
        // The supported official row is a result, not evidence that its same-row
        // shape is a wire command. Keep that unverified wire form staged.
        bail!("unsupported_recall_wire_form");
    }
    if message.message_type != 10002 {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(&message.content) else {
        return Ok(None);
    };
    let group_xml = || {
        // Incoming group controls use the ordinary "sender:\n" envelope.
        // Admit it only for this account's incoming group, preserving the
        // session/target checks below and the original sealed wire bytes.
        if !message.from.ends_with("@chatroom")
            || message.peer != message.from
            || message.to != own
            || message.outgoing
            || text.len() > 128 * 1024 + 514
        {
            return None;
        }
        let (sender, xml) = text.split_once(":\n")?;
        if sender.is_empty()
            || sender.len() > 512
            || sender.ends_with("@chatroom")
            || sender.chars().any(|c| c.is_whitespace() || c.is_control())
            || message.sender_id.as_deref().is_some_and(|id| id != sender)
        {
            return None;
        }
        recall_xml(xml)
    };
    let Some(doc) = recall_xml(text).or_else(group_xml) else {
        return Ok(None);
    };
    ensure!(
        doc.root_element()
            .children()
            .filter(|node| node.is_element())
            .all(|node| node.tag_name().namespace().is_none()
                && node.tag_name().name() == "revokemsg"),
        "unsupported_recall_control"
    );
    let revoke = child(doc.root_element(), "revokemsg").context("unsupported_recall_control")?;
    ensure!(
        revoke
            .children()
            .filter(|n| n.is_element())
            .all(|n| n.tag_name().namespace().is_none()
                && matches!(
                    n.tag_name().name(),
                    "session"
                        | "msgid"
                        | "newmsgid"
                        | "replacemsg"
                        | "revoketime"
                        | "announcement_id"
                )),
        "unsupported_recall_control"
    );
    for name in [
        "session",
        "msgid",
        "newmsgid",
        "replacemsg",
        "revoketime",
        "announcement_id",
    ] {
        ensure!(
            revoke
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == name)
                .count()
                <= 1,
            "unsupported_recall_control"
        );
    }
    if let Some(announcement) = child(revoke, "announcement_id") {
        // Linux 4.1.13.26 parser 0x60df1e0's ordinary recall branch (without
        // revoketime) reads session/newmsgid/replacemsg and ignores this field.
        // Admit only the recorded empty form, not announcement mutations.
        ensure!(
            announcement.attributes().len() == 0
                && announcement
                    .children()
                    .all(|node| node.is_text() && node.text().is_some_and(str::is_empty))
                && child(revoke, "revoketime").is_none(),
            "unsupported_recall_control"
        );
    }
    let thread = leaf(child(revoke, "session").context("unsupported_recall_control")?)
        .context("unsupported_recall_control")?;
    ensure!(
        !thread.is_empty()
            && thread.len() <= 512
            && !thread.chars().any(|c| c.is_control() || c.is_whitespace()),
        "unsupported_recall_control"
    );
    ensure!(
        thread == message.peer || (message.from == own && message.to == own),
        "recall_target_thread_mismatch"
    );
    let server = leaf(child(revoke, "newmsgid").context("unsupported_recall_control")?)
        .and_then(|s| exact_server(s.trim()))
        .context("unsupported_recall_target_identity")?;
    Ok(Some(IncomingRecall {
        thread,
        server,
        recalled_at: occurrence(revoke),
        replacement_text: notice(revoke, "replacemsg")?,
    }))
}
pub(crate) fn official_recall(message: &wechat_profile::Message) -> Option<Option<i64>> {
    if message.message_type != 10000 {
        return None;
    }
    let doc = recall_xml(message.content.text.as_deref()?)?;
    let revoke = child(doc.root_element(), "revokemsg")?;
    if revoke
        .children()
        .filter(|n| n.is_element())
        .any(|n| n.tag_name().namespace().is_some())
    {
        return None;
    }
    for name in ["newmsgid", "session", "revoketime", "content"] {
        if revoke
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == name)
            .count()
            > 1
        {
            return None;
        }
    }
    if let Some(target) = child(revoke, "newmsgid") {
        if exact_server(leaf(target)?.trim()).as_ref() != message.server_message_id.as_ref() {
            return None;
        }
    }
    if let Some(session) = child(revoke, "session") {
        if Some(leaf(session)?.as_str()) != message.peer.as_deref() {
            return None;
        }
    }
    Some(occurrence(revoke))
}
fn official_notice(message: &wechat_profile::Message) -> Option<String> {
    official_recall(message)?;
    let doc = recall_xml(message.content.text.as_deref()?)?;
    notice(child(doc.root_element(), "revokemsg")?, "content")
        .ok()
        .flatten()
        .filter(|text| text != RECALLED_BODY)
}
pub(crate) fn public_recall(marker: &RecallMarker) -> Value {
    let mut value = json!({"observedAt":marker.observed_at});
    if let Some(time) = marker
        .recalled_at
        .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
    {
        value["recalledAt"] = json!(time.to_rfc3339());
    }
    value
}
impl Core {
    pub(crate) fn reconcile_recall_markers(&self, account: &str) -> Result<()> {
        let first = self
            .store
            .runtime(account, "recall-reconciled-generation")?
            .as_deref()
            != Some(self.ownership_generation.as_bytes());
        for marker in self.store.recall_markers(account)? {
            if first || marker.projection_state == "pending" {
                use sha2::Digest;
                let hash = format!(
                    "{:x}",
                    sha2::Sha256::digest(serde_json::to_vec(&(
                        &marker.thread_id,
                        &marker.server_id
                    ))?)
                );
                self.project_recall_marker(account, &marker, &format!("recall-reconcile:{hash}"))?;
            }
        }
        if first {
            self.store.put_runtime(
                account,
                "recall-reconciled-generation",
                self.ownership_generation.as_bytes(),
            )?;
        }
        Ok(())
    }
    pub fn apply_confirmed_recall(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        recalled_at: Option<i64>,
        origin: &str,
        receipt: &str,
    ) -> Result<RecallMarker> {
        self.apply_confirmed_recall_with_notice(
            account,
            thread,
            server,
            recalled_at,
            None,
            origin,
            receipt,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_confirmed_recall_with_notice(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        recalled_at: Option<i64>,
        replacement_text: Option<&str>,
        origin: &str,
        receipt: &str,
    ) -> Result<RecallMarker> {
        ensure!(
            replacement_text.is_none_or(valid_notice),
            "unsupported_recall_notice"
        );
        // Operation receipts survive missing keys and partial official writes.
        // They do not retain a second settled message or original-author field.
        let source = self.recall_source(account, thread, server);
        let known = source.as_ref().ok().and_then(|s| s.as_ref());
        let control = if replacement_text.is_some()
            && self
                .store
                .runtime(account, &format!("sync-control:{receipt}"))?
                .is_some()
        {
            let parsed = self.recall_control(account, receipt)?;
            ensure!(
                parsed.thread == thread
                    && parsed.server == server
                    && parsed.replacement_text.as_deref() == replacement_text,
                "recall_control_target_mismatch"
            );
            Some(receipt)
        } else {
            None
        };
        let marker = self.store.record_recall_with_control(
            account,
            thread,
            server,
            known.map(|(id, _)| id.as_str()),
            known.map(|(_, created)| *created),
            recalled_at,
            origin,
            control,
        )?;
        source?;
        self.project_recall_with_notice(account, &marker, receipt, replacement_text)?;
        self.store
            .recall_marker(account, thread, server)?
            .context("recall_marker_missing")
    }
    fn recall_source(
        &self,
        account: &str,
        thread: &str,
        server: &str,
    ) -> Result<Option<(String, i64)>> {
        let messages = self
            .profile_reader(account)?
            .messages_by_server_id(thread, server, None)?;
        ensure!(messages.len() <= 1, "recall_target_ambiguous");
        messages
            .into_iter()
            .next()
            .map(|message| Ok((message_id(&message.id)?, message.create_time)))
            .transpose()
    }
    fn recall_control(&self, account: &str, receipt: &str) -> Result<IncomingRecall> {
        let key = format!("sync-control:{receipt}");
        let sealed = self
            .store
            .runtime(account, &key)?
            .context("recall_control_receipt_missing")?;
        let own = self
            .profile_username(account)?
            .context("recall_control_account_missing")?;
        let payload = self.vault.open_record(account, &key, &sealed)?;
        let message = wechat_protocol::wire::decode_add_message(&payload, &own)?;
        incoming_recall(&message, &own)?.context("recall_control_receipt_invalid")
    }
    fn retained_recall_notice(
        &self,
        account: &str,
        marker: &RecallMarker,
    ) -> Result<Option<String>> {
        let Some(receipt) =
            self.store
                .recall_control_receipt(account, &marker.thread_id, &marker.server_id)?
        else {
            return Ok(None);
        };
        let control = self.recall_control(account, &receipt)?;
        ensure!(
            control.thread == marker.thread_id && control.server == marker.server_id,
            "recall_control_target_mismatch"
        );
        Ok(control.replacement_text)
    }
    fn recall_projection_notice(
        &self,
        account: &str,
        marker: &RecallMarker,
    ) -> Result<Option<String>> {
        let reader = self.profile_reader(account)?;
        let messages = reader.messages_by_server_id(&marker.thread_id, &marker.server_id, None)?;
        ensure!(messages.len() <= 1, "recall_target_ambiguous");
        // Settled text is read from the official message. A receipt is only a
        // recovery input while that row is missing or unfinished.
        if let Some(message) = messages
            .first()
            .filter(|message| official_recall(message).is_some())
        {
            return Ok(Some(
                official_notice(message).unwrap_or_else(|| RECALLED_BODY.into()),
            ));
        }
        self.retained_recall_notice(account, marker)
    }
    pub(crate) fn recall_preview(&self, account: &str, marker: &RecallMarker) -> Result<String> {
        Ok(self
            .recall_projection_notice(account, marker)?
            .unwrap_or_else(|| RECALLED_BODY.into()))
    }
    pub(crate) fn project_recall_marker(
        &self,
        account: &str,
        marker: &RecallMarker,
        receipt: &str,
    ) -> Result<()> {
        self.project_recall_with_notice(account, marker, receipt, None)
    }
    fn project_recall_with_notice(
        &self,
        account: &str,
        marker: &RecallMarker,
        receipt: &str,
        transient_notice: Option<&str>,
    ) -> Result<()> {
        self.store
            .begin_recall_projection(account, &marker.thread_id, &marker.server_id)?;
        let notice = self.recall_projection_notice(account, marker)?;
        let notice = notice
            .as_deref()
            .or(transient_notice)
            .unwrap_or(RECALLED_BODY);
        let options = self.write_options(account, receipt)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let source = wechat_profile::apply_recall_projection_with_notice(
            &location,
            &keys,
            &marker.thread_id,
            &marker.server_id,
            marker.recalled_at,
            Some(notice),
            &options,
        )?;
        if let Some(source) = source {
            let id = message_id(&source)?;
            let created = self
                .profile_reader(account)?
                .message(&source, None)?
                .create_time;
            self.store.record_recall(
                account,
                &marker.thread_id,
                &marker.server_id,
                Some(&id),
                Some(created),
                marker.recalled_at,
                &marker.origin,
            )?;
            self.store.finish_recall_projection(
                account,
                &marker.thread_id,
                &marker.server_id,
                "applied",
            )?;
        } else {
            self.store.finish_recall_projection(
                account,
                &marker.thread_id,
                &marker.server_id,
                "target_absent",
            )?;
        }
        Ok(())
    }
    pub(crate) fn observe_profile_recall(
        &self,
        account: &str,
        msg: &wechat_profile::Message,
    ) -> Result<Option<RecallMarker>> {
        let Some(server) = msg.server_message_id.as_deref() else {
            return Ok(None);
        };
        if let Some(thread) = msg.peer.as_deref() {
            if let Some(marker) = self.store.recall_marker(account, thread, server)? {
                if marker.projection_state == "applied" && official_recall(msg).is_none() {
                    // An operation receipt is not canonical settled state. If
                    // a restored row needs repair, track that obligation again.
                    self.store
                        .begin_recall_projection(account, thread, server)?;
                    return self.store.recall_marker(account, thread, server);
                }
                return Ok(Some(marker));
            }
        }
        let pending = msg.outgoing == Some(true)
            && matches!(msg.raw_fields.get("status"),Some(wechat_profile::SqlValue::Integer(n)) if n=="7");
        // An explicit pending own row may carry an older provisional server ID.
        // Exact source identity also survives a removed peer/name-map entry.
        if msg.peer.is_none() || pending {
            if let Some(marker) =
                self.store
                    .recall_by_source(account, &message_id(&msg.id)?, msg.create_time)?
            {
                use md5::Digest;
                let table = format!("Msg_{:x}", md5::Md5::digest(marker.thread_id.as_bytes()));
                ensure!(
                    msg.id.table.eq_ignore_ascii_case(&table)
                        && msg
                            .peer
                            .as_ref()
                            .is_none_or(|thread| thread == &marker.thread_id),
                    "recall_source_thread_mismatch"
                );
                if marker.server_id == server || pending {
                    return Ok(Some(marker));
                }
            }
        }
        let Some(thread) = msg.peer.as_deref() else {
            return Ok(None);
        };
        if let Some(time) = official_recall(msg) {
            return Ok(Some(self.store.record_recall(
                account,
                thread,
                server,
                Some(&message_id(&msg.id)?),
                Some(msg.create_time),
                time,
                "profile_import",
            )?));
        }
        Ok(None)
    }
    pub(crate) fn ensure_message_not_recalled(&self, account: &str, id: &str) -> Result<()> {
        let reader = self.profile_reader(account)?;
        let msg = reader.message(
            &crate::source_message_id(id)?,
            self.profile_username(account)?.as_deref(),
        )?;
        if self.observe_profile_recall(account, &msg)?.is_some() {
            bail!("message_recalled");
        }
        if let Some(app) = crate::quotes::message_app(&msg) {
            if let Some((server, _)) = app.reference {
                if let Some(thread) = msg.peer.as_deref() {
                    if self
                        .store
                        .recall_marker(account, thread, &server)?
                        .is_some()
                    {
                        bail!("message_recalled");
                    }
                    for original in reader.messages_by_server_id(
                        thread,
                        &server,
                        self.profile_username(account)?.as_deref(),
                    )? {
                        if self.observe_profile_recall(account, &original)?.is_some() {
                            bail!("message_recalled");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn normalize_recalled(
        &self,
        account: &str,
        mut msg: wechat_profile::Message,
        own: Option<&str>,
        reader: &wechat_profile::ProfileReader,
    ) -> Result<Value> {
        let recalled = self.observe_profile_recall(account, &msg)?;
        let quoted_recalled = if let (Some(thread), Some((server, _))) = (
            msg.peer.as_deref(),
            crate::quotes::message_app(&msg).and_then(|app| app.reference),
        ) {
            self.quote_is_recalled(account, thread, &server, reader, own)?
        } else {
            false
        };
        if let Some(marker) = recalled.as_ref() {
            msg.peer = Some(marker.thread_id.clone());
        }
        let author = if recalled.is_some() {
            // Optional attribution must not prevent reading the cleared row.
            // Unavailable or unsupported receipt evidence leaves it unknown.
            self.infer_recalled_author(account, &msg, own)
                .unwrap_or_default()
        } else {
            None
        };
        let notice = if let Some(marker) = recalled.as_ref() {
            // Read the official row first. Pending state uses only its existing
            // sealed control receipt, without a copied settled notice column.
            if official_recall(&msg).is_some() {
                official_notice(&msg)
            } else {
                self.retained_recall_notice(account, marker)?
            }
        } else {
            None
        };
        let mut value = crate::profile::normalized_message(account, msg, own, reader.generation())?;
        if let Some(marker) = recalled {
            value["threadId"] = json!(marker.thread_id);
            value["peerId"] = json!(marker.thread_id);
            value["type"] = json!(10000);
            value["senderId"] = json!(author);
            value["direction"] =
                json!(author
                    .as_deref()
                    .zip(own)
                    .map(|(sender, own)| if sender == own {
                        "outgoing"
                    } else {
                        "incoming"
                    }));
            value["body"] = json!(RECALLED_BODY);
            value["quote"] = Value::Null;
            value["link"] = Value::Null;
            value["appMessageType"] = Value::Null;
            value["mentions"] =
                json!({"state":"known","userIds":[],"self":false,"all":false,"source":null});
            value["media"] = json!([]);
            value["recall"] = public_recall(&marker);
            if let Some(notice) = notice {
                value["recall"]["replacementText"] = json!(notice);
            }
        } else if quoted_recalled {
            if let Some(quote) = value["quote"].as_object_mut() {
                quote.retain(|key, _| matches!(key.as_str(), "serverId" | "type"));
                quote.insert("state".into(), json!("recalled"));
            }
        }
        Ok(value)
    }
    pub(crate) fn quote_is_recalled(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        reader: &wechat_profile::ProfileReader,
        own: Option<&str>,
    ) -> Result<bool> {
        if self.store.recall_marker(account, thread, server)?.is_some() {
            return Ok(true);
        }
        for mut original in reader.messages_by_server_id(thread, server, own)? {
            original.peer = Some(thread.into());
            if self.observe_profile_recall(account, &original)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
