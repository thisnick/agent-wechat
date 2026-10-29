//! Strict offline decoding of a supported subset of call system messages.
//!
//! This does not admit an invitation or deliver it to a controller. The caller
//! must separately verify account/recipient identity, fresh receipt admission,
//! current room/attempt and notification ordering. Even successful decoding does
//! not complete an official profile projection or advance its cursor. Retain
//! the original message for unknown effects, whether this returns None or Err.

use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use wechat_protocol::{
    calls::{
        incoming::{CarrierKind, ConferenceNotification, NotificationCarrier, NotificationPayload},
        signaling::MAX_SIGNALING_BYTES,
    },
    wire::AddMessage,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationFamily {
    ILinkVoip,
    VoipMt,
}

/// All three protobuf layers remain available; unknown fields are not discarded.
/// No audio/video interpretation or account/call authority is inferred here.
pub struct DecodedCallControl {
    pub family: NotificationFamily,
    pub carrier: NotificationCarrier,
    pub conference: ConferenceNotification,
    pub payload: NotificationPayload,
}

/// Decode a type-10002 call control, leaving type-50 history and every runtime
/// effect to their separate owners. Unknown XML children fail closed, rather
/// than treating a known child as proof the whole control has been handled.
pub fn decode_system_control(message: &AddMessage) -> Result<Option<DecodedCallControl>> {
    if message.message_type != 10002 {
        return Ok(None);
    }
    ensure!(
        message.content.len() <= MAX_SIGNALING_BYTES,
        "Call XML too large"
    );
    let xml = std::str::from_utf8(&message.content)?;
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 128,
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
        "Unsupported call XML shape"
    );
    let root = document.root_element();
    if !ordinary(root, "sysmsg") {
        return Ok(None);
    }
    let family = match root.attribute("type") {
        Some("ilinkvoip") => NotificationFamily::ILinkVoip,
        Some("voipmt") => NotificationFamily::VoipMt,
        _ => return Ok(None),
    };
    ensure!(
        root.attributes().len() == 1,
        "Unsupported call root attributes"
    );
    let container = only_element(root)?;
    ensure!(
        ordinary(container, "voipmt") && container.attributes().len() == 0,
        "Unsupported call container"
    );
    let leaf = only_element(container)?;
    ensure!(
        leaf.attributes().len() == 0 && leaf.children().all(|node| node.is_text()),
        "Unsupported call leaf"
    );
    let kind = match (family, leaf.tag_name().name()) {
        (_, "invite") => CarrierKind::Invite,
        (NotificationFamily::ILinkVoip, "cancel")
        | (NotificationFamily::VoipMt, "cancelinvite") => CarrierKind::Cancel,
        _ => anyhow::bail!("Unsupported call control child"),
    };
    let text: String = leaf.children().filter_map(|node| node.text()).collect();
    let raw = decode_base64(&text)?;
    let carrier = NotificationCarrier::decode(kind, &raw)?;
    let conference = carrier.conference_notification()?;
    let payload = conference.payload()?;
    Ok(Some(DecodedCallControl {
        family,
        carrier,
        conference,
        payload,
    }))
}

fn ordinary(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.tag_name().name() == name && node.tag_name().namespace().is_none()
}

fn only_element<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
) -> Result<roxmltree::Node<'a, 'input>> {
    let mut selected = None;
    for child in node.children() {
        if child.is_element() {
            ensure!(selected.is_none(), "Ambiguous call XML children");
            selected = Some(child);
        } else {
            ensure!(
                child.is_text() && child.text().is_some_and(|text| text.trim().is_empty()),
                "Unsupported call XML content"
            );
        }
    }
    selected.context("Missing call XML child")
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    ensure!(
        !text.is_empty() && text.len() <= MAX_SIGNALING_BYTES,
        "Invalid call carrier size"
    );
    // Standard padded alphabet. Whitespace/URL-safe forms are not normalized.
    Ok(STANDARD.decode(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::protobuf as p;

    fn message(kind: i32, xml: &str) -> AddMessage {
        AddMessage {
            from: "synthetic-peer".into(),
            to: "synthetic-self".into(),
            peer: "synthetic-peer".into(),
            sender_id: None,
            outgoing: false,
            server_message_id: Some("123".into()),
            numbered1: 0,
            message_type: kind,
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

    fn carrier(invite: bool) -> Vec<u8> {
        let inner = p::message(&[
            p::uint(1, 42),
            p::uint(2, 43),
            p::uint(3, 7),
            p::uint(4, 2),
            p::uint(99, 1),
        ])
        .unwrap();
        let third = p::message(&[
            p::uint(1, if invite { 2 } else { 3 }),
            p::bytes(2, &inner),
            p::bytes(90, b"future"),
        ])
        .unwrap();
        p::message(&[p::bytes(if invite { 6 } else { 2 }, &third), p::uint(88, 9)]).unwrap()
    }

    fn xml(family: &str, child: &str, raw: &[u8]) -> String {
        format!(
            "<sysmsg type='{family}'><voipmt><{child}>{}</{child}></voipmt></sysmsg>",
            STANDARD.encode(raw)
        )
    }

    #[test]
    fn invitation_uses_root_family_and_preserves_all_wire_layers() {
        for (name, family) in [
            ("ilinkvoip", NotificationFamily::ILinkVoip),
            ("voipmt", NotificationFamily::VoipMt),
        ] {
            let source = xml(name, "invite", &carrier(true));
            let input = message(10002, &source);
            let control = decode_system_control(&input).unwrap().unwrap();
            assert_eq!(control.family, family);
            assert_eq!(p::scalar(control.carrier.fields(), 88).unwrap(), 9);
            assert_eq!(
                p::data(&control.conference.unknown_fields, 90).unwrap(),
                b"future"
            );
            let NotificationPayload::Invite(invite) = control.payload else {
                panic!("wrong control")
            };
            assert_eq!(invite.room_id, Some(42));
            assert_eq!(invite.room_type, Some(2));
            assert_eq!(p::scalar(&invite.unknown_fields, 99).unwrap(), 1);
            assert_eq!(input.content, source.as_bytes());
            assert!(input.server_sequence.is_none());
        }
    }

    #[test]
    fn cancellation_names_are_specific_to_the_root_family() {
        for (family, child) in [("ilinkvoip", "cancel"), ("voipmt", "cancelinvite")] {
            let control =
                decode_system_control(&message(10002, &xml(family, child, &carrier(false))))
                    .unwrap()
                    .unwrap();
            let NotificationPayload::Cancel(cancel) = control.payload else {
                panic!("wrong control")
            };
            assert_eq!(cancel.room_key, Some(43));
            assert_eq!(cancel.reason, Some(2));
        }
        for (family, child) in [
            ("voipmt", "cancel"),
            ("ilinkvoip", "cancelinvite"),
            ("voipmt", "banner"),
            ("ilinkvoip", "tipsbar"),
        ] {
            assert!(
                decode_system_control(&message(10002, &xml(family, child, &carrier(false))))
                    .is_err()
            );
        }
    }

    #[test]
    fn history_and_other_controls_are_never_promoted_to_call_invites() {
        let valid = xml("ilinkvoip", "invite", &carrier(true));
        assert!(decode_system_control(&message(50, &valid))
            .unwrap()
            .is_none());
        assert!(
            decode_system_control(&message(10002, &valid.replace("ilinkvoip", "unhandled")))
                .unwrap()
                .is_none()
        );
        assert!(
            decode_system_control(&message(10002, "<msg type='ilinkvoip'/>"))
                .unwrap()
                .is_none()
        );
        assert!(decode_system_control(&message(
            10002,
            &valid.replace("sysmsg type='ilinkvoip'", "sysmsg")
        ))
        .unwrap()
        .is_none());
        assert!(decode_system_control(&message(
            10002,
            &xml("ilinkvoip", "invite", &carrier(false))
        ))
        .is_err());
    }

    #[test]
    fn base64_is_standard_padded_and_does_not_trim() {
        assert_eq!(decode_base64("//8=").unwrap(), vec![255, 255]);
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        for invalid in ["__8=", "Zg", "Zg===", " Zg==", "Zg==\n", "Zh==", ""] {
            assert!(decode_base64(invalid).is_err(), "accepted {invalid:?}");
        }
        let raw = carrier(true);
        let encoded = STANDARD.encode(&raw);
        let cdata = format!("<sysmsg type='ilinkvoip'><voipmt><invite><![CDATA[{encoded}]]></invite></voipmt></sysmsg>");
        assert!(decode_system_control(&message(10002, &cdata))
            .unwrap()
            .is_some());
    }

    #[test]
    fn malformed_ambiguous_extended_and_oversized_xml_fail_closed() {
        let valid = xml("ilinkvoip", "invite", &carrier(true));
        let invalid = [
            format!("<!DOCTYPE sysmsg [<!ENTITY x SYSTEM 'file:///private'>]>{valid}"),
            valid.replace("<sysmsg ", "<sysmsg xmlns='urn:unknown' "),
            valid.replace("<sysmsg ", "<sysmsg extra='1' "),
            valid.replace("<voipmt>", "<voipmt type='ilinkvoip'>"),
            valid.replace("</voipmt>", "<tipsbar>opaque</tipsbar></voipmt>"),
            valid.replace("</invite>", "<unknown/></invite>"),
            valid.replace("</sysmsg>", "</sysmsg><sysmsg/>"),
            valid.replace("<invite>", "<invite><!--comment-->"),
            valid.replace("<invite>", "<invite><?unknown value?>"),
        ];
        for source in invalid {
            assert!(decode_system_control(&message(10002, &source)).is_err());
        }
        assert!(
            decode_system_control(&message(10002, &"x".repeat(MAX_SIGNALING_BYTES + 1))).is_err()
        );
        let many = format!(
            "<sysmsg type='ilinkvoip'><voipmt>{}</voipmt></sysmsg>",
            "<invite/>".repeat(129)
        );
        assert!(decode_system_control(&message(10002, &many)).is_err());
    }
}
