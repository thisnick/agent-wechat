//! Generic call-app authentication messages and the negotiated unencrypted app
//! session. Host MMTLS authentication remains required. No socket or credential
//! persistence is performed here; app encryption is distinct from media AEAD.

use super::signaling::CommonResponse;
use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};

pub const AUTH_COMMAND: u16 = 7994;
pub const AUTH_PATH: &str = "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth";
const MAX_AUTH: usize = 64 * 1024;

/// Accepted account/logout binding chosen by the account owner, not values
/// recovered from an unauthenticated server message. It is not a session key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppAuthority {
    pub account_generation: u64,
    pub logout_epoch: u64,
    pub credential_digest: [u8; 32],
}

impl AppAuthority {
    fn validate(self) -> Result<()> {
        ensure!(
            self.account_generation != 0 && self.credential_digest != [0; 32],
            "Inactive app account authority"
        );
        Ok(())
    }
}

/// One first-auth request. Consuming completion prevents accidental reuse after
/// success, malformed data or rejection. The host must retain uncertain intent
/// outcomes separately instead of automatically preparing another attempt.
pub struct PendingAppAuth {
    authority: AppAuthority,
    own_device: [u8; 32],
}

impl PendingAppAuth {
    /// Device identity is a fresh, independent 32-byte value supplied by a secure
    /// host RNG. It is not the account's ordinary host device ID.
    pub fn new(authority: AppAuthority, own_device: [u8; 32]) -> Result<Self> {
        authority.validate()?;
        ensure!(own_device != [0; 32], "Missing app device identity");
        Ok(Self {
            authority,
            own_device,
        })
    }

    /// Inner CGI7994 body, to place in the host CommonRequest's field2.
    pub fn request_body(&self) -> Result<Vec<u8>> {
        p::message(&[p::bytes(1, &p::bytes(1, &self.own_device))])
    }

    /// The input must be the corresponding host-authenticated CommonResponse.
    /// This selected implementation supports the explicit no-app-encryption
    /// branch only. It never falls back from failed encrypted processing.
    pub fn complete(
        self,
        authenticated_common: &[u8],
        current: AppAuthority,
    ) -> Result<AppSession> {
        ensure!(current == self.authority, "App account authority changed");
        ensure!(
            authenticated_common.len() <= MAX_AUTH,
            "App auth response too large"
        );
        let common = CommonResponse::decode(authenticated_common)?;
        ensure!(
            common.return_code()? == 0,
            "App auth host operation rejected"
        );
        let inner = decode(
            common
                .body
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing app auth body"))?,
        )?;
        let base = decode(data(&inner, 1)?)?;
        ensure!(
            signed(&base, 1, None)? == 0,
            "App auth base operation rejected"
        );
        let unify = decode(data(&inner, 2)?)?;
        ensure!(
            signed(&unify, 4, Some(0))? == 0,
            "App auth detail operation rejected"
        );
        let auth = decode(data(&unify, 2)?)?;
        let account = decode(data(&unify, 3)?)?;
        let flag = scalar(&auth, 11, Some(0))?;
        ensure!(
            u32::try_from(flag)? & 1 != 0,
            "Unsupported app encryption negotiation"
        );
        for number in [1, 2, 4] {
            ensure!(
                p::optional_data(&auth, number)?.is_none_or(|v| v.is_empty()),
                "Contradictory app encryption keys"
            );
        }
        let app_uin = scalar(&account, 1, None)?;
        ensure!(app_uin != 0, "Missing app identity");
        let server_id = p::optional_data(&auth, 9)?.unwrap_or_default();
        ensure!(server_id.len() <= 1024, "App server identity too large");
        Ok(AppSession {
            authority: self.authority,
            own_device: self.own_device,
            app_uin,
            server_id: server_id.to_vec(),
            auth_flags: flag as u32,
        })
    }
}

/// Server-selected app identity. No app keys exist in this branch. The server
/// does not install the encrypted branch's default expiry. Host account/logout
/// authority still fences every use; hosts may impose a separate local deadline.
/// Private app identities intentionally do not implement Debug or Serialize.
pub struct AppSession {
    authority: AppAuthority,
    own_device: [u8; 32],
    app_uin: u64,
    server_id: Vec<u8>,
    auth_flags: u32,
}

impl AppSession {
    pub fn validate_authority(&self, current: AppAuthority) -> Result<()> {
        ensure!(self.authority == current, "App account authority changed");
        Ok(())
    }

    pub fn app_uin(&self) -> u64 {
        self.app_uin
    }
    pub fn server_id(&self) -> &[u8] {
        &self.server_id
    }
    pub fn own_device(&self) -> &[u8; 32] {
        &self.own_device
    }
    pub fn auth_flags(&self) -> u32 {
        self.auth_flags
    }

    /// Source CommonSession fields4,5,6. Account UIN and app UIN are different
    /// namespaces; never use the host account's UIN here.
    pub fn common_session(&self, current: AppAuthority) -> Result<Vec<u8>> {
        self.validate_authority(current)?;
        p::message(&[
            p::uint(4, self.app_uin),
            p::bytes(5, &self.server_id),
            p::uint(6, 1),
        ])
    }
}

fn decode(input: &[u8]) -> Result<Vec<Field>> {
    ensure!(input.len() <= MAX_AUTH, "App auth message too large");
    super::signaling::decode_fields(input)
}
fn data(fields: &[Field], number: u32) -> Result<&[u8]> {
    p::optional_data(fields, number)?
        .ok_or_else(|| anyhow::anyhow!("Missing app auth message {number}"))
}
fn scalar(fields: &[Field], number: u32, default: Option<u64>) -> Result<u64> {
    match p::optional(fields, number)? {
        Some(Value::Varint(value)) => Ok(*value),
        None => default.ok_or_else(|| anyhow::anyhow!("Missing app auth integer {number}")),
        _ => bail!("Invalid app auth integer {number}"),
    }
}
fn signed(fields: &[Field], number: u32, default: Option<i32>) -> Result<i32> {
    Ok(i32::try_from(
        scalar(fields, number, default.map(|v| v as i64 as u64))? as i64,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn authority() -> AppAuthority {
        AppAuthority {
            account_generation: 1,
            logout_epoch: 0,
            credential_digest: [9; 32],
        }
    }
    fn reply(auth: &[u8], app_uin: u64) -> Vec<u8> {
        let inner = p::message(&[
            p::bytes(1, &[8, 0]),
            p::bytes(
                2,
                &p::message(&[p::bytes(2, auth), p::bytes(3, &p::uint(1, app_uin))]).unwrap(),
            ),
        ])
        .unwrap();
        p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &inner)]).unwrap()
    }
    #[test]
    fn encrypted_or_contradictory_branch_is_not_plain_fallback() {
        for auth in [vec![88, 0], vec![10, 1, 4, 88, 1], vec![88, 1, 88, 1]] {
            let pending = PendingAppAuth::new(authority(), [7; 32]).unwrap();
            assert!(pending.complete(&reply(&auth, 7), authority()).is_err());
        }
    }
    #[test]
    fn account_or_logout_change_invalidates_completion_and_session_use() {
        let pending = PendingAppAuth::new(authority(), [7; 32]).unwrap();
        let mut changed = authority();
        changed.logout_epoch = 1;
        assert!(pending.complete(&reply(&[88, 1], 7), changed).is_err());
        let pending = PendingAppAuth::new(authority(), [7; 32]).unwrap();
        let result = pending.complete(&reply(&[88, 1], 7), authority()).unwrap();
        assert!(result.common_session(changed).is_err());
    }
}
