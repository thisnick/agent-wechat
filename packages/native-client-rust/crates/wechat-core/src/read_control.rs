//! Strictly recognized cross-device read controls; other controls stay opaque.
use super::*;
use anyhow::ensure;
use wechat_protocol::wire::AddMessage;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RemoteThreadRead {
    pub thread: String,
    /// The official handler uses the AddMsg notification time as its local
    /// clear cutoff. The JSON last-message time is not that cutoff.
    pub through_time: i64,
}

#[derive(Clone, Copy)]
pub(super) enum PartialThreadRead {
    Entry,
    Exit,
}

impl PartialThreadRead {
    fn marker_prefix(self) -> &'static str {
        match self {
            Self::Entry => "partial-thread-entry-read",
            // Preserve existing exit receipts across upgrades.
            Self::Exit => "partial-thread-exit-read",
        }
    }

    fn origin(self) -> &'static str {
        match self {
            Self::Entry => "remote_entry_read",
            Self::Exit => "remote_exit_read",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LastMessage {
    #[serde(rename = "messageSvrId")]
    server_id: String,
    #[serde(rename = "MsgCreateTime")]
    create_time: String,
}

/// Return only the observed own-account -> peer opcode 1 lastMessage form.
/// Missing, ambiguous, foreign-account and other opcode controls remain blocked
/// by the caller; opcodes 2 and 5 are not aliases for opcode 1.
pub(super) fn parse_remote_thread_read(
    message: &AddMessage,
    own_username: &str,
) -> Option<RemoteThreadRead> {
    parse_thread_control(message, own_username, "1")
}

/// Project only the shared unread component for the strict own-to-peer entry
/// form. Linux 4.1.13.26's 0x61f92c0 -> 0x638ce60 -> 0x638dd10 uses the outer
/// AddMsg time. Session preparation, recent handling and observers stay pending.
pub(super) fn parse_remote_thread_entry_read(
    message: &AddMessage,
    own_username: &str,
) -> Option<RemoteThreadRead> {
    let read = parse_thread_control(message, own_username, "2")?;
    // Friend-request sessions take a separate official entry path.
    (read.thread != "fmessage").then_some(read)
}

/// Opcode 5 includes a bounded unread-clear side effect, plus distinct session
/// exit behavior. Parsing this form does not mark its complete control applied.
pub(super) fn parse_remote_thread_exit_read(
    message: &AddMessage,
    own_username: &str,
) -> Option<RemoteThreadRead> {
    parse_thread_control(message, own_username, "5")
}

fn parse_thread_control(
    message: &AddMessage,
    own_username: &str,
    operation: &str,
) -> Option<RemoteThreadRead> {
    let parse = || -> Result<RemoteThreadRead> {
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        };
        ensure!(
            message.message_type == 51
                && identity(own_username)
                && message.from == own_username
                && message.to != own_username
                && message.peer == message.to
                && message.outgoing
                && identity(&message.to)
                && message.created_at != 0,
            "unsupported_read_control"
        );
        let xml = std::str::from_utf8(&message.content)?;
        ensure!(
            xml.len() <= 1024 * 1024
                && !xml.to_ascii_uppercase().contains("<!DOCTYPE")
                && !xml.to_ascii_uppercase().contains("<!ENTITY"),
            "unsupported_read_control"
        );
        let document = roxmltree::Document::parse(xml)?;
        ensure!(
            document.descendants().count() <= 4096,
            "unsupported_read_control"
        );
        let root = document.root_element();
        let ordinary = |node: roxmltree::Node<'_, '_>, name: &str| {
            node.tag_name().namespace().is_none() && node.tag_name().name() == name
        };
        let no_mixed_text = |node: roxmltree::Node<'_, '_>| {
            node.children()
                .filter(|child| child.is_text())
                .all(|child| child.text().is_some_and(|text| text.trim().is_empty()))
        };
        ensure!(
            ordinary(root, "msg") && root.attributes().len() == 0 && no_mixed_text(root),
            "unsupported_read_control"
        );
        let ops: Vec<_> = root.children().filter(|node| node.is_element()).collect();
        ensure!(ops.len() == 1, "unsupported_read_control");
        let op = ops[0];
        ensure!(
            ordinary(op, "op")
                && op.attributes().len() == 1
                && op.attribute("id") == Some(operation)
                && no_mixed_text(op),
            "unsupported_read_control"
        );
        let children: Vec<_> = op.children().filter(|node| node.is_element()).collect();
        ensure!(children.len() == 3, "unsupported_read_control");
        let field = |name: &str| -> Result<&str> {
            let nodes: Vec<_> = children
                .iter()
                .copied()
                .filter(|node| ordinary(*node, name))
                .collect();
            ensure!(
                nodes.len() == 1
                    && nodes[0].attributes().len() == 0
                    && nodes[0].children().count() == 1
                    && nodes[0].children().all(|node| node.is_text()),
                "unsupported_read_control"
            );
            nodes[0].text().context("unsupported_read_control")
        };
        ensure!(
            field("username")? == message.to && field("name")? == "lastMessage",
            "unsupported_read_control"
        );
        let last: LastMessage = serde_json::from_str(field("arg")?)?;
        ensure!(
            last.server_id
                .parse::<u64>()
                .is_ok_and(|value| value != 0 && value.to_string() == last.server_id),
            "unsupported_read_control"
        );
        ensure!(
            last.create_time
                // This metadata can exceed the notification's 32-bit seconds.
                // Validate it losslessly without using it as the read cutoff.
                .parse::<u64>()
                .is_ok_and(|value| value != 0 && value.to_string() == last.create_time),
            "unsupported_read_control"
        );
        Ok(RemoteThreadRead {
            thread: message.to.clone(),
            through_time: i64::from(message.created_at),
        })
    };
    parse().ok()
}

impl Core {
    /// Apply the supported read component once, even while the complete entry/exit
    /// control remains blocked. Evaluated no-ops are durable too: a later local
    /// session change must not make an older blocked control apply anew.
    pub(super) fn apply_partial_thread_read(
        &self,
        account: &str,
        read: &RemoteThreadRead,
        receipt: &str,
        kind: PartialThreadRead,
    ) -> Result<()> {
        let partial = format!("{}:{receipt}", kind.marker_prefix());
        if self.store.runtime(account, &partial)?.is_some() {
            return Ok(());
        }
        let options = self.write_options(account, &partial)?;
        let (location, keys) = self.profile_location_keys(account)?;
        // The official profile and sidecar cannot commit atomically. Admit one
        // attempt before touching the profile; an interrupted or failed attempt
        // remains uncertain instead of clearing newer local state on recovery.
        self.store.put_runtime(
            account,
            &partial,
            &serde_json::to_vec(&json!({"outcome":"needs_review","completeControl":false}))?,
        )?;
        // Reuse the conservative exit-read writer for both partial effects:
        // existing visible ordinary sessions only; never create or unhide one.
        let result = wechat_profile::mark_read_thread_exit(
            &location,
            &keys,
            &read.thread,
            read.through_time,
            &options,
        )?;
        if result.outcome == wechat_profile::ThreadExitReadOutcome::Applied {
            self.store.append_event_once(
                account,
                &partial,
                "thread.read-state",
                &json!({"threadId":read.thread,"readScope":"thread","origin":kind.origin(),"throughTime":read.through_time,"projection":result.projection,"completeControl":false}),
            )?;
        }
        self.store
            .put_runtime(account, &partial, &serde_json::to_vec(&result)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> AddMessage {
        AddMessage {
            from: "self".into(), to: "peer".into(), peer: "peer".into(),
            sender_id: None, outgoing: true, server_message_id: Some("99".into()),
            numbered1: 0, message_type: 51,
            content: br#"<msg><op id="1"><username>peer</username><name>lastMessage</name><arg><![CDATA[{"messageSvrId":"18446744073709551615","MsgCreateTime":"90"}]]></arg></op></msg>"#.to_vec(),
            content_text: None, numbered6: 0, numbered7: 0, inline_data: vec![], created_at: 100,
            msgsource: None, opaque11: None, numbered13: None, server_sequence: None, raw: vec![],
        }
    }

    #[test]
    fn own_peer_read_uses_notification_time_and_accepts_unsigned_message_identity() {
        assert_eq!(
            parse_remote_thread_read(&message(), "self"),
            Some(RemoteThreadRead {
                thread: "peer".into(),
                through_time: 100
            })
        );
        let mut group = message();
        group.to = "group@chatroom".into();
        group.peer = group.to.clone();
        group.content = String::from_utf8(group.content)
            .unwrap()
            .replace(
                "<username>peer</username>",
                "<username>group@chatroom</username>",
            )
            .into_bytes();
        assert_eq!(
            parse_remote_thread_read(&group, "self").unwrap().thread,
            "group@chatroom"
        );
    }

    #[test]
    fn exit_read_is_identified_separately_from_opcode_one() {
        let mut value = message();
        value.content = String::from_utf8(value.content)
            .unwrap()
            .replace("id=\"1\"", "id=\"5\"")
            .into_bytes();
        assert!(parse_remote_thread_read(&value, "self").is_none());
        assert_eq!(
            parse_remote_thread_exit_read(&value, "self")
                .unwrap()
                .through_time,
            100
        );
        assert!(parse_remote_thread_exit_read(&message(), "self").is_none());
        value.from = "foreign".into();
        assert!(parse_remote_thread_exit_read(&value, "self").is_none());
    }

    #[test]
    fn entry_read_is_partial_and_bound_to_an_ordinary_peer_or_group() {
        let original = String::from_utf8(message().content)
            .unwrap()
            .replace("id=\"1\"", "id=\"2\"");
        let mut value = message();
        value.content = original.as_bytes().to_vec();
        assert_eq!(
            parse_remote_thread_entry_read(&value, "self"),
            Some(RemoteThreadRead {
                thread: "peer".into(),
                through_time: 100,
            })
        );
        assert!(parse_remote_thread_read(&value, "self").is_none());
        assert!(parse_remote_thread_exit_read(&value, "self").is_none());
        for peer in ["room@chatroom", "fmessage", "self"] {
            value.to = peer.into();
            value.peer = peer.into();
            value.content = original
                .replace(
                    "<username>peer</username>",
                    &format!("<username>{peer}</username>"),
                )
                .into_bytes();
            assert_eq!(
                parse_remote_thread_entry_read(&value, "self").is_some(),
                peer == "room@chatroom"
            );
        }
        for mutation in 0..7 {
            let mut invalid = message();
            invalid.content = original.as_bytes().to_vec();
            match mutation {
                0 => invalid.from = "foreign".into(),
                1 => invalid.outgoing = false,
                2 => invalid.peer = "other".into(),
                3 => invalid.created_at = 0,
                4 => invalid.message_type = 1,
                5 => {
                    invalid.content = original
                        .replace("<username>peer</username>", "<username>other</username>")
                        .into_bytes()
                }
                _ => {
                    invalid.content = original
                        .replace("</msg>", "<op id='5'/></msg>")
                        .into_bytes()
                }
            }
            assert!(parse_remote_thread_entry_read(&invalid, "self").is_none());
        }
        for opcode in ["1", "3", "5", "11"] {
            value.to = "peer".into();
            value.peer = "peer".into();
            value.content = original
                .replace("id=\"2\"", &format!("id=\"{opcode}\""))
                .into_bytes();
            assert!(parse_remote_thread_entry_read(&value, "self").is_none());
        }
    }

    #[test]
    fn wide_last_message_times_do_not_replace_the_notification_cutoff() {
        let original = String::from_utf8(message().content).unwrap();
        for timestamp in ["4294967296", "1700000000123", "18446744073709551615"] {
            for operation in ["1", "2", "5"] {
                let mut value = message();
                value.content = original
                    .replace("id=\"1\"", &format!("id=\"{operation}\""))
                    .replace(
                        "MsgCreateTime\":\"90",
                        &format!("MsgCreateTime\":\"{timestamp}"),
                    )
                    .into_bytes();
                let read = match operation {
                    "1" => parse_remote_thread_read(&value, "self"),
                    "2" => parse_remote_thread_entry_read(&value, "self"),
                    _ => parse_remote_thread_exit_read(&value, "self"),
                }
                .unwrap();
                assert_eq!(read.thread, "peer");
                assert_eq!(read.through_time, 100);
            }
        }
    }

    #[test]
    fn invalid_last_message_times_remain_blocked_for_reads_and_exits() {
        let original = String::from_utf8(message().content).unwrap();
        for timestamp in [
            "0",
            "090",
            "-1",
            "+1",
            " 90",
            "90.0",
            "1e3",
            "18446744073709551616",
        ] {
            let mut value = message();
            value.content = original
                .replace(
                    "MsgCreateTime\":\"90",
                    &format!("MsgCreateTime\":\"{timestamp}"),
                )
                .into_bytes();
            assert!(parse_remote_thread_read(&value, "self").is_none());
            value.content = String::from_utf8(value.content)
                .unwrap()
                .replace("id=\"1\"", "id=\"2\"")
                .into_bytes();
            assert!(parse_remote_thread_entry_read(&value, "self").is_none());
            value.content = String::from_utf8(value.content)
                .unwrap()
                .replace("id=\"2\"", "id=\"5\"")
                .into_bytes();
            assert!(parse_remote_thread_exit_read(&value, "self").is_none());
        }
    }

    #[test]
    fn other_opcodes_ambiguous_xml_and_invalid_boundaries_stay_opaque() {
        let original = String::from_utf8(message().content).unwrap();
        for xml in [
            original.replace("id=\"1\"", "id=\"5\""),
            original.replace("id=\"1\"", "id=\"11\""),
            original.replace("</msg>", "<op id=\"1\"/></msg>"),
            original.replace("<username>peer</username>", "<username>other</username>"),
            original.replace(
                "<username>peer</username>",
                "<username>peer</username><username>peer</username>",
            ),
            original.replace("<msg>", "<msg xmlns=\"urn:unknown\">"),
            original.replace("<msg>", "<msg>unexpected"),
            original.replace(
                "<username>peer</username>",
                "<username>peer<!-- gap -->other</username>",
            ),
            original.replace("18446744073709551615", "18446744073709551616"),
            original.replace("MsgCreateTime\":\"90", "MsgCreateTime\":\"0"),
            original.replace("MsgCreateTime\":\"90", "MsgCreateTime\":\"090"),
            format!("<!DOCTYPE msg [<!ENTITY e SYSTEM 'file:///private'>]>{original}"),
        ] {
            let mut value = message();
            value.content = xml.into_bytes();
            assert!(parse_remote_thread_read(&value, "self").is_none());
        }
        for mutation in 0..5 {
            let mut value = message();
            match mutation {
                0 => value.from = "foreign".into(),
                1 => value.peer = "other".into(),
                2 => value.outgoing = false,
                3 => value.created_at = 0,
                _ => value.message_type = 1,
            };
            assert!(parse_remote_thread_read(&value, "self").is_none());
        }
    }
}
