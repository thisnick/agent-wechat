//! Authenticated logout for the supported native profile.
//!
//! The current typed wrapper uses CGI282; the historical embedded CGI133 table
//! does not describe this request. Optional request field2 has no assigned
//! semantics and is deliberately absent. Acceptance does not establish saved
//! device revocation or deletion from the phone's device inventory.
use super::*;

const CGI: u16 = 282;
const PATH: &str = "/cgi-bin/micromsg-bin/logout";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoutReply {
    pub service_code: i32,
    pub accepted: bool,
}

fn request(state: &AccountState) -> Result<Vec<u8>> {
    state.validate()?;
    Ok(p::bytes(
        1,
        &wire::base_request(&state.device, state.uin, 0)?,
    ))
}

fn response(bytes: &[u8]) -> Result<LogoutReply> {
    let fields = p::decode(bytes)?;
    let base = p::decode(p::data(&fields, 1)?)?;
    let raw = p::scalar(&base, 1)?;
    let code = raw as u32 as i32;
    ensure!(
        raw <= u32::MAX as u64 || raw == code as i64 as u64,
        "Invalid logout response status"
    );
    Ok(LogoutReply {
        service_code: code,
        accepted: code == 0,
    })
}

impl NativeClient {
    /// Submit one authenticated logout without renewal, retry or login fallback.
    /// An error can follow submission and must be treated as an unknown outcome.
    pub fn logout(&self, state: &mut AccountState) -> Result<LogoutReply> {
        let body = request(state)?;
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
    #[test]
    fn current_cgi_and_base_only_presence_match_the_typed_wrapper() {
        assert_eq!(CGI, 282);
        assert_eq!(PATH, "/cgi-bin/micromsg-bin/logout");
        let encoded = request(&state()).unwrap();
        let outer = p::decode(&encoded).unwrap();
        assert_eq!(outer.len(), 1);
        assert!(p::optional(&outer, 2).unwrap().is_none());
        let base = p::decode(p::data(&outer, 1).unwrap()).unwrap();
        assert_eq!(p::data(&base, 1).unwrap(), &[0]);
        assert_eq!(p::scalar(&base, 2).unwrap(), 42);
        assert_eq!(p::data(&base, 3).unwrap(), b"0123456789abcdef");
        assert_eq!(p::scalar(&base, 4).unwrap() as u32 as i32, -227271398);
        assert_eq!(p::data(&base, 5).unwrap(), b"UnifiedPCLinux x86_64");
        assert_eq!(p::scalar(&base, 6).unwrap(), 0);
    }
    #[test]
    fn response_requires_a_unique_initialized_base_and_retains_service_rejection() {
        let accepted = response(&p::bytes(1, &p::uint(1, 0))).unwrap();
        assert!(accepted.accepted);
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
    #[test]
    fn invalid_account_material_fails_before_any_transport() {
        let mut invalid = state();
        invalid.uin = 0;
        assert!(request(&invalid).is_err());
    }
}
