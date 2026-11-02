//! Call-specific ordinary reply policy and non-sensitive transport diagnostics.
//! A verified host transport may return either owned ordinary key mode. This
//! does not change request selection or authorize Ax/app/media key substitution.

use crate::{business, crypto, AccountState, MAX_MESSAGE_BYTES};
use openssl::symm::Mode;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallHostStage {
    Route,
    Handshake,
    Prepare,
    Exchange,
    Decode,
}
impl CallHostStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Handshake => "handshake",
            Self::Prepare => "prepare",
            Self::Exchange => "exchange",
            Self::Decode => "decode",
        }
    }
}

/// Only fixed labels and optional numeric protocol/status codes are retained.
/// Underlying error text, hosts, response bytes and keys are never included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallHostRpcError {
    pub stage: CallHostStage,
    pub code: &'static str,
    pub value: Option<i64>,
}
impl std::fmt::Display for CallHostRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Call host {} failed: {}", self.stage.as_str(), self.code)?;
        if let Some(value) = self.value {
            write!(f, " ({value})")?;
        }
        Ok(())
    }
}
impl std::error::Error for CallHostRpcError {}
impl CallHostRpcError {
    pub(crate) fn new(stage: CallHostStage, code: &'static str, value: Option<i64>) -> Self {
        Self { stage, code, value }
    }
    /// False only when the call request is known not to have been submitted.
    /// Exchange/decode failures remain uncertain, including connection timeouts.
    pub fn may_have_submitted(&self) -> bool {
        matches!(self.stage, CallHostStage::Exchange | CallHostStage::Decode)
            && self.code != "unsupported_route"
    }
    pub(crate) fn from_cause(stage: CallHostStage, cause: &anyhow::Error) -> Self {
        let mut result = Self::new(stage, "rejected", None);
        if cause.is::<crate::mmtls::UnsupportedNativeRoute>() {
            result.code = "unsupported_route";
        } else if let Some(http) = cause.downcast_ref::<reqwest::Error>() {
            result.code = if http.is_timeout() {
                "timeout"
            } else if http.is_connect() {
                "connect"
            } else if let Some(status) = http.status() {
                result.value = Some(i64::from(status.as_u16()));
                "http_status"
            } else {
                "http_transport"
            };
        } else if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            result.code = match io.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timeout",
                std::io::ErrorKind::ConnectionRefused => "connection_refused",
                std::io::ErrorKind::ConnectionReset => "connection_reset",
                std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                std::io::ErrorKind::ConnectionAborted => "connection_aborted",
                std::io::ErrorKind::NetworkUnreachable => "network_unreachable",
                std::io::ErrorKind::HostUnreachable => "host_unreachable",
                std::io::ErrorKind::AddrNotAvailable => "address_unavailable",
                std::io::ErrorKind::BrokenPipe => "broken_pipe",
                _ => "io",
            };
        } else if cause.is::<openssl::error::ErrorStack>() {
            result.code = "openssl_crypto";
        } else if let Some((code, value)) = cause
            .chain()
            .find_map(|error| known_transport_literal(&error.to_string()))
        {
            result.code = code;
            result.value = value;
        }
        result
    }
}

// Only exact messages emitted by the maintained session/framing/crypto checks
// are admitted. Formatting unknown errors is transient; none of that text is
// returned or persisted. Context strings and payload-bearing suffixes cannot
// become diagnostic labels.
fn known_transport_literal(message: &str) -> Option<(&'static str, Option<i64>)> {
    let code = match message {
        "Unsupported native server host" => "host_policy",
        "Invalid receive budget" => "receive_budget_configuration",
        "Invalid MMTLS timeout" => "timeout_configuration",
        "Native connection failed" => "connect_failed",
        "MMTLS deadline exceeded" => "deadline",
        "MMTLS connection closed" => "connection_closed",
        "Unexpected MMTLS header" => "record_header",
        "Receive overflow" | "MMTLS receive budget exceeded" => "receive_budget",
        "Server returned MMTLS alert" => "server_alert",
        "Expected handshake" => "expected_server_hello",
        "Unsupported ServerHello" => "server_hello_selection",
        "Unoffered key" => "unoffered_key",
        "Duplicate extension" => "duplicate_extension",
        "Invalid ECDH point" => "ecdh_point",
        "Missing ECDH selection" => "missing_ecdh_selection",
        "Missing certificate selection" => "missing_certificate_selection",
        "Expected signature record" => "expected_signature_record",
        "Unsupported MMTLS certificate version" => "certificate_version",
        "Unoffered MMTLS pin" => "unoffered_pin",
        "Unsupported signature pin curve" => "signature_curve",
        "Pinned server signature verification failed" => "signature_verification",
        "Expected ticket record" => "expected_ticket_record",
        "Expected tickets" => "expected_tickets",
        "Invalid ticket count" => "ticket_count",
        "Invalid shortlink ticket" => "ticket_shape",
        "Expected Finished" => "expected_finished_record",
        "Server Finished verification failed" => "finished_verification",
        "Expected NOOP reply" => "expected_noop_record",
        "Unexpected NOOP reply" => "noop_reply",
        "Truncated structure" => "truncated_structure",
        "Integer too wide" => "integer_width",
        "Trailing structure bytes" => "trailing_structure",
        "Unexpected handshake message" => "handshake_message",
        "Missing authentication tag" => "authentication_tag_missing",
        "Invalid IV" | "Invalid GCM nonce" => "iv_shape",
        "Invalid GCM key" | "Expected AES192 key" => "key_shape",
        "Invalid GCM envelope" => "gcm_envelope",
        "Invalid HKDF size" => "hkdf_size",
        "Invalid MMTLS record" => "record_shape",
        "Section too large" | "MMTLS plaintext too large" | "Payload too large" => "request_size",
        "Shortlink receive budget exceeded" | "Shortlink plaintext too large" => {
            "shortlink_reply_size"
        }
        "Invalid shortlink header" => "shortlink_header",
        "Invalid shortlink sequence" => "shortlink_sequence",
        "Unsupported shortlink selection" => "shortlink_selection",
        "Unsupported shortlink extensions" => "shortlink_extensions",
        "Shortlink Finished failed" => "shortlink_finished",
        "Unexpected shortlink close" => "shortlink_close",
        "Invalid longlink envelope" => "longlink_envelope",
        _ => {
            let value = message.strip_prefix("Native shortlink HTTP status ")?;
            if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let status = value.parse::<u16>().ok()?;
            return (100..=599)
                .contains(&status)
                .then_some(("http_status", Some(i64::from(status))));
        }
    };
    Some((code, None))
}
fn decode_error(code: &'static str, value: Option<i64>) -> CallHostRpcError {
    CallHostRpcError::new(CallHostStage::Decode, code, value)
}

/// Only call after authenticated MMTLS has verified these bytes. Keys belong to
/// the same exclusively borrowed account state used to prepare this request;
/// account/logout generation admission remains the embedding actor's job.
/// Cookies are returned only after all envelope, decryption and length checks.
pub(crate) fn decode_verified_reply(
    state: &AccountState,
    cgi: u16,
    input: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), CallHostRpcError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(decode_error("reply_size", None));
    }
    let envelope = business::split(input).map_err(|_| decode_error("envelope", None))?;
    if envelope.return_code != 0 {
        return Err(decode_error(
            "business_return",
            Some(i64::from(envelope.return_code)),
        ));
    }
    if envelope.cgi != u64::from(cgi) {
        return Err(decode_error("command", None));
    }
    if ![1, 2].contains(&envelope.compression) {
        return Err(decode_error(
            "compression",
            Some(i64::from(envelope.compression)),
        ));
    }
    let body = Zeroizing::new(
        match envelope.encryption {
            13 if state.response_key.len() == 24 => {
                crypto::envelope_open(&envelope.payload, &state.response_key, b"")
            }
            5 if state.session_key.len() == 16 => {
                crypto::cbc(&envelope.payload, &state.session_key, Mode::Decrypt)
            }
            5 | 13 => {
                return Err(decode_error(
                    "response_key",
                    Some(i64::from(envelope.encryption)),
                ))
            }
            mode => return Err(decode_error("encryption_mode", Some(i64::from(mode)))),
        }
        .map_err(|_| decode_error("decrypt", Some(i64::from(envelope.encryption))))?,
    );
    if body.len() != envelope.pre_encryption_length {
        return Err(decode_error("decrypted_length", None));
    }
    let plain = Zeroizing::new(if envelope.compression == 1 {
        crypto::inflate(&body).map_err(|_| decode_error("inflate", None))?
    } else {
        body.to_vec()
    });
    if plain.len() != envelope.original_length {
        return Err(decode_error("plaintext_length", None));
    }
    Ok((plain.to_vec(), envelope.cookie))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: crate::DeviceProfile {
                device_id: vec![b'x'; 16],
                device_type: "Linux".into(),
                client_version: -227271398,
            },
            uin: 1,
            own_username: "synthetic".into(),
            cookie: vec![],
            session_key: vec![7; 16],
            request_key: vec![8; 24],
            response_key: vec![9; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: crate::AuthSnapshot::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    // Response-header construction is independent of the request packer. Its
    // five varints follow CGI, original size, encrypted-plain size, reserveds.
    fn reply(state: &AccountState, cgi: u16, mode: u8, compressed: bool) -> Vec<u8> {
        let plain = b"synthetic call reply";
        let body = if compressed {
            crypto::deflate(plain).unwrap()
        } else {
            plain.to_vec()
        };
        let cipher = match mode {
            13 => crypto::envelope_seal(&body, &state.response_key, b"", [3; 12]).unwrap(),
            _ => crypto::cbc(&body, &state.session_key, Mode::Encrypt).unwrap(),
        };
        let cookie = b"new";
        let mut tail = Vec::new();
        for n in [u64::from(cgi), plain.len() as u64, body.len() as u64, 0, 0] {
            tail.extend(crate::protobuf::varint(n));
        }
        let size = 10 + cookie.len() + tail.len();
        assert!(size < 64);
        let flags = (u16::from(mode) << 12)
            | ((cookie.len() as u16) << 8)
            | ((size as u16) << 2)
            | if compressed { 1 } else { 2 };
        let mut raw = flags.to_le_bytes().to_vec();
        raw.extend(0i32.to_be_bytes());
        raw.extend(state.uin.to_be_bytes());
        raw.extend(cookie);
        raw.extend(tail);
        raw.extend(cipher);
        raw
    }
    #[test]
    fn call_reply_mode_is_independent_while_generic_policy_stays_strict() {
        let mut s = state();
        let generic = business::AuthenticatedPacket::prepare(&s, 7994, b"request").unwrap();
        let origin_reply = reply(&s, 7994, 5, false);
        assert!(generic
            .decode_verified_transport_reply(&origin_reply)
            .is_err());
        assert_eq!(
            decode_verified_reply(&s, 7994, &origin_reply).unwrap(),
            (b"synthetic call reply".to_vec(), b"new".to_vec())
        );
        for compressed in [false, true] {
            assert_eq!(
                decode_verified_reply(&s, 7994, &reply(&s, 7994, 13, compressed))
                    .unwrap()
                    .0,
                b"synthetic call reply"
            );
        }
        s.request_key.clear();
        s.response_key.clear();
        let generic_origin = business::AuthenticatedPacket::prepare(&s, 7994, b"request").unwrap();
        let main_reply = reply(&state(), 7994, 13, false);
        assert!(generic_origin
            .decode_verified_transport_reply(&main_reply)
            .is_err());
        assert_eq!(
            decode_verified_reply(&s, 7994, &main_reply)
                .unwrap_err()
                .code,
            "response_key"
        );
    }
    #[test]
    fn wrong_owned_keys_commands_modes_and_lengths_do_not_admit_cookie() {
        let s = state();
        for mode in [5, 13] {
            let packet = reply(&s, 7994, mode, false);
            let mut wrong = state();
            wrong.session_key.fill(0);
            wrong.response_key.fill(0);
            assert_eq!(
                decode_verified_reply(&wrong, 7994, &packet)
                    .unwrap_err()
                    .code,
                "decrypt"
            );
        }
        assert_eq!(
            decode_verified_reply(&s, 8069, &reply(&s, 7994, 5, false))
                .unwrap_err()
                .code,
            "command"
        );
        assert_eq!(
            decode_verified_reply(&s, 7994, &reply(&s, 7994, 11, false))
                .unwrap_err()
                .value,
            Some(11)
        );
        let mut short = reply(&s, 7994, 13, false);
        short.pop();
        assert_eq!(
            decode_verified_reply(&s, 7994, &short).unwrap_err().code,
            "decrypt"
        );
    }
    #[test]
    fn diagnostics_never_copy_underlying_error_text() {
        let error = CallHostRpcError::from_cause(
            CallHostStage::Handshake,
            &anyhow::anyhow!("synthetic-private-key-and-body"),
        );
        assert_eq!(error.to_string(), "Call host handshake failed: rejected");
        let timeout = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert_eq!(
            CallHostRpcError::from_cause(CallHostStage::Exchange, &timeout).code,
            "timeout"
        );
        assert!(
            CallHostRpcError::from_cause(CallHostStage::Exchange, &timeout).may_have_submitted()
        );
        assert!(!error.may_have_submitted());
        let rejected = crate::mmtls::validate_short_route("/bad").unwrap_err();
        let diagnostic = CallHostRpcError::from_cause(CallHostStage::Exchange, &rejected);
        assert_eq!(diagnostic.code, "unsupported_route");
        assert!(!diagnostic.may_have_submitted());
    }
}
