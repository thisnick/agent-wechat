//! Read one exact OpenIM contact through the supported native contact operation.
use crate::{protobuf as p, AccountState, NativeClient};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

const CGI: u16 = 881;
const PATH: &str = "/cgi-bin/micromsg-bin/getopenimcontact";
const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_BYTES: usize = 4096;
const MAX_URL_BYTES: usize = 16384;
const MAX_TICKET_BYTES: usize = 16384;

/// Supplied contact fields for one accepted OpenIM identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenImContact {
    /// Exact OpenIM username returned for the requested identity.
    pub username: String,
    /// Contact's nickname, preserving absent and present-empty values.
    pub nickname: Option<String>,
    /// Saved remark supplied by the service, when present.
    pub remark: Option<String>,
    /// Server-advertised large-avatar source. Validate it before any download.
    pub large_avatar_url: Option<String>,
    /// Server-advertised small-avatar source. Validate it before any download.
    pub small_avatar_url: Option<String>,
}

/// A service rejection retaining the signed OpenIM response code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenImServiceError {
    /// Signed `ret` returned by the OpenIM service.
    pub service_code: i32,
}

impl std::fmt::Display for OpenImServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "OpenIM service returned {}", self.service_code)
    }
}

impl std::error::Error for OpenImServiceError {}

fn identity(value: &str, suffix: &str) -> bool {
    value.len() <= MAX_ID_BYTES
        && value
            .strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.contains('@'))
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

/// Encode an exact OpenIM lookup with optional ordinary/OpenIM group context and ticket.
pub fn request(username: &str, room: Option<&str>, ticket: Option<&str>) -> Result<Vec<u8>> {
    ensure!(identity(username, "@openim"), "Invalid OpenIM identity");
    if let Some(room) = room {
        ensure!(
            identity(room, "@chatroom") || identity(room, "@im.chatroom"),
            "Invalid OpenIM room context"
        );
    }
    if let Some(ticket) = ticket {
        ensure!(
            ticket.len() <= MAX_TICKET_BYTES && !ticket.contains('\0'),
            "Invalid OpenIM lookup ticket"
        );
    }
    let mut fields = vec![p::bytes(1, username.as_bytes())];
    if let Some(room) = room {
        fields.push(p::bytes(2, room.as_bytes()));
    }
    if let Some(ticket) = ticket {
        fields.push(p::bytes(3, ticket.as_bytes()));
    }
    p::message(&fields)
}

fn optional_text(fields: &[p::Field], number: u32, max: usize) -> Result<Option<String>> {
    p::optional_data(fields, number)?
        .map(|bytes| {
            ensure!(bytes.len() <= max, "OpenIM field exceeds limit");
            let text = std::str::from_utf8(bytes)?;
            ensure!(!text.contains('\0'), "Invalid OpenIM text");
            Ok(text.to_owned())
        })
        .transpose()
}

/// Decode an authenticated OpenIM response and require the exact requested contact identity.
pub fn decode_response(bytes: &[u8], requested_username: &str) -> Result<OpenImContact> {
    ensure!(
        identity(requested_username, "@openim"),
        "Invalid OpenIM identity"
    );
    let fields = p::decode(bytes)?;
    let raw_code = p::scalar(&fields, 1)?;
    let service_code = raw_code as u32 as i32;
    ensure!(
        raw_code <= u64::from(u32::MAX)
            || (service_code < 0 && raw_code == service_code as i64 as u64),
        "Invalid OpenIM service code"
    );
    let raw_contact = p::optional_data(&fields, 2)?;
    if service_code != 0 {
        return Err(OpenImServiceError { service_code }.into());
    }
    let contact = p::decode(raw_contact.ok_or_else(|| anyhow::anyhow!("Missing OpenIM contact"))?)?;
    let username = optional_text(&contact, 1, MAX_ID_BYTES)?
        .ok_or_else(|| anyhow::anyhow!("Missing OpenIM contact identity"))?;
    ensure!(
        username == requested_username,
        "OpenIM contact identity mismatch"
    );
    Ok(OpenImContact {
        username,
        nickname: optional_text(&contact, 2, MAX_LABEL_BYTES)?,
        remark: optional_text(&contact, 4, MAX_LABEL_BYTES)?,
        large_avatar_url: optional_text(&contact, 5, MAX_URL_BYTES)?,
        small_avatar_url: optional_text(&contact, 6, MAX_URL_BYTES)?,
    })
}

impl NativeClient {
    pub(crate) fn openim_contact_raw_snapshot(
        &self,
        state: &mut AccountState,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<Vec<u8>> {
        let request = zeroize::Zeroizing::new(request(username, room, ticket)?);
        self.authenticated_rpc(state, CGI, PATH, &request)
    }

    pub(crate) fn openim_contact_snapshot(
        &self,
        state: &mut AccountState,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<OpenImContact> {
        let reply = self.openim_contact_raw_snapshot(state, username, room, ticket)?;
        decode_response(&reply, username)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "123@openim";

    fn response(fields: &[Vec<u8>]) -> Vec<u8> {
        p::message(&[p::int32(1, 0), p::bytes(2, &p::message(fields).unwrap())]).unwrap()
    }

    #[test]
    fn request_matches_the_direct_string_wire_layout() {
        // Three direct string fields, in field-number order. Optional context
        // and ticket presence is controlled separately from the required identity.
        let expected =
            hex::decode("0a0a313233406f70656e696d120a374063686174726f6f6d1a0174").unwrap();
        assert_eq!(
            request(USER, Some("7@chatroom"), Some("t")).unwrap(),
            expected
        );
        assert_eq!(
            request(USER, None, None).unwrap(),
            hex::decode("0a0a313233406f70656e696d").unwrap()
        );
        assert!(request(USER, Some("7@im.chatroom"), None).is_ok());
        assert_eq!(
            request(USER, None, Some("")).unwrap(),
            hex::decode("0a0a313233406f70656e696d1a00").unwrap()
        );
    }

    #[test]
    fn request_rejects_invalid_identities_contexts_and_oversized_tickets() {
        for user in [
            "",
            "@openim",
            "person",
            "person@kefu.openim",
            "two words@openim",
            "x@y@openim",
        ] {
            assert!(request(user, None, None).is_err());
        }
        assert!(request(&format!("{}@openim", "x".repeat(MAX_ID_BYTES)), None, None).is_err());
        for room in [
            "",
            "@chatroom",
            "person",
            "person@openim",
            "two words@chatroom",
        ] {
            assert!(request(USER, Some(room), None).is_err());
        }
        assert!(request(USER, None, Some(&"x".repeat(MAX_TICKET_BYTES + 1))).is_err());
        assert!(request(USER, None, Some("bad\0ticket")).is_err());
    }

    #[test]
    fn response_reads_supplied_fields_and_preserves_empty_values() {
        let bytes = response(&[
            p::bytes(1, USER.as_bytes()),
            p::bytes(2, "Example member".as_bytes()),
            p::bytes(4, b""),
            p::bytes(5, b"https://wework.qpic.cn/synthetic-large"),
            p::bytes(6, b"https://wework.qpic.cn/synthetic-small"),
            p::uint(99, 1),
            p::uint(99, 2),
        ]);
        let contact = decode_response(&bytes, USER).unwrap();
        assert_eq!(contact.username, USER);
        assert_eq!(contact.nickname.as_deref(), Some("Example member"));
        assert_eq!(contact.remark.as_deref(), Some(""));
        assert_eq!(
            contact.large_avatar_url.as_deref(),
            Some("https://wework.qpic.cn/synthetic-large")
        );
        assert_eq!(
            contact.small_avatar_url.as_deref(),
            Some("https://wework.qpic.cn/synthetic-small")
        );
        let minimal = decode_response(&response(&[p::bytes(1, USER.as_bytes())]), USER).unwrap();
        assert!(
            minimal.nickname.is_none()
                && minimal.remark.is_none()
                && minimal.small_avatar_url.is_none()
        );
    }

    #[test]
    fn response_preserves_signed_service_rejections() {
        for code in [-1, -13, i32::MIN, 1, i32::MAX] {
            let bytes = p::message(&[p::int32(1, code)]).unwrap();
            let error = decode_response(&bytes, USER).unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<OpenImServiceError>()
                    .unwrap()
                    .service_code,
                code
            );
        }
    }

    #[test]
    fn response_rejects_cross_identity_ambiguous_or_malformed_fields() {
        for bytes in [
            Vec::new(),
            p::bytes(2, &p::bytes(1, USER.as_bytes())),
            p::int32(1, 0),
            response(&[]),
            p::message(&[
                p::int32(1, 0),
                p::int32(1, 0),
                p::bytes(2, &p::bytes(1, USER.as_bytes())),
            ])
            .unwrap(),
            p::message(&[p::int32(1, 0), p::bytes(2, b""), p::bytes(2, b"")]).unwrap(),
            p::message(&[p::uint(1, 1u64 << 32), p::bytes(2, b"")]).unwrap(),
            response(&[p::bytes(1, b"456@openim")]),
            response(&[p::bytes(1, USER.as_bytes()), p::bytes(1, USER.as_bytes())]),
            response(&[p::bytes(1, USER.as_bytes()), p::uint(2, 1)]),
            response(&[p::bytes(1, USER.as_bytes()), p::bytes(2, &[0xff])]),
            response(&[
                p::bytes(1, USER.as_bytes()),
                p::bytes(2, &vec![b'x'; MAX_LABEL_BYTES + 1]),
            ]),
            response(&[
                p::bytes(1, USER.as_bytes()),
                p::bytes(4, b"one"),
                p::bytes(4, b"two"),
            ]),
            response(&[
                p::bytes(1, USER.as_bytes()),
                p::bytes(5, &vec![b'x'; MAX_URL_BYTES + 1]),
            ]),
            response(&[
                p::bytes(1, USER.as_bytes()),
                p::bytes(6, b"one"),
                p::bytes(6, b"two"),
            ]),
        ] {
            assert!(decode_response(&bytes, USER).is_err());
        }
    }
}
