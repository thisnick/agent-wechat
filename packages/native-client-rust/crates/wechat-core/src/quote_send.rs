//! Quote targets are resolved from the selected account, never caller XML.
use crate::{source_message_id, Core};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use wechat_profile::{ListQuery, Message};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuoteSendReference {
    pub message_id: String,
    // Expected unsigned server identity protects against reused local row IDs.
    pub server_id: String,
}
impl QuoteSendReference {
    pub(crate) fn validate(&self) -> Result<()> {
        let value = self
            .server_id
            .parse::<u64>()
            .context("invalid_quote_reference")?;
        ensure!(
            value > 0 && value.to_string() == self.server_id,
            "invalid_quote_reference"
        );
        source_message_id(&self.message_id).context("invalid_quote_reference")?;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ResolvedQuote {
    pub target: QuoteSendReference,
    /// Generated once at admission; replay and local repair reuse these bytes.
    pub xml: String,
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn source_content(message: &Message) -> Result<String> {
    let content = message
        .content
        .text
        .as_deref()
        .context("quote_content_unavailable")?;
    ensure!(
        content.len() <= 128 * 1024 && !content.contains('\0'),
        "quote_content_unavailable"
    );
    let group = message
        .peer
        .as_deref()
        .is_some_and(|s| s.ends_with("@chatroom"));
    let raw_content = content;
    let content = if group {
        message
            .sender
            .as_ref()
            .and_then(|sender| content.strip_prefix(&format!("{sender}:\n")))
            .unwrap_or(content)
    } else {
        content
    };
    if message.message_type == 1 {
        let content = if message.outgoing == Some(true) {
            raw_content
        } else {
            content
        };
        ensure!(!content.is_empty(), "quote_content_unavailable");
        return Ok(content.into());
    }
    if message.message_type == 49 {
        let app = crate::quotes::message_app(message).context("unsupported_quote_type")?;
        match app.subtype {
            5 | 6 => return Ok(raw_content.into()),
            57 => {
                let title = app.title.context("quote_content_unavailable")?;
                // The official content adapter retains the immediate quoted
                // message's comment, without recursively copying its reference.
                return Ok(format!(
                    "<msg><appmsg><title>{}</title><type>57</type></appmsg></msg>",
                    escape(&title)
                ));
            }
            _ => bail!("unsupported_quote_type"),
        }
    }
    let expected = match message.message_type {
        3 => "img",
        34 => "voicemsg",
        43 => "videomsg",
        _ => bail!("unsupported_quote_type"),
    };
    ensure!(
        !content.to_ascii_uppercase().contains("<!DOCTYPE")
            && !content.to_ascii_uppercase().contains("<!ENTITY"),
        "quote_content_unavailable"
    );
    let doc = roxmltree::Document::parse_with_options(
        content,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .context("quote_content_unavailable")?;
    let root = doc.root_element();
    ensure!(
        root.tag_name().name() == "msg"
            && root.tag_name().namespace().is_none()
            && crate::quotes::child(root, expected).is_some()
            && doc
                .descendants()
                .all(|n| n.ancestors().take(34).count() <= 32),
        "quote_content_unavailable"
    );
    Ok(raw_content.into())
}
impl Core {
    pub(crate) fn quote_target(
        &self,
        account: &str,
        thread: &str,
        reference: &QuoteSendReference,
    ) -> Result<Message> {
        reference.validate()?;
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        let original = reader
            .message(&source_message_id(&reference.message_id)?, own.as_deref())
            .context("quote_target_unavailable")?;
        ensure!(
            original.peer.as_deref() == Some(thread)
                && original.server_message_id.as_deref() == Some(reference.server_id.as_str()),
            "quote_target_mismatch"
        );
        let matches = reader.messages_by_server_id(thread, &reference.server_id, own.as_deref())?;
        ensure!(
            matches.len() == 1 && matches[0].id == original.id,
            "quote_target_ambiguous"
        );
        ensure!(
            !self.quote_is_recalled(
                account,
                thread,
                &reference.server_id,
                &reader,
                own.as_deref()
            )?,
            "message_recalled"
        );
        Ok(original)
    }
    pub(crate) fn resolve_quote(
        &self,
        account: &str,
        thread: &str,
        text: &str,
        target: &QuoteSendReference,
        own: &str,
    ) -> Result<ResolvedQuote> {
        let original = self.quote_target(account, thread, target)?;
        ensure!(
            !original
                .source
                .text
                .as_deref()
                .is_some_and(|s| s.contains("announcement@all")),
            "unsupported_quote_type"
        );
        let content = source_content(&original)?;
        let sender = original
            .sender
            .as_deref()
            .context("quote_sender_unavailable")?;
        ensure!(original.outgoing.is_some(), "quote_sender_unavailable");
        let reader = self.profile_reader(account)?;
        let query = ListQuery {
            limit: Some(100),
            search: Some(sender.into()),
            ..Default::default()
        };
        let mut name = reader
            .contacts(&query)?
            .items
            .into_iter()
            .find(|c| c.username == sender)
            .map(|c| c.label)
            .unwrap_or_else(|| sender.into());
        if thread.ends_with("@chatroom") {
            if let Ok(page) = reader.group_members(thread, &query) {
                if let Some(member) = page.items.into_iter().find(|m| m.username == sender) {
                    name = member.label;
                }
            }
        }
        let source = original.source.text.filter(|s| !s.is_empty());
        ensure!(
            source
                .as_ref()
                .is_none_or(|s| s.len() <= 16384 && !s.contains('\0')),
            "quote_content_unavailable"
        );
        let reference = wechat_protocol::quote_send::QuoteReference {
            message_type: i32::try_from(original.message_type)?,
            server_id: target.server_id.clone(),
            from_user: if original.outgoing == Some(true) {
                own.into()
            } else if thread.ends_with("@chatroom") {
                thread.into()
            } else {
                sender.into()
            },
            chat_user: sender.into(),
            display_name: name,
            content,
            created_at: u32::try_from(original.create_time).context("quote_content_unavailable")?,
            msg_source: source,
        };
        Ok(ResolvedQuote {
            target: target.clone(),
            xml: wechat_protocol::quote_send::quote_xml(own, text, &reference)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(kind: i64, subtype: u64, body: &str) -> Message {
        Message {
            id: wechat_profile::SourceMessageId {
                database: "message/message_0.db".into(),
                table: "Msg_synthetic".into(),
                local_id: "1".into(),
            },
            server_message_id: Some("123".into()),
            server_id_signed: Some("123".into()),
            peer: Some("room@chatroom".into()),
            sender: Some("sender".into()),
            outgoing: Some(false),
            message_type: kind,
            local_type: ((subtype << 32) | kind as u64).to_string(),
            create_time: 42,
            sort_seq: None,
            content: wechat_profile::DecodedField {
                text: Some(body.into()),
                state: wechat_profile::DecodeState::Utf8,
                compression_flag: Some(0),
            },
            source: wechat_profile::DecodedField {
                text: None,
                state: wechat_profile::DecodeState::Null,
                compression_flag: None,
            },
            raw_fields: Default::default(),
        }
    }
    #[test]
    fn ordinary_reference_content_is_scoped_and_nested_quotes_are_pruned() {
        for (kind, subtype, content) in [
            (1, 0, "Text & <xml>"),
            (3, 0, "<msg><img aeskey='synthetic-key'/></msg>"),
            (34, 0, "<msg><voicemsg voiceformat='4'/></msg>"),
            (43, 0, "<msg><videomsg playlength='3'/></msg>"),
            (
                49,
                5,
                "<msg><appmsg><type>5</type><title>Link</title></appmsg></msg>",
            ),
            (
                49,
                6,
                "<msg><appmsg><type>6</type><title>File</title></appmsg></msg>",
            ),
        ] {
            let prefixed = format!("sender:\n{content}");
            assert_eq!(
                source_content(&message(kind, subtype, &prefixed)).unwrap(),
                if kind == 1 {
                    content
                } else {
                    prefixed.as_str()
                }
            );
        }
        let mut own_text = message(1, 0, "sender:\nLiteral own prefix");
        own_text.outgoing = Some(true);
        assert_eq!(
            source_content(&own_text).unwrap(),
            "sender:\nLiteral own prefix"
        );
        let nested = "<msg><appmsg><type>57</type><title>Immediate &amp; reply</title><refermsg><type>1</type><svrid>999</svrid><content>Older private quote</content></refermsg></appmsg></msg>";
        let pruned = source_content(&message(49, 57, nested)).unwrap();
        assert!(pruned.contains("Immediate &amp; reply"));
        assert!(
            !pruned.contains("refermsg") && !pruned.contains("999") && !pruned.contains("Older")
        );
        for (kind, subtype, body) in [
            (10002, 0, "system"),
            (47, 0, "<msg><emoji/></msg>"),
            (49, 19, "<msg><appmsg><type>19</type></appmsg></msg>"),
            (49, 6, "<msg><appmsg><type>5</type></appmsg></msg>"),
            (3, 0, "<!DOCTYPE msg><msg><img/></msg>"),
            (3, 0, "<msg><videomsg/></msg>"),
        ] {
            assert!(source_content(&message(kind, subtype, body)).is_err());
        }
    }
    #[test]
    fn quote_reference_requires_an_exact_unsigned_server_identity() {
        let id = crate::message_id(&message(1, 0, "Text").id).unwrap();
        for server in ["0", "01", "-1", "1e3", "18446744073709551616", ""] {
            assert!(QuoteSendReference {
                message_id: id.clone(),
                server_id: server.into()
            }
            .validate()
            .is_err());
        }
        assert!(QuoteSendReference {
            message_id: id,
            server_id: u64::MAX.to_string()
        }
        .validate()
        .is_ok());
        assert!(QuoteSendReference {
            message_id: "not-a-source-id".into(),
            server_id: "1".into()
        }
        .validate()
        .is_err());
    }
}
