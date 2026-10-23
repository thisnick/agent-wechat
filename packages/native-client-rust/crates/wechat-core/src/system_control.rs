//! Narrow Linux 4.1.13.26 system controls without a local profile effect.
use anyhow::{ensure, Context, Result};
use wechat_protocol::wire::AddMessage;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NoLocalEffectSystem {
    ClientCheckWithoutHandler,
    SelfAppMessage35,
    ChatroomBarWithoutHandler,
}

impl NoLocalEffectSystem {
    pub(super) fn classification(&self) -> &'static str {
        match self {
            Self::ClientCheckWithoutHandler => "client_check_without_handler",
            Self::SelfAppMessage35 => "self_appmsg_35_log_only",
            Self::ChatroomBarWithoutHandler => "chatroom_bar_without_handler",
        }
    }
}

fn ordinary(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.tag_name().name() == name && node.tag_name().namespace().is_none()
}

fn elements_and_spacing(node: roxmltree::Node<'_, '_>) -> bool {
    node.children().all(|child| {
        child.is_element()
            || (child.is_text() && child.text().is_some_and(|text| text.trim().is_empty()))
    })
}

fn leaf(node: roxmltree::Node<'_, '_>) -> bool {
    node.attributes().len() == 0
        && node.children().count() == 1
        && node.children().all(|child| child.is_text())
}

fn exact_fields(node: roxmltree::Node<'_, '_>, fields: &[&str]) -> Result<()> {
    ensure!(
        node.attributes().len() == 0 && elements_and_spacing(node),
        "unsupported_system_control"
    );
    let children: Vec<_> = node.children().filter(|child| child.is_element()).collect();
    ensure!(children.len() == fields.len(), "unsupported_system_control");
    for name in fields {
        let matches: Vec<_> = children
            .iter()
            .filter(|child| ordinary(**child, name))
            .collect();
        ensure!(
            matches.len() == 1 && leaf(*matches[0]),
            "unsupported_system_control"
        );
    }
    Ok(())
}

/// Dispatcher 0x623c880 logs root appmsg/type 35 and returns without forwarding
/// it. Its complete predicate set has no ClientCheckGetExtInfo or
/// mmchatroombarannouncememt branch; the caller 0x62312d0 does not put these
/// type-10002 entries through ordinary message projection. This policy covers
/// only the recorded envelopes and shapes, not arbitrary system controls.
pub(super) fn parse_no_local_effect_system(
    message: &AddMessage,
    own_username: &str,
) -> Option<NoLocalEffectSystem> {
    let parse = || -> Result<NoLocalEffectSystem> {
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
        };
        ensure!(
            message.message_type == 10002
                && identity(own_username)
                && identity(&message.from)
                && identity(&message.to)
                && message.created_at != 0,
            "unsupported_system_control"
        );
        let xml = std::str::from_utf8(&message.content)?;
        ensure!(xml.len() <= 1024 * 1024, "unsupported_system_control");
        let document = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 4096,
                ..Default::default()
            },
        )?;
        ensure!(
            document.descendants().all(|node| {
                (!node.is_element()
                    || (node.tag_name().namespace().is_none() && node.namespaces().len() == 0))
                    && node
                        .attributes()
                        .all(|attribute| attribute.namespace().is_none())
                    && !node.is_comment()
                    && !node.is_pi()
            }),
            "unsupported_system_control"
        );
        let root = document.root_element();
        if ordinary(root, "appmsg") {
            ensure!(
                message.from == own_username
                    && message.to == own_username
                    && message.peer == own_username
                    && message.outgoing
                    && root.attributes().len() == 2
                    && root.attribute("appid") == Some("")
                    && root
                        .attribute("sdkver")
                        .is_some_and(|value| !value.is_empty())
                    && elements_and_spacing(root),
                "unsupported_system_control"
            );
            let types: Vec<_> = root
                .children()
                .filter(|child| child.is_element() && ordinary(*child, "type"))
                .collect();
            ensure!(
                types.len() == 1 && leaf(types[0]) && types[0].text() == Some("35"),
                "unsupported_system_control"
            );
            // Other appmsg fields are not consumed by this log-only branch.
            return Ok(NoLocalEffectSystem::SelfAppMessage35);
        }
        ensure!(
            ordinary(root, "sysmsg") && root.attributes().len() == 1 && elements_and_spacing(root),
            "unsupported_system_control"
        );
        let kind = root
            .attribute("type")
            .context("unsupported_system_control")?;
        let children: Vec<_> = root.children().filter(|child| child.is_element()).collect();
        ensure!(
            children.len() == 1 && ordinary(children[0], kind),
            "unsupported_system_control"
        );
        match kind {
            "ClientCheckGetExtInfo" => {
                ensure!(
                    message.from == "weixin"
                        && message.to == own_username
                        && message.peer == "weixin"
                        && !message.outgoing,
                    "unsupported_system_control"
                );
                exact_fields(children[0], &["ReportContext", "Basic", "Cellular"])?;
                Ok(NoLocalEffectSystem::ClientCheckWithoutHandler)
            }
            "mmchatroombarannouncememt" => {
                ensure!(
                    message.from == own_username
                        && message.to.ends_with("@chatroom")
                        && message.peer == message.to
                        && message.outgoing,
                    "unsupported_system_control"
                );
                exact_fields(children[0], &["content", "xmlcontent", "announcement_id"])?;
                Ok(NoLocalEffectSystem::ChatroomBarWithoutHandler)
            }
            _ => anyhow::bail!("unsupported_system_control"),
        }
    };
    parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECK: &str = "<sysmsg type='ClientCheckGetExtInfo'><ClientCheckGetExtInfo><ReportContext>synthetic</ReportContext><Basic>1</Basic><Cellular>0</Cellular></ClientCheckGetExtInfo></sysmsg>";
    const BAR: &str = "<sysmsg type='mmchatroombarannouncememt'><mmchatroombarannouncememt><content>notice</content><xmlcontent><![CDATA[<msg>synthetic</msg>]]></xmlcontent><announcement_id>123</announcement_id></mmchatroombarannouncememt></sysmsg>";
    const APP: &str = "<appmsg appid='' sdkver='0'><title>synthetic</title><type>35</type><appattach><extinfo>opaque</extinfo></appattach></appmsg>";

    fn message(from: &str, to: &str, xml: &str) -> AddMessage {
        AddMessage {
            from: from.into(),
            to: to.into(),
            peer: if from == "self" {
                to.into()
            } else {
                from.into()
            },
            sender_id: Some(from.into()),
            outgoing: from == "self",
            server_message_id: Some("99".into()),
            numbered1: 0,
            message_type: 10002,
            content: xml.as_bytes().to_vec(),
            content_text: None,
            numbered6: 0,
            numbered7: 0,
            inline_data: vec![],
            created_at: 100,
            msgsource: None,
            opaque11: None,
            numbered13: None,
            server_sequence: None,
            raw: vec![],
        }
    }

    #[test]
    fn only_recorded_system_envelopes_have_a_matching_build_classification() {
        for (from, to, xml, expected) in [
            (
                "weixin",
                "self",
                CHECK,
                NoLocalEffectSystem::ClientCheckWithoutHandler,
            ),
            (
                "self",
                "room@chatroom",
                BAR,
                NoLocalEffectSystem::ChatroomBarWithoutHandler,
            ),
            ("self", "self", APP, NoLocalEffectSystem::SelfAppMessage35),
        ] {
            let value = message(from, to, xml);
            assert_eq!(parse_no_local_effect_system(&value, "self"), Some(expected));
            for mutation in 0..6 {
                let mut invalid = value.clone();
                match mutation {
                    0 => invalid.from = "foreign".into(),
                    1 => invalid.to = "foreign".into(),
                    2 => invalid.peer = "foreign".into(),
                    3 => invalid.outgoing = !invalid.outgoing,
                    4 => invalid.message_type = 49,
                    _ => invalid.created_at = 0,
                }
                assert!(parse_no_local_effect_system(&invalid, "self").is_none());
            }
        }
    }

    #[test]
    fn unknown_ambiguous_and_active_system_controls_stay_blocked() {
        for xml in [
            CHECK.replace("<Basic>1</Basic>", "<Basic>1</Basic><Basic>2</Basic>"),
            CHECK.replace("<Basic>1</Basic>", "<Basic><work/></Basic>"),
            CHECK.replace("<Basic>1</Basic>", "<Basic extra='1'>1</Basic>"),
            CHECK.replace("ClientCheckGetExtInfo", "unknown"),
            CHECK.replace("<sysmsg ", "<sysmsg xmlns='urn:unknown' "),
            CHECK.replace("<sysmsg ", "<sysmsg xmlns:x='urn:unknown' "),
            CHECK.replace("<Basic>", "<!--unknown--><Basic>"),
            CHECK.replace("<Basic>", "<?unknown work?><Basic>"),
            CHECK.replace("<Basic>", "unexpected<Basic>"),
            format!("<!DOCTYPE sysmsg [<!ENTITY x 'private'>]>{CHECK}"),
            format!("{}{}", " ".repeat(1024 * 1024), CHECK),
            "<sysmsg type='dynacfg'><dynacfg/></sysmsg>".into(),
            "<sysmsg type='functionmsg'><functionmsg/></sysmsg>".into(),
        ] {
            assert!(
                parse_no_local_effect_system(&message("weixin", "self", &xml), "self").is_none()
            );
        }
        for xml in [
            APP.replace("<type>35</type>", "<type>6</type>"),
            APP.replace("<type>35</type>", "<type>35</type><type>35</type>"),
            APP.replace("appid=''", "appid='unknown'"),
            APP.replace("sdkver='0'", "sdkver=''"),
        ] {
            assert!(parse_no_local_effect_system(&message("self", "self", &xml), "self").is_none());
        }
        assert!(parse_no_local_effect_system(
            &message(
                "self",
                "room@chatroom",
                &BAR.replace("announcement_id", "unknown")
            ),
            "self"
        )
        .is_none());
    }
}
