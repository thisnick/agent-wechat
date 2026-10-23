//! Explicit status-notification forms with no local profile effect.
//! Every other opcode and payload remains an unresolved projection obligation.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use wechat_protocol::wire::AddMessage;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoLocalEffectStatus {
    EmptyHandOffMaster,
    EmptyHandOffMasterList,
    PackagesUpdatedWithoutHandler,
    OwnUsernameWithoutHandler,
    ZeroLastMessageWithoutHandler,
    PeerLastMessageWithoutHandler,
}

impl NoLocalEffectStatus {
    pub(super) fn operation_id(&self) -> u32 {
        match self {
            Self::EmptyHandOffMaster | Self::EmptyHandOffMasterList => 11,
            Self::PackagesUpdatedWithoutHandler => 23,
            Self::OwnUsernameWithoutHandler => 3,
            Self::ZeroLastMessageWithoutHandler => 15,
            Self::PeerLastMessageWithoutHandler => 16,
        }
    }

    pub(super) fn classification(&self) -> &'static str {
        match self {
            Self::EmptyHandOffMaster => "empty_handoff_master",
            Self::EmptyHandOffMasterList => "empty_handoff_master_list",
            Self::PackagesUpdatedWithoutHandler => "packages_updated_without_handler",
            Self::OwnUsernameWithoutHandler => "own_username_without_handler",
            Self::ZeroLastMessageWithoutHandler => "zero_last_message_without_handler",
            Self::PeerLastMessageWithoutHandler => "peer_last_message_without_handler",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackagesUpdated {
    op: String,
    #[serde(rename = "packageId")]
    _package_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LastMessage {
    #[serde(rename = "messageSvrId")]
    server_id: String,
    #[serde(rename = "MsgCreateTime")]
    create_time: String,
}

fn field_text<'a, 'input>(node: roxmltree::Node<'a, 'input>) -> Result<&'a str> {
    ensure!(
        node.children().count() == 1 && node.children().all(|child| child.is_text()),
        "unsupported_status_control"
    );
    node.text().context("unsupported_status_control")
}

/// Linux x64 4.1.13.26 dispatches opcode 11/HandOffMaster to its handoff-list
/// consumer. Empty args have no work entries. The observed empty-list form
/// updates only in-memory sequence/counters when its inner opcode is 4 and
/// neither counter attribute is present; without items, dispatch loops do not
/// run. Other inner opcodes can request synchronization and remain blocked.
/// Table 0xa8b0ec0 and constructor 0x6202080 register exactly 1,2,4,5,7,9,11,
/// 13,14,21. Lookup 0x61fe220 returns a null handler for absent entries, and
/// dispatcher 0x61fc800 advances without calling one. Only the recorded forms
/// of unregistered opcodes 3,15,16,23 are classified here.
pub(super) fn parse_no_local_effect_status(
    message: &AddMessage,
    own_username: &str,
) -> Option<NoLocalEffectStatus> {
    let parse = || -> Result<NoLocalEffectStatus> {
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        };
        ensure!(
            message.message_type == 51
                && identity(own_username)
                && message.from == own_username
                && message.outgoing
                && identity(&message.to)
                && message.peer == message.to
                && message.created_at != 0,
            "unsupported_status_control"
        );
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
        let ordinary = |node: roxmltree::Node<'_, '_>, name: &str| {
            node.tag_name().namespace().is_none()
                && node.tag_name().name() == name
                && node.namespaces().len() == 0
        };
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
                && matches!(op.attribute("id"), Some("3" | "11" | "15" | "16" | "23"))
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
        if op.attribute("id") == Some("3") {
            ensure!(
                children.len() == 1
                    && message.to == own_username
                    && field_text(field("username")?)? == own_username,
                "unsupported_status_control"
            );
            return Ok(NoLocalEffectStatus::OwnUsernameWithoutHandler);
        }
        if matches!(op.attribute("id"), Some("15" | "16")) {
            ensure!(
                children.len() == 2
                    && message.to != own_username
                    && field_text(field("name")?)? == "lastMessage",
                "unsupported_status_control"
            );
            let last: LastMessage = serde_json::from_str(field_text(field("arg")?)?)?;
            if op.attribute("id") == Some("15") {
                ensure!(
                    last.server_id == "0" && last.create_time == "0",
                    "unsupported_status_control"
                );
                return Ok(NoLocalEffectStatus::ZeroLastMessageWithoutHandler);
            }
            let positive = |value: &str| {
                value
                    .parse::<u64>()
                    .is_ok_and(|n| n != 0 && n.to_string() == value)
            };
            ensure!(
                positive(&last.server_id) && positive(&last.create_time),
                "unsupported_status_control"
            );
            return Ok(NoLocalEffectStatus::PeerLastMessageWithoutHandler);
        }
        ensure!(children.len() == 2, "unsupported_status_control");
        let name = field_text(field("name")?)?;
        let arg = field("arg")?;
        match op.attribute("id") {
            Some("11") => {
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
                            child.is_text()
                                && child.text().is_some_and(|text| text.trim().is_empty())
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
            }
            Some("23") => {
                ensure!(
                    message.to != own_username && name == "packagesUpdated",
                    "unsupported_status_control"
                );
                let argument = field_text(arg)?;
                ensure!(argument.len() <= 4096, "unsupported_status_control");
                let packages: PackagesUpdated = serde_json::from_str(argument)?;
                ensure!(packages.op == "newPackage", "unsupported_status_control");
                Ok(NoLocalEffectStatus::PackagesUpdatedWithoutHandler)
            }
            _ => anyhow::bail!("unsupported_status_control"),
        }
    };
    parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_handler_policy_accepts_only_the_recorded_opcode_forms() {
        let own = "<msg><op id='3'><username>self</username></op></msg>";
        let last = |opcode, server, time| {
            format!("<msg><op id='{opcode}'><name>lastMessage</name><arg><![CDATA[{{\"messageSvrId\":\"{server}\",\"MsgCreateTime\":\"{time}\"}}]]></arg></op></msg>")
        };
        for (xml, peer, expected, opcode) in [
            (
                own.into(),
                "self",
                NoLocalEffectStatus::OwnUsernameWithoutHandler,
                3,
            ),
            (
                last(15, "0", "0"),
                "peer",
                NoLocalEffectStatus::ZeroLastMessageWithoutHandler,
                15,
            ),
            (
                last(16, "18446744073709551615", "1700000000123"),
                "peer",
                NoLocalEffectStatus::PeerLastMessageWithoutHandler,
                16,
            ),
        ] {
            let mut value = message(&xml);
            value.to = peer.into();
            value.peer = peer.into();
            let classified = parse_no_local_effect_status(&value, "self").unwrap();
            assert_eq!(classified, expected);
            assert_eq!(classified.operation_id(), opcode);
            for xml in [
                xml.replace("</msg>", "<op id='999'/></msg>"),
                xml.replace("<msg>", "<msg xmlns='urn:unknown'>"),
                xml.replace("<msg>", "<msg>unexpected"),
                xml.replace("</op>", "<unknown/></op>"),
                xml.replace("<op ", "<op extra='1' "),
                xml.replace("</op>", "<!--unknown--></op>"),
            ] {
                value.content = xml.into_bytes();
                assert!(parse_no_local_effect_status(&value, "self").is_none());
            }
        }
        for (opcode, server, time) in [
            (1, "0", "0"),
            (2, "1", "90"),
            (5, "1", "90"),
            (15, "1", "90"),
            (16, "0", "0"),
            (16, "01", "90"),
            (16, "1", "18446744073709551616"),
            (99, "1", "90"),
        ] {
            let mut value = message(&last(opcode, server, time));
            value.to = "peer".into();
            value.peer = "peer".into();
            assert!(parse_no_local_effect_status(&value, "self").is_none());
        }
        for xml in [
            own.replace(">self<", ">peer<"),
            own.replace("</op>", "<username>self</username></op>"),
        ] {
            assert!(parse_no_local_effect_status(&message(&xml), "self").is_none());
        }
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
    fn only_observed_peer_directed_packages_updated_shape_is_classified() {
        for xml in [
            PACKAGES.to_string(),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":0"),
            PACKAGES.replace("\"packageId\":1", &format!("\"packageId\":{}", u64::MAX)),
            PACKAGES.replace("<![CDATA[", "").replace("]]>", ""),
        ] {
            assert_eq!(
                parse_no_local_effect_status(&message_to(&xml, "peer"), "self"),
                Some(NoLocalEffectStatus::PackagesUpdatedWithoutHandler)
            );
        }
        assert!(parse_no_local_effect_status(&message(PACKAGES), "self").is_none());
        for xml in [
            PACKAGES.replace("packagesUpdated", "other"),
            PACKAGES.replace("newPackage", "other"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":-1"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":1.0"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":1e0"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":\"1\""),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":18446744073709551616"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":null"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":true"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":{}"),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":1,\"packageId\":1"),
            PACKAGES.replace(
                "\"op\":\"newPackage\"",
                "\"op\":\"newPackage\",\"op\":\"newPackage\"",
            ),
            PACKAGES.replace("\"packageId\":1", "\"packageId\":1,\"extra\":0"),
            PACKAGES.replace(",\"packageId\":1", ""),
            PACKAGES.replace("{\"op\":\"newPackage\",\"packageId\":1}", "[]"),
            PACKAGES.replace("<name>", "<name extra=\"value\">"),
            PACKAGES.replace("<arg>", "<arg xmlns=\"urn:unknown\">"),
            PACKAGES.replace("</arg>", "<function/></arg>"),
            PACKAGES.replace("<arg>", "<username>peer</username><arg>"),
            PACKAGES.replace("</op>", "<name>packagesUpdated</name></op>"),
            PACKAGES.replace("id=\"23\"", "id=\"2\""),
            PACKAGES.replace(
                "{\"op\":\"newPackage\",\"packageId\":1}",
                &format!(
                    "{}{{\"op\":\"newPackage\",\"packageId\":1}}",
                    " ".repeat(4096)
                ),
            ),
        ] {
            assert!(parse_no_local_effect_status(&message_to(&xml, "peer"), "self").is_none());
        }
        let mut foreign = message_to(PACKAGES, "peer");
        foreign.from = "other".into();
        assert!(parse_no_local_effect_status(&foreign, "self").is_none());
    }

    #[test]
    fn other_controls_and_ambiguous_or_nonempty_shapes_stay_blocked() {
        for xml in [
            EMPTY.replace("id=\"11\"", "id=\"2\""),
            EMPTY.replace("id=\"11\"", "id=\"23\""),
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
