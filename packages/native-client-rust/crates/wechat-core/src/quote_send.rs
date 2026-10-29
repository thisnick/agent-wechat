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

// A quote copies an owned descriptor; it does not require a supported/downloadable
// source URL. Keep alternative/expired representations available for references.
fn sticker_identity(content: &str, shared: bool) -> Option<()> {
    if content.to_ascii_uppercase().contains("<!DOCTYPE")
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
    let root = doc.root_element();
    if root.tag_name().name() != "msg"
        || root.tag_name().namespace().is_some()
        || doc
            .descendants()
            .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    let md5 = if shared {
        let app = crate::quotes::child(root, "appmsg")?;
        let attach = crate::quotes::child(app, "appattach")?;
        crate::quotes::leaf(crate::quotes::child(attach, "emoticonmd5")?)?
    } else {
        crate::quotes::child(root, "emoji")?
            .attribute("md5")?
            .to_owned()
    };
    (md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit())).then_some(())
}

fn forwarded_reference_content(content: &str) -> Result<String> {
    fn parse(value: &str) -> Result<roxmltree::Document<'_>> {
        ensure!(
            value.len() <= 128 * 1024
                && !value.to_ascii_uppercase().contains("<!DOCTYPE")
                && !value.to_ascii_uppercase().contains("<!ENTITY"),
            "quote_content_unavailable"
        );
        let doc = roxmltree::Document::parse_with_options(
            value,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 4096,
                ..Default::default()
            },
        )
        .context("quote_content_unavailable")?;
        ensure!(
            doc.descendants()
                .all(|n| n.ancestors().take(34).count() <= 32),
            "quote_content_unavailable"
        );
        Ok(doc)
    }
    fn field(node: roxmltree::Node<'_, '_>, name: &str) -> Result<String> {
        let nodes: Vec<_> = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == name)
            .collect();
        if nodes.is_empty() {
            return Ok(String::new());
        }
        ensure!(
            nodes.len() == 1 && nodes[0].tag_name().namespace().is_none(),
            "quote_content_unavailable"
        );
        let value = crate::quotes::leaf(nodes[0]).context("quote_content_unavailable")?;
        ensure!(
            value.len() <= 16384 && !value.contains('\0'),
            "quote_content_unavailable"
        );
        Ok(value)
    }
    let doc = parse(content)?;
    let app =
        crate::quotes::child(doc.root_element(), "appmsg").context("quote_content_unavailable")?;
    let title = field(app, "title")?;
    let description = field(app, "des")?;
    let record = crate::quotes::child(app, "recorditem").context("quote_content_unavailable")?;
    let embedded;
    let embedded_doc;
    let record = if let Some(node) = crate::quotes::child(record, "recordinfo") {
        node
    } else {
        embedded = crate::quotes::leaf(record).context("quote_content_unavailable")?;
        embedded_doc = parse(&embedded)?;
        embedded_doc.root_element()
    };
    ensure!(
        record.tag_name().name() == "recordinfo"
            && record.tag_name().namespace().is_none()
            && crate::quotes::child(record, "datalist").is_some(),
        "quote_content_unavailable"
    );
    let record_title = field(record, "title")?;
    let record_description = field(record, "desc")?;
    ensure!(
        !title.trim().is_empty()
            || !record_title.trim().is_empty()
            || !description.trim().is_empty()
            || !record_description.trim().is_empty(),
        "quote_content_unavailable"
    );
    let mut summary = String::from("<recordinfo><fromscene>0</fromscene><favcreatetime>0</favcreatetime><isChatRoom>0</isChatRoom>");
    for (name, value) in [("title", record_title), ("desc", record_description)] {
        if !value.is_empty() {
            summary.push_str(&format!("<{name}>{}</{name}>", escape(&value)));
        }
    }
    summary.push_str("<datalist count=\"0\"/></recordinfo>");
    Ok(format!("<msg><appmsg><title>{}</title><des>{}</des><type>19</type><recorditem>{}</recorditem></appmsg></msg>", escape(&title), escape(&description), escape(&summary)))
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
            19 => return forwarded_reference_content(content),
            1 | 87 => {
                ensure!(
                    app.text
                        .as_deref()
                        .is_some_and(|title| !title.trim().is_empty()),
                    "quote_content_unavailable"
                );
                return Ok(raw_content.into());
            }
            51 => {
                ensure!(app.link.is_some(), "quote_content_unavailable");
                return Ok(raw_content.into());
            }
            3..=6 => return Ok(raw_content.into()),
            8 => {
                sticker_identity(content, true).context("quote_content_unavailable")?;
                return Ok(if message.outgoing == Some(true) {
                    raw_content
                } else {
                    content
                }
                .into());
            }
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
    if message.message_type == 47 {
        sticker_identity(content, false).context("quote_content_unavailable")?;
        return Ok(if message.outgoing == Some(true) {
            raw_content
        } else {
            content
        }
        .into());
    }
    if message.message_type == 42 {
        crate::quotes::contact_card_text(content).context("quote_content_unavailable")?;
        return Ok(if message.outgoing == Some(true) {
            raw_content
        } else {
            content
        }
        .into());
    }
    if message.message_type == 48 {
        ensure!(
            crate::locations::parse(content).is_some(),
            "quote_content_unavailable"
        );
        return Ok(raw_content.into());
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
            (48, 0, "<msg><location x='12.5' y='-45.25' label='Example park'/></msg>"),
            (49, 3, "<msg><appmsg><type>3</type><title>Music</title></appmsg></msg>"),
            (49, 4, "<msg><appmsg><type>4</type><title>Shared video</title></appmsg></msg>"),
            (
                49,
                1,
                "<msg><appmsg><type>1</type><title>https://example.invalid/shared</title><url/></appmsg></msg>",
            ),
            (
                49,
                51,
                "<msg><appmsg><type>51</type><title>Channels post</title><finderFeed><desc>Post</desc></finderFeed></appmsg></msg>",
            ),
            (
                49,
                87,
                "<msg><appmsg><type>87</type><textannouncement>Announcement</textannouncement></appmsg></msg>",
            ),
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
        for (kind, subtype, content) in [
            (42, 0, "<msg username='contact_user' nickname='Shared &amp; contact' unknown='preserved'/>"),
            (47, 0, "<msg><emoji md5='0123456789abcdef0123456789abcdef' expiretime='1' encrypturl='https://example.invalid/alternate'/></msg>"),
            (49, 8, "<msg><appmsg><type>8</type><title>Shared sticker</title><appattach><emoticonmd5>0123456789abcdef0123456789abcdef</emoticonmd5><unknown>preserved</unknown></appattach></appmsg></msg>"),
        ] {
            let prefixed = format!("sender:\n{content}");
            assert_eq!(source_content(&message(kind, subtype, &prefixed)).unwrap(), content);
            let mut own = message(kind, subtype, &prefixed);
            own.outgoing = Some(true);
            assert_eq!(source_content(&own).unwrap(), prefixed);
            let mut direct = message(kind, subtype, content);
            direct.peer = Some("sender".into());
            assert_eq!(source_content(&direct).unwrap(), content);
        }
        for (kind, subtype, body) in [
            (10002, 0, "system"),
            (47, 0, "<msg><emoji/></msg>"),
            (42, 0, "<msg nickname='Missing identity'/>"),
            (42, 0, "<msg username='contact_user'><appmsg><type>5</type></appmsg></msg>"),
            (47, 0, "<msg><emoji md5='0123456789abcdef0123456789abcdef'/><emoji md5='0123456789abcdef0123456789abcdef'/></msg>"),
            (49, 8, "<msg><appmsg><type>8</type><appattach><emoticonmd5>bad</emoticonmd5></appattach></appmsg></msg>"),
            (48, 0, "<msg><location x='91' y='0'/></msg>"),
            (48, 0, "<msg><location x='0' y='0'/><location x='1' y='1'/></msg>"),
            (49, 135, "<msg><appmsg><type>135</type><title>Special adapter required</title></appmsg></msg>"),
            (49, 19, "<msg><appmsg><type>19</type></appmsg></msg>"),
            (
                49,
                74,
                "<msg><appmsg><type>74</type><title>Upload notice</title></appmsg></msg>",
            ),
            (49, 1, "<msg><appmsg><type>1</type><title/></appmsg></msg>"),
            (
                49,
                1,
                "<msg><appmsg><type>5</type><title>Wrong stored subtype</title></appmsg></msg>",
            ),
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

    #[test]
    fn forwarded_references_copy_only_the_immediate_summary_into_a_fresh_carrier() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/synthetic-forwarded-quote.json"
        ))
        .unwrap();
        let content = fixture["source"].as_str().unwrap();
        for selected in [
            message(49, 19, content),
            message(49, 19, &format!("sender:\n{content}")),
        ] {
            let pruned = source_content(&selected).unwrap();
            assert_eq!(pruned, fixture["expected"]);
            assert!(!pruned.contains("Private") && !pruned.contains("Nested"));
            let app = crate::quotes::app_message(&pruned).unwrap();
            assert_eq!(app.subtype, 19);
            assert_eq!(app.title.as_deref(), Some("Forwarded & bundle"));
        }
        let no_outer_title = content.replace("Forwarded &amp; bundle", "");
        let pruned = source_content(&message(49, 19, &no_outer_title)).unwrap();
        let app = crate::quotes::app_message(&pruned).unwrap();
        assert_eq!(
            app.text.as_deref(),
            Some("Summary & title\nSummary description")
        );
        assert!(!app.text.as_deref().unwrap().contains("Nested"));
        let parent = crate::quotes::app_message(&format!("<msg><appmsg><type>57</type><title>Reply</title><refermsg><type>49</type><svrid>123</svrid><content>{}</content></refermsg></appmsg></msg>", escape(&pruned))).unwrap();
        let embedded = crate::quotes::embedded_quote(&parent, None).unwrap();
        assert_eq!(embedded["state"], "embedded");
        assert_eq!(
            embedded["embeddedText"],
            "Summary & title\nSummary description"
        );
        for invalid in [
            content.replace("<recorditem>", "<recorditem/><recorditem>"),
            content.replace("Summary description", "<nested/>"),
            content.replace("&lt;recordinfo&gt;", "&lt;recordinfo xmlns='foreign'&gt;"),
            content
                .replace("&lt;datalist count='1'&gt;", "&lt;not-datalist&gt;")
                .replace("&lt;/datalist&gt;", "&lt;/not-datalist&gt;"),
        ] {
            assert!(source_content(&message(49, 19, &invalid)).is_err());
        }
    }
}
