//! Type-51 status controls the official client applies with no local effect:
//! controls whose every op lacks a registered handler and the recorded
//! deleteMessages and empty HandOffMaster forms.
use anyhow::{ensure, Context, Result};
use wechat_protocol::wire::AddMessage;

/// Status IDs with a handler in Linux x64 4.1.13.26: constructor 0x6202080
/// inserts exactly these ten entries from table 0xa8b0ec0.
const REGISTERED_OPERATIONS: [u32; 10] = [1, 2, 4, 5, 7, 9, 11, 13, 14, 21];
/// Ops one control may carry before it stays blocked as oversized.
const MAX_OPERATIONS: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoLocalEffectStatus {
    EmptyHandOffMaster,
    EmptyHandOffMasterList,
    DeleteMessagesWithoutHandler,
    /// Every op names an ID without a registered handler, in document order.
    UnregisteredOperations(Vec<u32>),
}

impl NoLocalEffectStatus {
    pub(super) fn operation_ids(&self) -> Vec<u32> {
        match self {
            Self::EmptyHandOffMaster | Self::EmptyHandOffMasterList => vec![11],
            Self::DeleteMessagesWithoutHandler => vec![9],
            Self::UnregisteredOperations(ids) => ids.clone(),
        }
    }

    pub(super) fn classification(&self) -> &'static str {
        match self {
            Self::EmptyHandOffMaster => "empty_handoff_master",
            Self::EmptyHandOffMasterList => "empty_handoff_master_list",
            Self::DeleteMessagesWithoutHandler => "delete_messages_without_handler",
            Self::UnregisteredOperations(_) => "unregistered_operation",
        }
    }
}

fn field_text<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> Result<&'a str> {
    ensure!(
        node.children().count() == 1 && node.children().all(|child| child.is_text()),
        "unsupported_status_control"
    );
    node.text().context("unsupported_status_control")
}

fn ordinary(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.tag_name().namespace().is_none()
        && node.tag_name().name() == name
        && node.namespaces().len() == 0
}

/// The IDs of a control whose every op lacks a registered handler.
///
/// Linux x64 4.1.13.26's parser 0x61fc800 walks the `op` children of `msg`
/// and converts each `id` with a default of zero. Lookup 0x61fe220 returns a
/// null handler for an absent ID, and the dispatcher advances to the next op
/// without calling one; the parser itself writes no account state. Type 51
/// bypasses the message writers, so such a control has no local effect,
/// whatever its sender or op contents. Only canonical decimal IDs count: a
/// lenient conversion could read `02` or ` 2` as a registered ID.
fn unregistered_operations(document: &roxmltree::Document<'_>) -> Result<Vec<u32>> {
    let root = document.root_element();
    ensure!(ordinary(root, "msg"), "unsupported_status_control");
    let mut ids = vec![];
    for op in root.children().filter(|node| node.is_element()) {
        ensure!(
            ordinary(op, "op")
                && op
                    .attributes()
                    .all(|attribute| attribute.namespace().is_none())
                && ids.len() < MAX_OPERATIONS,
            "unsupported_status_control"
        );
        let raw = op.attribute("id").context("unsupported_status_control")?;
        let id = raw
            .parse::<u32>()
            .ok()
            .filter(|id| *id <= i32::MAX as u32 && id.to_string() == raw)
            .context("unsupported_status_control")?;
        ensure!(
            !REGISTERED_OPERATIONS.contains(&id),
            "unsupported_status_control"
        );
        ids.push(id);
    }
    ensure!(!ids.is_empty(), "unsupported_status_control");
    Ok(ids)
}

/// Classify a type-51 control the official client applies without a local
/// effect: one whose every op lacks a registered handler (see
/// [`unregistered_operations`]) or one of the recorded opcode-9 and
/// opcode-11 forms.
///
/// Linux x64 4.1.13.26 dispatches opcode 11/HandOffMaster to its handoff-list
/// consumer. Empty args have no work entries. The observed empty-list form
/// updates only in-memory sequence/counters when its inner opcode is 4 and
/// neither counter attribute is present; without items, dispatch loops do not
/// run. Other inner opcodes can request synchronization and remain blocked.
pub(super) fn parse_no_local_effect_status(
    message: &AddMessage,
    own_username: &str,
) -> Option<NoLocalEffectStatus> {
    let parse = || -> Result<NoLocalEffectStatus> {
        ensure!(message.message_type == 51, "unsupported_status_control");
        let xml = std::str::from_utf8(&message.content)?;
        ensure!(
            xml.len() <= 1024 * 1024
                && !xml.to_ascii_uppercase().contains("<!DOCTYPE")
                && !xml.to_ascii_uppercase().contains("<!ENTITY"),
            "unsupported_status_control"
        );
        let document = roxmltree::Document::parse(xml)?;
        ensure!(
            document.descendants().count() <= 4096,
            "unsupported_status_control"
        );
        if let Ok(ids) = unregistered_operations(&document) {
            return Ok(NoLocalEffectStatus::UnregisteredOperations(ids));
        }
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        };
        ensure!(
            identity(own_username)
                && message.from == own_username
                && message.outgoing
                && identity(&message.to)
                && message.peer == message.to
                && message.created_at != 0,
            "unsupported_status_control"
        );
        let only_elements_and_spacing = |node: roxmltree::Node<'_, '_>| {
            node.children().all(|child| {
                child.is_element()
                    || (child.is_text() && child.text().is_some_and(|text| text.trim().is_empty()))
            })
        };
        let root = document.root_element();
        ensure!(
            ordinary(root, "msg")
                && root.attributes().len() == 0
                && only_elements_and_spacing(root),
            "unsupported_status_control"
        );
        let ops: Vec<_> = root.children().filter(|node| node.is_element()).collect();
        ensure!(ops.len() == 1, "unsupported_status_control");
        let op = ops[0];
        ensure!(
            ordinary(op, "op")
                && op.attributes().len() == 1
                && matches!(op.attribute("id"), Some("9" | "11"))
                && op
                    .attributes()
                    .all(|attribute| attribute.namespace().is_none())
                && only_elements_and_spacing(op),
            "unsupported_status_control"
        );
        let children: Vec<_> = op.children().filter(|node| node.is_element()).collect();
        let field = |name: &str| -> Result<roxmltree::Node<'_, '_>> {
            let mut nodes = children
                .iter()
                .copied()
                .filter(|node| ordinary(*node, name));
            let node = nodes.next().context("unsupported_status_control")?;
            ensure!(
                nodes.next().is_none() && node.attributes().len() == 0,
                "unsupported_status_control"
            );
            Ok(node)
        };
        ensure!(children.len() == 2, "unsupported_status_control");
        let name = field_text(field("name")?)?;
        let arg = field("arg")?;
        if op.attribute("id") == Some("9") {
            // Opcode 9 has a handler. This exact name falls through its two
            // service branches without using the argument.
            ensure!(
                message.to != own_username
                    && !message.to.ends_with("@chatroom")
                    && name == "deleteMessages"
                    && field_text(arg)? == "0",
                "unsupported_status_control"
            );
            return Ok(NoLocalEffectStatus::DeleteMessagesWithoutHandler);
        }
        ensure!(
            message.to == own_username && name == "HandOffMaster",
            "unsupported_status_control"
        );
        if arg
            .children()
            .all(|child| child.is_text() && child.text().is_some_and(str::is_empty))
        {
            return Ok(NoLocalEffectStatus::EmptyHandOffMaster);
        }
        ensure!(only_elements_and_spacing(arg), "unsupported_status_control");
        let lists: Vec<_> = arg.children().filter(|node| node.is_element()).collect();
        ensure!(lists.len() == 1, "unsupported_status_control");
        let list = lists[0];
        ensure!(
            ordinary(list, "handofflist")
                && list.attributes().len() == 5
                && list
                    .attributes()
                    .all(|attribute| attribute.namespace().is_none())
                && list.attribute("availablecount") == Some("1000")
                && list.attribute("opcode") == Some("4")
                && matches!(list.attribute("networkstatus"), Some("wifi" | "4g" | "5g"))
                && list.children().all(|child| {
                    child.is_text() && child.text().is_some_and(|text| text.trim().is_empty())
                }),
            "unsupported_status_control"
        );
        let device = list
            .attribute("devicevirtualid")
            .context("unsupported_status_control")?;
        ensure!(identity(device), "unsupported_status_control");
        let sequence = list
            .attribute("seq")
            .context("unsupported_status_control")?;
        ensure!(
            !sequence.is_empty()
                && sequence.bytes().all(|byte| byte.is_ascii_digit())
                && sequence
                    .parse::<u32>()
                    .is_ok_and(|value| value.to_string() == sequence),
            "unsupported_status_control"
        );
        Ok(NoLocalEffectStatus::EmptyHandOffMasterList)
    };
    parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unregistered(value: &AddMessage) -> Option<Vec<u32>> {
        match parse_no_local_effect_status(value, "self")? {
            NoLocalEffectStatus::UnregisteredOperations(ids) => Some(ids),
            _ => None,
        }
    }

    #[test]
    fn controls_without_a_registered_handler_complete_whatever_their_contents_or_sender() {
        let last = |opcode, server, time| {
            format!("<msg><op id='{opcode}'><name>lastMessage</name><arg><![CDATA[{{\"messageSvrId\":\"{server}\",\"MsgCreateTime\":\"{time}\"}}]]></arg></op></msg>")
        };
        for (xml, ids) in [
            // Recorded forms.
            (
                "<msg><op id='3'><username>self</username></op></msg>".into(),
                vec![3],
            ),
            (last(15, "0", "0"), vec![15]),
            (last(16, "18446744073709551615", "1700000000123"), vec![16]),
            (PACKAGES.into(), vec![23]),
            // Forms this parser used to leave blocked.
            (PACKAGES.replace("newPackage", "update"), vec![23]),
            (PACKAGES.replace("id=\"23\"", "id=\"24\""), vec![24]),
            (last(99, "1", "90"), vec![99]),
            (
                "<msg><op id='24'><private>unknown</private><arg/><arg/></op></msg>".into(),
                vec![24],
            ),
            (
                "<msg><op id='23' extra='1'/><op id='24'>text</op></msg>".into(),
                vec![23, 24],
            ),
            (
                "<msg extra='1'>\n <op id='0'/><!-- note -->\n</msg>".into(),
                vec![0],
            ),
            (
                format!("<msg><op id='{}'/></msg>", i32::MAX),
                vec![i32::MAX as u32],
            ),
            (EMPTY.replace("id=\"11\"", "id=\"23\""), vec![23]),
        ] {
            for mutation in 0..5 {
                let mut value = message_to(&xml, "peer");
                match mutation {
                    0 => {}
                    1 => value.from = "foreign".into(),
                    2 => value.outgoing = false,
                    3 => value.created_at = 0,
                    _ => {
                        value.to = "self".into();
                        value.peer = "self".into();
                    }
                }
                assert_eq!(unregistered(&value), Some(ids.clone()), "{xml}");
            }
        }
        let many = "<op id='23'/>".repeat(MAX_OPERATIONS);
        assert_eq!(
            unregistered(&message(&format!("<msg>{many}</msg>"))),
            Some(vec![23; MAX_OPERATIONS])
        );
        assert_eq!(
            NoLocalEffectStatus::UnregisteredOperations(vec![23, 24]).operation_ids(),
            vec![23, 24]
        );
    }

    #[test]
    fn registered_ambiguous_or_mixed_operation_ids_stay_blocked() {
        let op = |id: &str| format!("<msg><op id=\"{id}\"><name>other</name></op></msg>");
        for id in REGISTERED_OPERATIONS {
            assert!(parse_no_local_effect_status(&message(&op(&id.to_string())), "self").is_none());
        }
        let mut cases: Vec<String> = [
            // A lenient integer conversion could read these as a registered ID.
            "02",
            " 23",
            "23 ",
            "+23",
            "-1",
            "0x17",
            "23.0",
            "",
            "&#50;",
            "2147483648",
            // 2^32 + 23 wraps to 23 in 32 bits.
            "4294967319",
        ]
        .iter()
        .map(|id| op(id))
        .collect();
        cases.extend([
            "<msg><op id='23'/><op id='2'/></msg>".into(),
            "<msg><op id='5'/><op id='24'/></msg>".into(),
            "<msg><op/></msg>".into(),
            "<msg><op id='23'/><other/></msg>".into(),
            "<msg/>".into(),
            "<sysmsg><op id='23'/></sysmsg>".into(),
            "<msg xmlns='urn:unknown'><op id='23'/></msg>".into(),
            "<msg xmlns:o='urn:unknown'><op id='23'/></msg>".into(),
            "<msg><o:op xmlns:o='urn:unknown' id='23'/></msg>".into(),
            "<msg><op xmlns:o='urn:unknown' o:id='23'/></msg>".into(),
            "<msg><op xml:id='2' id='23'/></msg>".into(),
            "<msg><op id='23' id='24'/></msg>".into(),
            format!("<msg>{}</msg>", "<op id='23'/>".repeat(MAX_OPERATIONS + 1)),
            format!("<!DOCTYPE msg [<!ENTITY e SYSTEM 'file:///private'>]>{PACKAGES}"),
            format!("{}{}", " ".repeat(1024 * 1024), PACKAGES),
        ]);
        for xml in cases {
            assert!(
                parse_no_local_effect_status(&message_to(&xml, "peer"), "self").is_none(),
                "{xml}"
            );
        }
        let mut value = message_to(PACKAGES, "peer");
        value.message_type = 10002;
        assert!(parse_no_local_effect_status(&value, "self").is_none());
        value.message_type = 51;
        value.content = b"\xff\xfe".to_vec();
        assert!(parse_no_local_effect_status(&value, "self").is_none());
    }
    use wechat_protocol::protobuf as p;

    fn message(xml: &str) -> AddMessage {
        message_to(xml, "self")
    }

    fn message_to(xml: &str, recipient: &str) -> AddMessage {
        wechat_protocol::wire::decode_add_message(
            &p::message(&[
                p::uint(1, 1),
                p::bytes(2, &p::bytes(1, b"self")),
                p::bytes(3, &p::bytes(1, recipient.as_bytes())),
                p::uint(4, 51),
                p::bytes(5, &p::bytes(1, xml.as_bytes())),
                p::uint(6, 0),
                p::uint(7, 0),
                p::bytes(8, &p::buffer(&[])),
                p::uint(9, 100),
            ])
            .unwrap(),
            "self",
        )
        .unwrap()
    }

    const EMPTY: &str = r#"<msg><op id="11"><name>HandOffMaster</name><arg/></op></msg>"#;
    const EMPTY_LIST: &str = r#"<msg><op id="11"><name>HandOffMaster</name><arg><handofflist availablecount="1000" devicevirtualid="synthetic-device" networkstatus="wifi" opcode="4" seq="7"/></arg></op></msg>"#;

    #[test]
    fn only_recorded_delete_messages_form_completes() {
        let xml = "<msg><op id='9'><name>deleteMessages</name><arg>0</arg></op></msg>";
        assert_eq!(
            parse_no_local_effect_status(&message_to(xml, "peer"), "self"),
            Some(NoLocalEffectStatus::DeleteMessagesWithoutHandler)
        );
        for recipient in ["self", "room@chatroom"] {
            assert!(parse_no_local_effect_status(&message_to(xml, recipient), "self").is_none());
        }
        let mut incoming = message_to(xml, "peer");
        incoming.outgoing = false;
        assert!(parse_no_local_effect_status(&incoming, "self").is_none());
        for changed in [
            xml.replace("deleteMessages", "DeleteMessages"),
            xml.replace("deleteMessages", "MomentsTimelineStatus"),
            xml.replace("deleteMessages", "VoiceMsgStatus"),
            xml.replace("<arg>0", "<arg>1"),
            xml.replace("<arg>0</arg>", "<arg><zero/></arg>"),
            xml.replace("</msg>", "<op id='23'/></msg>"),
        ] {
            assert!(parse_no_local_effect_status(&message_to(&changed, "peer"), "self").is_none());
        }
    }
    const PACKAGES: &str = r#"<msg><op id="23"><name>packagesUpdated</name><arg><![CDATA[{"op":"newPackage","packageId":1}]]></arg></op></msg>"#;

    #[test]
    fn only_empty_handoff_master_argument_is_classified() {
        for xml in [
            EMPTY.to_string(),
            EMPTY.replace("<arg/>", "<arg></arg>"),
            EMPTY.replace("<arg/>", "<arg><![CDATA[]]></arg>"),
        ] {
            assert_eq!(
                parse_no_local_effect_status(&message(&xml), "self"),
                Some(NoLocalEffectStatus::EmptyHandOffMaster)
            );
        }
    }

    #[test]
    fn only_observed_empty_handoff_list_with_memory_only_opcode_is_classified() {
        for xml in [
            EMPTY_LIST.to_string(),
            EMPTY_LIST.replace("networkstatus=\"wifi\"", "networkstatus=\"4g\""),
            EMPTY_LIST.replace("networkstatus=\"wifi\"", "networkstatus=\"5g\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"0\""),
            EMPTY_LIST.replace("seq=\"7\"", &format!("seq=\"{}\"", u32::MAX)),
            EMPTY_LIST
                .replace("<arg>", "<arg>\n ")
                .replace("/></arg>", "> \n </handofflist>\n </arg>"),
        ] {
            assert_eq!(
                parse_no_local_effect_status(&message(&xml), "self"),
                Some(NoLocalEffectStatus::EmptyHandOffMasterList)
            );
        }
        assert!(parse_no_local_effect_status(&message_to(EMPTY_LIST, "peer"), "self").is_none());
        for xml in [
            EMPTY_LIST.replace("availablecount=\"1000\"", "availablecount=\"999\""),
            EMPTY_LIST.replace("availablecount=\"1000\"", "availablecount=\"01000\""),
            EMPTY_LIST.replace(
                "devicevirtualid=\"synthetic-device\"",
                "devicevirtualid=\"\"",
            ),
            EMPTY_LIST.replace("synthetic-device", &"x".repeat(513)),
            EMPTY_LIST.replace("synthetic-device", "bad&#10;identity"),
            EMPTY_LIST.replace("networkstatus=\"wifi\"", "networkstatus=\"ethernet\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"-1\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"+1\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"07\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"1.0\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"4294967296\""),
            EMPTY_LIST.replace("seq=\"7\"", "seq=\"7\" seq=\"7\""),
            EMPTY_LIST.replace(
                "<handofflist ",
                "<handofflist currentfloatwindowcount=\"0\" ",
            ),
            EMPTY_LIST.replace("<handofflist ", "<handofflist currenthandoffcount=\"0\" "),
            EMPTY_LIST.replace("<handofflist ", "<handofflist unknown=\"0\" "),
            EMPTY_LIST.replace("<handofflist ", "<handofflist xmlns=\"urn:unknown\" "),
            EMPTY_LIST.replace("<arg>", "<arg extra=\"value\">"),
            EMPTY_LIST.replace("<arg>", "<arg>unexpected"),
            EMPTY_LIST.replace("/></arg>", "><handoffitem/></handofflist></arg>"),
            EMPTY_LIST.replace("/></arg>", ">unexpected</handofflist></arg>"),
            EMPTY_LIST.replace("/></arg>", "><!-- unknown --></handofflist></arg>"),
            EMPTY_LIST.replace("/></arg>", "/><handofflist/></arg>"),
            EMPTY_LIST.replace("</op>", "<arg/></op>"),
            EMPTY_LIST.replace("</msg>", "<op id=\"11\"/></msg>"),
        ] {
            assert!(parse_no_local_effect_status(&message(&xml), "self").is_none());
        }
        for opcode in ["0", "1", "2", "3", "5", "6", "999"] {
            let xml = EMPTY_LIST.replace("opcode=\"4\"", &format!("opcode=\"{opcode}\""));
            assert!(parse_no_local_effect_status(&message(&xml), "self").is_none());
        }
        for attribute in [
            " availablecount=\"1000\"",
            " devicevirtualid=\"synthetic-device\"",
            " networkstatus=\"wifi\"",
            " opcode=\"4\"",
            " seq=\"7\"",
        ] {
            assert!(parse_no_local_effect_status(
                &message(&EMPTY_LIST.replace(attribute, "")),
                "self"
            )
            .is_none());
        }
    }

    #[test]
    fn other_controls_and_ambiguous_or_nonempty_shapes_stay_blocked() {
        for xml in [
            EMPTY.replace("id=\"11\"", "id=\"2\""),
            EMPTY.replace("HandOffMaster", "other"),
            EMPTY.replace("<arg/>", "<arg> </arg>"),
            EMPTY.replace("<arg/>", "<arg><![CDATA[work]]></arg>"),
            EMPTY.replace("<arg/>", "<arg><function/></arg>"),
            EMPTY.replace("<arg/>", "<arg><!-- unknown --></arg>"),
            EMPTY.replace("<arg/>", "<arg private=\"value\"/>"),
            EMPTY.replace("<arg/>", "<arg/><arg/>"),
            EMPTY.replace("<arg/>", "<username>self</username><arg/>"),
            EMPTY.replace(
                "<name>HandOffMaster</name>",
                "<name>HandOffMaster</name><name>HandOffMaster</name>",
            ),
            EMPTY.replace(
                "<name>HandOffMaster</name>",
                "<name>HandOff<!-- gap -->Master</name>",
            ),
            EMPTY.replace("<msg>", "<msg xmlns=\"urn:unknown\">"),
            EMPTY.replace("<msg>", "<msg xmlns:other=\"urn:unknown\">"),
            EMPTY.replace("<arg/>", "<arg xmlns=\"urn:unknown\"/>"),
            EMPTY.replace("<msg>", "<msg extra=\"value\">"),
            EMPTY.replace("<op id=\"11\">", "<op id=\"11\" extra=\"value\">"),
            EMPTY.replace("</msg>", "<op id=\"11\"/></msg>"),
            EMPTY.replace("<msg>", "<msg>unexpected"),
            format!("<!DOCTYPE msg [<!ENTITY e SYSTEM 'file:///private'>]>{EMPTY}"),
            format!("{}{}", " ".repeat(1024 * 1024), EMPTY),
        ] {
            assert!(parse_no_local_effect_status(&message(&xml), "self").is_none());
        }
        for mutation in 0..7 {
            let mut value = message(EMPTY);
            match mutation {
                0 => value.from = "foreign".into(),
                1 => value.peer = "other".into(),
                2 => value.outgoing = false,
                3 => value.created_at = 0,
                4 => value.message_type = 10002,
                5 => value.to.clear(),
                _ => {
                    value.to = "peer".into();
                    value.peer = "peer".into();
                }
            }
            assert!(parse_no_local_effect_status(&value, "self").is_none());
        }
    }
}
