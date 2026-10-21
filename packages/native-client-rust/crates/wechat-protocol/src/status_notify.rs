//! One-attempt native read-session notifications for the supported profile.
//!
//! This reports an entire conversation through a caller-frozen boundary. It is
//! deliberately separate from an exact set of locally read message IDs. The
//! caller must journal admission and an uncertain outcome before retrying could
//! be considered; an I/O error is never permission to submit it again.
use super::*;

const CGI: u16 = 251;
const PATH: &str = "/cgi-bin/micromsg-bin/statusnotify";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionReadNotification {
    pub peer_username: String,
    pub last_message_server_id: String,
    pub last_message_create_time: u32,
    /// A stable request identity retained with the durable frozen boundary.
    pub client_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReadReply {
    pub service_code: i32,
    pub accepted: bool,
}

fn bounded_identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn request(state: &AccountState, notification: &SessionReadNotification) -> Result<Vec<u8>> {
    state.validate()?;
    ensure!(
        bounded_identity(&state.own_username, 256)
            && bounded_identity(&notification.peer_username, 512)
            && bounded_identity(&notification.client_message_id, 256),
        "Invalid read notification identity"
    );
    let id = notification.last_message_server_id.parse::<u64>();
    ensure!(
        id.is_ok_and(|id| id != 0 && id.to_string() == notification.last_message_server_id)
            && notification.last_message_create_time != 0,
        "Invalid read notification boundary"
    );
    let argument = serde_json::to_vec(&serde_json::json!({
        "messageSvrId": notification.last_message_server_id,
        "MsgCreateTime": notification.last_message_create_time.to_string(),
    }))?;
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::uint(2, 1),
        p::bytes(3, state.own_username.as_bytes()),
        p::bytes(4, notification.peer_username.as_bytes()),
        p::bytes(5, notification.client_message_id.as_bytes()),
        p::bytes(
            8,
            &p::message(&[p::bytes(1, b"lastMessage"), p::bytes(2, &argument)])?,
        ),
    ])
}

fn response(bytes: &[u8]) -> Result<SessionReadReply> {
    let fields = p::decode(bytes)?;
    let base = p::decode(p::data(&fields, 1)?)?;
    let raw = p::scalar(&base, 1)?;
    let code = raw as u32 as i32;
    ensure!(
        raw <= u32::MAX as u64 || raw == code as i64 as u64,
        "Invalid read notification response status"
    );
    Ok(SessionReadReply {
        service_code: code,
        accepted: code == 0,
    })
}

impl NativeClient {
    /// Submit once, with no renewal, retry, or login fallback. A caller must
    /// retain the boundary and request identity before invoking this method.
    pub fn notify_read_session(
        &self,
        state: &mut AccountState,
        notification: &SessionReadNotification,
    ) -> Result<SessionReadReply> {
        let body = request(state, notification)?;
        response(&self.authenticated_rpc(state, CGI, PATH, &body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: b"0123456789abcdef".to_vec(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 42,
            own_username: "synthetic_user".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![4; 16],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }

    fn notification() -> SessionReadNotification {
        SessionReadNotification {
            peer_username: "synthetic_room@chatroom".into(),
            last_message_server_id: u64::MAX.to_string(),
            last_message_create_time: 1_800_000_000,
            client_message_id: "read-fixture-stable-id".into(),
        }
    }

    #[test]
    fn read_session_uses_proven_fields_and_lossless_boundary_strings() {
        assert_eq!(CGI, 251);
        assert_eq!(PATH, "/cgi-bin/micromsg-bin/statusnotify");
        let input = notification();
        let body = request(&state(), &input).unwrap();
        assert_eq!(body, request(&state(), &input).unwrap());
        let outer = p::decode(&body).unwrap();
        assert_eq!(
            outer.iter().map(|field| field.number).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 8]
        );
        let base = p::decode(p::data(&outer, 1).unwrap()).unwrap();
        assert_eq!(p::scalar(&base, 2).unwrap(), 42);
        assert_eq!(p::scalar(&outer, 2).unwrap(), 1);
        assert_eq!(p::data(&outer, 3).unwrap(), b"synthetic_user");
        assert_eq!(p::data(&outer, 4).unwrap(), b"synthetic_room@chatroom");
        assert_eq!(p::data(&outer, 5).unwrap(), b"read-fixture-stable-id");
        let extra = p::decode(p::data(&outer, 8).unwrap()).unwrap();
        assert_eq!(p::data(&extra, 1).unwrap(), b"lastMessage");
        let argument: serde_json::Value =
            serde_json::from_slice(p::data(&extra, 2).unwrap()).unwrap();
        assert_eq!(argument["messageSvrId"], u64::MAX.to_string());
        assert_eq!(argument["MsgCreateTime"], "1800000000");
    }

    #[test]
    fn malformed_boundaries_and_identities_fail_before_transport() {
        for id in ["", "0", "-1", "01", " 1", "+1", "18446744073709551616"] {
            let mut input = notification();
            input.last_message_server_id = id.into();
            assert!(request(&state(), &input).is_err());
        }
        for value in [String::new(), "peer\nother".into(), "x".repeat(513)] {
            let mut input = notification();
            input.peer_username = value;
            assert!(request(&state(), &input).is_err());
        }
        let mut input = notification();
        input.last_message_create_time = 0;
        assert!(request(&state(), &input).is_err());
        let mut input = notification();
        input.client_message_id.clear();
        assert!(request(&state(), &input).is_err());
        let mut invalid = state();
        invalid.uin = 0;
        assert!(request(&invalid, &notification()).is_err());
    }

    #[test]
    fn reply_preserves_rejection_and_requires_unique_initialized_base_status() {
        assert!(response(&p::bytes(1, &p::uint(1, 0))).unwrap().accepted);
        let rejected = response(&p::bytes(1, &p::int32(1, -13))).unwrap();
        assert_eq!(rejected.service_code, -13);
        assert!(!rejected.accepted);
        for invalid in [
            vec![],
            p::bytes(1, &[]),
            p::uint(1, 0),
            p::bytes(1, &p::uint(1, 1u64 << 32)),
            [p::bytes(1, &p::uint(1, 0)), p::bytes(1, &p::uint(1, 0))].concat(),
        ] {
            assert!(response(&invalid).is_err());
        }
    }
}
