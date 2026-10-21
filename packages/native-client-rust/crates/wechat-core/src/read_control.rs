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
/// by the caller; in particular opcode 5 is not an alias for opcode 1.
pub(super) fn parse_remote_thread_read(
    message: &AddMessage,
    own_username: &str,
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
                && op.attribute("id") == Some("1")
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
                .parse::<u32>()
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
