//! Quoted app-message metadata. Callers resolve the selected message and persist
//! this reference and the final XML before submitting a send attempt.
use crate::{protobuf as p, wire::base_request, AccountState};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const MAX_QUOTE_CONTENT_BYTES: usize = 128 * 1024;
pub const MAX_QUOTE_XML_BYTES: usize = 256 * 1024;

/// A reference to the immediate selected message. App messages retain raw type
/// 49 here; their application subtype belongs in their serialized content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteReference {
    pub message_type: i32,
    pub server_id: String,
    pub from_user: String,
    pub chat_user: String,
    pub display_name: String,
    pub content: String,
    pub created_at: u32,
    pub msg_source: Option<String>,
}

fn valid_text(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.is_empty())
        && value.len() <= max
        && value.chars().all(|c| {
            (!c.is_control() || matches!(c, '\t' | '\n' | '\r'))
                && !matches!(c, '\u{fffe}' | '\u{ffff}')
        })
}

fn identity(value: &str, empty: bool) -> bool {
    valid_text(value, 256, empty)
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

impl QuoteReference {
    pub fn validate(&self) -> Result<()> {
        let server_id = self.server_id.parse::<u64>();
        ensure!(
            matches!(self.message_type, 1 | 3 | 34 | 42 | 43 | 47 | 48 | 49)
                && server_id.is_ok_and(|value| value > 0 && value.to_string() == self.server_id)
                && self.created_at > 0
                && self.created_at <= i32::MAX as u32
                && identity(&self.from_user, false)
                && identity(&self.chat_user, true)
                && valid_text(&self.display_name, 16 * 1024, true)
                && valid_text(&self.content, MAX_QUOTE_CONTENT_BYTES, false)
                && self.msg_source.as_deref().is_none_or(|value| valid_text(
                    value,
                    16 * 1024,
                    true
                )),
            "invalid_quote_reference"
        );
        Ok(())
    }
}

fn tag(out: &mut String, name: &str, value: &str) {
    out.push('<');
    out.push_str(name);
    out.push('>');
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// Build an appmsg fragment, followed by fromusername, as required by the
/// app-message carrier. Embedded source XML remains text inside content.
pub fn quote_xml(sender: &str, text: &str, reference: &QuoteReference) -> Result<String> {
    reference.validate()?;
    ensure!(
        identity(sender, false) && !text.trim().is_empty() && valid_text(text, 16 * 1024, false),
        "invalid_quote_request"
    );
    let mut out = String::from("<appmsg appid=\"\" sdkver=\"0\">");
    for (name, value) in [
        ("title", text),
        ("des", ""),
        ("action", ""),
        ("type", "57"),
        ("showtype", "0"),
        ("url", ""),
        ("lowurl", ""),
    ] {
        tag(&mut out, name, value);
    }
    out.push_str("<refermsg>");
    tag(&mut out, "type", &reference.message_type.to_string());
    tag(&mut out, "svrid", &reference.server_id);
    tag(&mut out, "fromusr", &reference.from_user);
    tag(&mut out, "chatusr", &reference.chat_user);
    tag(&mut out, "displayname", &reference.display_name);
    tag(&mut out, "content", &reference.content);
    tag(&mut out, "createtime", &reference.created_at.to_string());
    if let Some(source) = reference
        .msg_source
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        tag(&mut out, "msgsource", source);
    }
    out.push_str("</refermsg></appmsg>");
    tag(&mut out, "fromusername", sender);
    ensure!(out.len() <= MAX_QUOTE_XML_BYTES, "invalid_quote_request");
    Ok(out)
}

/// Submit the persisted quote XML with the reply subtype in AppMsg.Type.
/// Reference MsgSource inside the XML is separate from outgoing MsgSource.
pub fn quote_send_request(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    created_at: u32,
    content: &str,
) -> Result<Vec<u8>> {
    quote_send_request_with_source(state, destination, client_id, created_at, content, None)
}

/// Outgoing source belongs to this reply; embedded reference source stays in XML.
pub fn quote_send_request_with_source(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    created_at: u32,
    content: &str,
    source: Option<&str>,
) -> Result<Vec<u8>> {
    ensure!(
        identity(&state.own_username, false)
            && identity(destination, false)
            && valid_text(client_id, 1024, false)
            && !client_id.chars().any(char::is_control)
            && created_at > 0
            && valid_text(content, MAX_QUOTE_XML_BYTES, false)
            && source.is_none_or(|value| {
                destination.ends_with("@chatroom") && valid_text(value, 64 * 1024, false)
            }),
        "invalid_quote_request"
    );
    let mut fields = vec![
        p::bytes(1, state.own_username.as_bytes()),
        p::bytes(2, &[]),
        p::uint(3, 0),
        p::bytes(4, destination.as_bytes()),
        p::uint(5, 57),
        p::bytes(6, content.as_bytes()),
        p::uint(7, u64::from(created_at)),
        p::bytes(8, client_id.as_bytes()),
        p::int32(10, 1),
    ];
    if let Some(source) = source {
        fields.push(p::bytes(12, source.as_bytes()));
    }
    fields.extend([p::bytes(13, &[]), p::bytes(14, &[]), p::bytes(15, &[])]);
    let app = p::message(&fields)?;
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &app),
        // The supported AppMsg builder includes this field even when no
        // attachment checksum is present.
        p::bytes(5, &[]),
        p::uint(9, 0),
        p::uint(10, 0),
        p::uint(11, 2),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> QuoteReference {
        QuoteReference {
            message_type: 49,
            server_id: u64::MAX.to_string(),
            from_user: "example@chatroom".into(),
            chat_user: "quoted-user".into(),
            display_name: "Someone <A>".into(),
            content: "<msg><appmsg><type>6</type><title>report & plan</title></appmsg></msg>"
                .into(),
            created_at: 1234566,
            msg_source: Some("<msgsource><flag>1</flag></msgsource>".into()),
        }
    }

    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: crate::DeviceProfile {
                device_id: vec![1; 16],
                device_type: "UnifiedPCLinux synthetic x86_64".into(),
                client_version: -227271398,
            },
            uin: 123,
            own_username: "synthetic-user".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![],
            response_key: vec![],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }

    #[test]
    fn quote_fields_and_wire_match_independent_fixture() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/synthetic-quote-wire.json")).unwrap();
        let xml = quote_xml("synthetic-user", "Reply <&> \"yes\"\n世界", &reference()).unwrap();
        assert_eq!(xml, fixture["xml"]);
        let request =
            quote_send_request(&state(), "example@chatroom", "quote-client", 1234567, &xml)
                .unwrap();
        assert_eq!(hex::encode(&request), fixture["requestHex"]);
        let outer = p::decode(&request).unwrap();
        assert_eq!(
            outer.iter().map(|field| field.number).collect::<Vec<_>>(),
            [1, 2, 5, 9, 10, 11]
        );
        assert_eq!(p::data(&outer, 5).unwrap(), b"");
        let app = p::decode(p::data(&outer, 2).unwrap()).unwrap();
        assert_eq!(p::scalar(&app, 5).unwrap(), 57);
        assert_eq!(p::data(&app, 6).unwrap(), xml.as_bytes());
        assert!(p::optional_data(&app, 12).unwrap().is_none());
        assert!(xml.contains("<msgsource>&lt;msgsource&gt;"));
        assert!(xml.contains("<refermsg><type>49</type>"));
        assert!(!xml.contains("<refermsg><type>6</type>"));
    }

    #[test]
    fn content_is_encoded_as_text_without_expanding_nested_markup() {
        let mut selected = reference();
        selected.message_type = 1;
        selected.chat_user.clear();
        selected.display_name.clear();
        selected.msg_source = Some(String::new());
        selected.content = "literal </content><type>10002</type> &amp; ]]> ' \"".into();
        let xml = quote_xml("synthetic-user", "comment", &selected).unwrap();
        assert!(xml.contains("&lt;/content&gt;&lt;type&gt;10002&lt;/type&gt;"));
        assert!(xml.contains("&amp;amp; ]]&gt; &apos; &quot;"));
        assert!(!xml.contains("<type>10002</type>"));
        assert!(!xml.contains("<msgsource>"));
        assert!(xml.contains("<chatusr></chatusr><displayname></displayname>"));
    }

    #[test]
    fn outgoing_mentions_use_app_source_independently_of_embedded_reference_source() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../fixtures/synthetic-quoted-mentions-wire.json"
        ))
        .unwrap();
        let mut selected = reference();
        selected.msg_source =
            Some("<msgsource><atuserlist>old_member</atuserlist></msgsource>".into());
        let xml = quote_xml("synthetic-user", "@Current\u{2005}Reply", &selected).unwrap();
        assert_eq!(xml, fixture["xml"]);
        let source = "<msgsource><atuserlist>current_member</atuserlist><alnode><fr>1</fr></alnode></msgsource>";
        let request = quote_send_request_with_source(
            &state(),
            "example@chatroom",
            "quote-client",
            1234567,
            &xml,
            Some(source),
        )
        .unwrap();
        assert_eq!(source, fixture["outgoingSource"]);
        assert_eq!(hex::encode(&request), fixture["requestHex"]);
        let outer = p::decode(&request).unwrap();
        assert_eq!(
            outer.iter().map(|field| field.number).collect::<Vec<_>>(),
            [1, 2, 5, 9, 10, 11]
        );
        let app = p::decode(p::data(&outer, 2).unwrap()).unwrap();
        assert_eq!(p::scalar(&app, 5).unwrap(), 57);
        assert_eq!(p::data(&app, 12).unwrap(), source.as_bytes());
        let body = std::str::from_utf8(p::data(&app, 6).unwrap()).unwrap();
        assert!(body.contains("old_member"));
        assert!(!body.contains("current_member"));
        assert!(body.contains("<title>@Current\u{2005}Reply</title>"));
        for invalid in ["", "bad\0source"] {
            assert!(quote_send_request_with_source(
                &state(),
                "example@chatroom",
                "client",
                1,
                &xml,
                Some(invalid)
            )
            .is_err());
        }
        assert!(
            quote_send_request_with_source(&state(), "peer", "client", 1, &xml, Some(source))
                .is_err()
        );
    }

    #[test]
    fn reference_validation_rejects_lossy_or_unsupported_values() {
        for id in ["0", "01", "+1", "18446744073709551616", "1\n"] {
            let mut selected = reference();
            selected.server_id = id.into();
            assert!(selected.validate().is_err(), "{id:?}");
        }
        for kind in [0, -1, 2, 57, 10002] {
            let mut selected = reference();
            selected.message_type = kind;
            assert!(selected.validate().is_err());
        }
        for control in ['\0', '\u{1}', '\u{fffe}', '\u{ffff}'] {
            let mut selected = reference();
            selected.content.push(control);
            assert!(selected.validate().is_err());
            assert!(quote_xml("synthetic-user", &control.to_string(), &reference()).is_err());
        }
        let mut selected = reference();
        selected.created_at = 0;
        assert!(selected.validate().is_err());
        selected = reference();
        selected.from_user.clear();
        assert!(selected.validate().is_err());
        for value in ["user name", "user\u{a0}name"] {
            selected = reference();
            selected.from_user = value.into();
            assert!(selected.validate().is_err());
            assert!(quote_xml(value, "comment", &reference()).is_err());
            assert!(quote_send_request(&state(), value, "client", 1, "xml").is_err());
        }
        selected = reference();
        selected.msg_source = Some("x".repeat(16 * 1024 + 1));
        assert!(selected.validate().is_err());
    }

    #[test]
    fn final_escaped_xml_and_request_size_remain_bounded() {
        let mut selected = reference();
        selected.content = "x".repeat(MAX_QUOTE_CONTENT_BYTES);
        assert!(quote_xml("synthetic-user", "comment", &selected).is_ok());
        selected.content.push('x');
        assert!(selected.validate().is_err());
        selected.content = "&".repeat(MAX_QUOTE_CONTENT_BYTES);
        assert!(selected.validate().is_ok());
        assert!(quote_xml("synthetic-user", "comment", &selected).is_err());
        assert!(quote_xml("synthetic-user", " \n\t", &reference()).is_err());
        assert!(quote_xml("synthetic-user", &"x".repeat(16 * 1024 + 1), &reference()).is_err());
        assert!(quote_send_request(&state(), "dest\0", "client", 1, "xml").is_err());
        assert!(quote_send_request(&state(), "dest", "client\n", 1, "xml").is_err());
        assert!(quote_send_request(&state(), "dest", "client", 0, "xml").is_err());
        assert!(quote_send_request(
            &state(),
            "dest",
            "client",
            1,
            &"x".repeat(MAX_QUOTE_XML_BYTES + 1)
        )
        .is_err());
    }
}
