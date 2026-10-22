//! One authenticated attempt to recall an exact own message.
//!
//! The Linux 4.1.13.23 AMD64 typed wrapper uses CGI 594. Its builder supplies
//! source local ID/time, sender/destination and the full 64-bit server identity.
//! Callers must persist admission before calling; errors never authorize retry.
use super::*;

const CGI: u16 = 594;
const PATH: &str = "/cgi-bin/micromsg-bin/revokemsg";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageRecall {
    pub peer_username: String,
    pub server_id: String,
    pub local_id: u32,
    pub create_time: u32,
    pub client_message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRecallReply {
    pub service_code: i32,
    pub accepted: bool,
}

impl MessageRecall {
    /// The native nonzero-local-ID formatter is peer_time_localId, with both
    /// numbers rendered as unsigned decimal. Keep it with the frozen target.
    pub fn for_source(
        peer_username: String,
        server_id: String,
        local_id: u32,
        create_time: u32,
    ) -> Self {
        let client_message_id = format!("{peer_username}_{create_time}_{local_id}");
        Self {
            peer_username,
            server_id,
            local_id,
            create_time,
            client_message_id,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let identity = |value: &str, max: usize| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        };
        ensure!(
            identity(&self.peer_username, 512) && identity(&self.client_message_id, 1024),
            "Invalid recall identity"
        );
        ensure!(
            self.local_id != 0 && self.create_time != 0,
            "Invalid recall source"
        );
        ensure!(
            self.server_id
                .parse::<u64>()
                .is_ok_and(|n| n != 0 && n.to_string() == self.server_id),
            "Invalid recall server identity"
        );
        Ok(())
    }
}

fn request(state: &AccountState, recall: &MessageRecall) -> Result<Vec<u8>> {
    state.validate()?;
    recall.validate()?;
    ensure!(
        !state.own_username.is_empty()
            && state.own_username.len() <= 256
            && !state.own_username.chars().any(char::is_control),
        "Invalid recall sender"
    );
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, recall.client_message_id.as_bytes()),
        p::uint(3, u64::from(recall.local_id)),
        p::uint(4, u64::from(recall.create_time)),
        p::uint(5, 0),
        p::bytes(6, state.own_username.as_bytes()),
        p::bytes(7, recall.peer_username.as_bytes()),
        p::uint(8, 0),
        p::uint(9, recall.server_id.parse()?),
    ])
}

fn response(bytes: &[u8]) -> Result<MessageRecallReply> {
    let fields = p::decode(bytes)?;
    let base = p::decode(p::data(&fields, 1)?)?;
    let raw = p::scalar(&base, 1)?;
    let code = raw as u32 as i32;
    ensure!(
        raw <= u32::MAX as u64 || raw == code as i64 as u64,
        "Invalid recall response status"
    );
    // Numbered optional reply strings are not target IDs or success evidence.
    // Keep their unknown semantics out of the public result and logs.
    p::optional_data(&fields, 2)?;
    p::optional_data(&fields, 3)?;
    Ok(MessageRecallReply {
        service_code: code,
        accepted: code == 0,
    })
}

impl NativeClient {
    /// Submit once without credential renewal, login fallback or automatic retry.
    pub fn recall_message(
        &self,
        state: &mut AccountState,
        recall: &MessageRecall,
    ) -> Result<MessageRecallReply> {
        let body = request(state, recall)?;
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
    fn recall() -> MessageRecall {
        MessageRecall::for_source(
            "synthetic_peer".into(),
            u64::MAX.to_string(),
            u32::MAX,
            1_800_000_000,
        )
    }
    #[test]
    fn native_client_identity_uses_exact_peer_time_and_unsigned_local_id() {
        assert_eq!(
            MessageRecall::for_source(
                "synthetic_room@chatroom".into(),
                "20".into(),
                u32::MAX,
                u32::MAX
            )
            .client_message_id,
            "synthetic_room@chatroom_4294967295_4294967295"
        );
        assert_eq!(
            MessageRecall::for_source("peer_with_underscore".into(), "20".into(), 1, 100)
                .client_message_id,
            "peer_with_underscore_100_1"
        );
    }
    #[test]
    fn native_recall_uses_source_fields_and_full_server_identity() {
        assert_eq!(CGI, 594);
        assert_eq!(PATH, "/cgi-bin/micromsg-bin/revokemsg");
        let input = recall();
        let body = request(&state(), &input).unwrap();
        assert_eq!(body, request(&state(), &input).unwrap());
        let f = p::decode(&body).unwrap();
        assert_eq!(
            f.iter().map(|f| f.number).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        assert_eq!(
            p::scalar(&p::decode(p::data(&f, 1).unwrap()).unwrap(), 2).unwrap(),
            42
        );
        assert_eq!(p::data(&f, 2).unwrap(), input.client_message_id.as_bytes());
        assert_eq!(p::scalar(&f, 3).unwrap(), u64::from(u32::MAX));
        assert_eq!(p::scalar(&f, 4).unwrap(), 1_800_000_000);
        assert_eq!(p::scalar(&f, 5).unwrap(), 0);
        assert_eq!(p::data(&f, 6).unwrap(), b"synthetic_user");
        assert_eq!(p::data(&f, 7).unwrap(), b"synthetic_peer");
        assert_eq!(p::scalar(&f, 8).unwrap(), 0);
        assert_eq!(p::scalar(&f, 9).unwrap(), u64::MAX);
    }
    #[test]
    fn invalid_source_or_lossy_id_is_rejected_before_transport() {
        for sid in ["", "0", "01", "-1", "+1", " 1", "18446744073709551616"] {
            let mut r = recall();
            r.server_id = sid.into();
            assert!(request(&state(), &r).is_err());
        }
        let mut r = recall();
        r.local_id = 0;
        assert!(request(&state(), &r).is_err());
        r = recall();
        r.create_time = 0;
        assert!(request(&state(), &r).is_err());
        for peer in ["".into(), "peer\nother".into(), "x".repeat(513)] {
            r = recall();
            r.peer_username = peer;
            assert!(request(&state(), &r).is_err());
        }
    }
    #[test]
    fn only_unique_initialized_base_response_confirms_acceptance() {
        assert!(response(&p::bytes(1, &p::uint(1, 0))).unwrap().accepted);
        let rejected = response(&p::bytes(1, &p::int32(1, -13))).unwrap();
        assert_eq!(rejected.service_code, -13);
        assert!(!rejected.accepted);
        for bytes in [
            vec![],
            p::bytes(1, &[]),
            p::uint(1, 0),
            p::bytes(1, &p::uint(1, 1u64 << 32)),
            [p::bytes(1, &p::uint(1, 0)), p::bytes(1, &p::uint(1, 0))].concat(),
            [p::bytes(1, &p::uint(1, 0)), p::uint(2, 1)].concat(),
        ] {
            assert!(response(&bytes).is_err());
        }
    }
}
