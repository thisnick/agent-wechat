#[path = "group_inventory.rs"]
mod group_inventory;
pub use group_inventory::*;
#[path = "logout.rs"]
mod logout;
pub use crate::wire::{
    AddMessage, AvatarBytes, ContactLookup, ContactRecord, Heartbeat, InitContactPage, RoomMember,
    RoomRoster, SyncBatch, SyncCommand, TextSendResult, VoiceChunk,
};
use crate::{
    business::{self, AuthenticatedPacket, Prelogin},
    crypto::Ecdh,
    mmtls::{self, Session},
    protobuf as p, wire,
};
use anyhow::{bail, ensure, Result};
pub use logout::*;
use openssl::{nid::Nid, pkey::PKey, rsa::Rsa};
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use zeroize::Zeroize;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    pub device_id: Vec<u8>,
    pub device_type: String,
    pub client_version: i32,
}
impl fmt::Debug for DeviceProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceProfile")
            .field("device_id", &"[redacted]")
            .field("device_type", &self.device_type)
            .field("client_version", &self.client_version)
            .finish()
    }
}
impl DeviceProfile {
    pub fn generate(device_type: String) -> Result<Self> {
        let bytes = crate::crypto::random::<16>()?;
        let id = format!("l{}", hex::encode(bytes)).into_bytes()[..16].to_vec();
        let p = Self {
            device_id: id,
            device_type,
            client_version: -227271398,
        };
        p.validate()?;
        Ok(p)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.device_id.len() == 16
                && !self.device_type.is_empty()
                && self.device_type.len() <= 1024
                && !self.device_type.contains('\0'),
            "Invalid native device profile"
        );
        ensure!(
            self.client_version == -227271398,
            "Unsupported native client version"
        );
        Ok(())
    }
}
/// Raw accepted auth sections, retained only inside the encrypted account state.
/// The update mask describes native application gates; presence alone does not
/// mean an optional section changed active account credentials.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSnapshot {
    pub update_mask: u32,
    pub response: Vec<u8>,
    pub auth: Vec<u8>,
    pub account: Vec<u8>,
    pub network: Vec<u8>,
    pub ax_auth: Vec<u8>,
}
impl fmt::Debug for AuthSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSnapshot")
            .field("sections", &"[redacted]")
            .finish()
    }
}
impl Drop for AuthSnapshot {
    fn drop(&mut self) {
        self.response.zeroize();
        self.auth.zeroize();
        self.account.zeroize();
        self.network.zeroize();
        self.ax_auth.zeroize();
    }
}
/// Accepted credentials. Serialize only into the service's encrypted state vault.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountState {
    pub schema: u32,
    pub device: DeviceProfile,
    pub uin: u32,
    pub own_username: String,
    pub cookie: Vec<u8>,
    pub session_key: Vec<u8>,
    pub request_key: Vec<u8>,
    pub response_key: Vec<u8>,
    pub auto_auth_key: Vec<u8>,
    pub key_info: Vec<u8>,
    /// Native AuthSect field 9 SKBuffer. This is the official profile password,
    /// distinct from transport keys, and is never part of an HTTP response.
    #[serde(default)]
    pub database_password: Vec<u8>,
    #[serde(default)]
    pub auth_snapshot: AuthSnapshot,
    pub long_host: String,
    pub short_host: String,
}
impl fmt::Debug for AccountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountState")
            .field("schema", &self.schema)
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl Drop for AccountState {
    fn drop(&mut self) {
        self.cookie.zeroize();
        self.session_key.zeroize();
        self.request_key.zeroize();
        self.response_key.zeroize();
        self.auto_auth_key.zeroize();
        self.key_info.zeroize();
        self.database_password.zeroize();
    }
}
impl AccountState {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.uin != 0
                && !self.own_username.is_empty()
                && self.own_username.len() <= 256
                && !self.own_username.contains('\0'),
            "Invalid native account identity"
        );
        self.device.validate()?;
        mmtls::validate_host(&self.long_host)?;
        mmtls::validate_host(&self.short_host)?;
        ensure!(
            self.cookie.len() <= 15
                && self.session_key.len() == 16
                && ((self.request_key.is_empty() && self.response_key.is_empty())
                    || (self.request_key.len() == 24 && self.response_key.len() == 24))
                && self.auto_auth_key.len() <= 1024 * 1024
                && self.key_info.len() <= 1024 * 1024
                && (self.database_password.is_empty() || self.database_password.len() == 32)
                && [
                    &self.auth_snapshot.response,
                    &self.auth_snapshot.auth,
                    &self.auth_snapshot.account,
                    &self.auth_snapshot.network,
                    &self.auth_snapshot.ax_auth
                ]
                .iter()
                .all(|v| v.len() <= 1024 * 1024),
            "Invalid native account keys"
        );
        Ok(())
    }
}
impl AccountState {
    /// Import an operator-selected private baseline checkpoint. This only converts
    /// explicit standalone credentials; it never searches or reads official files.
    /// Return diagnostics contain field names, never supplied credential values.
    pub fn from_baseline_json(input: &[u8]) -> Result<Self> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        ensure!(
            input.len() <= 8 * 1024 * 1024,
            "Baseline checkpoint too large"
        );
        let value: serde_json::Value = serde_json::from_slice(input)
            .map_err(|_| anyhow::anyhow!("Invalid baseline checkpoint JSON"))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Invalid baseline checkpoint object"))?;
        let string = |name: &str| -> Result<String> {
            object
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("Missing baseline field {name}"))
        };
        let decode = |name: &str, required: bool| -> Result<Vec<u8>> {
            let encoded = match object.get(name) {
                Some(v) => v
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid baseline field {name}"))?,
                None if !required => "",
                _ => bail!("Missing baseline field {name}"),
            };
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| anyhow::anyhow!("Invalid baseline encoding {name}"))?;
            ensure!(
                STANDARD.encode(&bytes) == encoded,
                "Noncanonical baseline encoding {name}"
            );
            Ok(bytes)
        };
        ensure!(
            object.get("schema").and_then(|v| v.as_u64()) == Some(1),
            "Unsupported baseline schema"
        );
        let uin = match object.get("uin") {
            Some(serde_json::Value::String(s)) => s
                .parse::<u32>()
                .map_err(|_| anyhow::anyhow!("Invalid baseline UIN"))?,
            Some(n) => u32::try_from(
                n.as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Invalid baseline UIN"))?,
            )?,
            None => bail!("Missing baseline UIN"),
        };
        let account = decode("accountSect", false)?;
        let section_username = if account.is_empty() {
            None
        } else {
            let f = p::decode(&account)?;
            p::optional_data(&f, 1)?
                .map(|v| std::str::from_utf8(v).map(str::to_string))
                .transpose()
                .map_err(|_| anyhow::anyhow!("Invalid baseline account username"))?
        };
        let own_username = object
            .get("ownUsername")
            .or_else(|| object.get("username"))
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("Invalid baseline username"))
            })
            .transpose()?
            .or(section_username.clone())
            .ok_or_else(|| anyhow::anyhow!("Missing baseline account identity"))?;
        ensure!(
            section_username.is_none_or(|s| s == own_username),
            "Baseline account identity mismatch"
        );
        let state =
            Self {
                schema: 1,
                device: DeviceProfile {
                    device_id: decode("deviceId", true)?,
                    device_type: string("deviceType")?,
                    client_version: i32::try_from(
                        object
                            .get("clientVersion")
                            .and_then(|v| v.as_i64())
                            .ok_or_else(|| anyhow::anyhow!("Invalid baseline client version"))?,
                    )?,
                },
                uin,
                own_username,
                cookie: decode("cookie", false)?,
                session_key: decode("sessionKey", true)?,
                request_key: decode("field23", false)?,
                response_key: decode("field24", false)?,
                auto_auth_key: decode("autoAuthKey", false)?,
                key_info: decode("keyInfo", false)?,
                database_password: decode("databasePassword", false)?,
                auth_snapshot: AuthSnapshot {
                    update_mask: match object.get("authSections") {
                        Some(serde_json::Value::String(value)) => value.parse()?,
                        Some(value) => u32::try_from(value.as_u64().ok_or_else(|| {
                            anyhow::anyhow!("Invalid baseline auth section mask")
                        })?)?,
                        None => 0,
                    },
                    response: decode("authResponse", false)?,
                    auth: decode("authSect", false)?,
                    account,
                    network: decode("networkSect", false)?,
                    ax_auth: decode("axAuthSect", false)?,
                },
                long_host: object
                    .get("longHost")
                    .and_then(|v| v.as_str())
                    .unwrap_or("long.weixin.qq.com")
                    .to_string(),
                short_host: object
                    .get("shortHost")
                    .and_then(|v| v.as_str())
                    .unwrap_or("short.weixin.qq.com")
                    .to_string(),
            };
        state.validate()?;
        Ok(state)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginAttempt {
    pub schema: u32,
    pub device: DeviceProfile,
    pub computer_name: String,
    pub qr_png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub created_at: u32,
    pub expires_at: u32,
    pub identifier: Vec<u8>,
    pub notify_key: Vec<u8>,
    pub cookie: Vec<u8>,
    pub key_version: u16,
    pub long_host: String,
    pub short_host: String,
    pub status: LoginStatus,
    credential3: Option<Vec<u8>>,
    credential4: Option<Vec<u8>>,
}
impl fmt::Debug for LoginAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginAttempt")
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl Drop for LoginAttempt {
    fn drop(&mut self) {
        self.identifier.zeroize();
        self.notify_key.zeroize();
        self.cookie.zeroize();
        self.credential3.zeroize();
        self.credential4.zeroize();
    }
}
impl LoginAttempt {
    pub fn qr_svg(&self) -> Result<String> {
        use base64::Engine;
        ensure!(
            !self.qr_png.is_empty(),
            "This saved-device attempt has no QR"
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(&self.qr_png);
        Ok(format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"530\" height=\"530\" viewBox=\"0 0 265 265\"><rect width=\"265\" height=\"265\" fill=\"white\"/><image x=\"40\" y=\"40\" width=\"185\" height=\"185\" style=\"image-rendering:pixelated\" href=\"data:image/png;base64,{encoded}\"/></svg>"))
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && wire::unix_time()? < self.expires_at,
            "Login attempt expired"
        );
        self.device.validate()?;
        ensure!(
            !self.identifier.is_empty()
                && self.identifier.len() <= 4096
                && !self.notify_key.is_empty()
                && self.notify_key.len() <= 1024
                && self.cookie.len() <= 15,
            "Invalid login attempt"
        );
        mmtls::validate_host(&self.long_host)?;
        mmtls::validate_host(&self.short_host)?;
        ensure!(
            [10014, 50005].contains(&self.key_version),
            "Unsupported login key version"
        );
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Pending,
    Scanned,
    Approved,
    Expired,
    Cancelled,
    Rejected,
    Completed,
}
#[derive(Clone, Debug)]
pub struct NativeOptions {
    pub long_host: String,
    pub short_host: String,
    pub timeout: Duration,
    pub receive_budget: usize,
}
impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            long_host: "long.weixin.qq.com".to_string(),
            short_host: "short.weixin.qq.com".to_string(),
            timeout: Duration::from_secs(30),
            receive_budget: crate::MAX_MESSAGE_BYTES,
        }
    }
}
pub struct NativeClient {
    options: NativeOptions,
}
/// Polling an issued login challenge is repeatable. This does not authorize
/// retrying QR creation, credential exchange, sends or rejected authentication.
pub fn is_retryable_login_poll_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            );
        }
        if let Some(http) = cause.downcast_ref::<reqwest::Error>() {
            return http.is_timeout() || http.is_connect();
        }
        let message = cause.to_string();
        matches!(
            message.as_str(),
            "MMTLS deadline exceeded" | "MMTLS connection closed"
        ) || message
            .strip_prefix("Native shortlink HTTP status ")
            .and_then(|s| s.parse::<u16>().ok())
            .is_some_and(|status| matches!(status, 408 | 429 | 500 | 502 | 503 | 504))
    })
}
impl NativeClient {
    pub fn new(options: NativeOptions) -> Result<Self> {
        mmtls::validate_host(&options.long_host)?;
        mmtls::validate_host(&options.short_host)?;
        ensure!(
            !options.timeout.is_zero() && options.timeout <= Duration::from_secs(120),
            "Invalid native timeout"
        );
        ensure!(
            (1024..=crate::MAX_MESSAGE_BYTES).contains(&options.receive_budget),
            "Invalid receive budget"
        );
        Ok(Self { options })
    }
    fn session(&self, host: &str) -> Result<Session> {
        Session::connect(host, self.options.timeout, self.options.receive_budget)
    }
    #[allow(clippy::too_many_arguments)]
    fn prelogin_rpc(
        &self,
        long_host: &str,
        short_host: &str,
        cgi: u16,
        path: &str,
        plain: &[u8],
        cookie: &[u8],
        auth_key: &[u8],
        fixed_version: Option<u16>,
    ) -> Result<(Vec<u8>, Vec<u8>, u16)> {
        let session = self.session(long_host)?;
        let profile = if let Some(version) = fixed_version {
            business::profile_for_region(if version == 50005 { 2 } else { 1 })?
        } else {
            business::profile_for_region(session.certificate_region)?
        };
        let prepared = Prelogin::prepare(plain, &profile, auth_key)?;
        let packet = business::prelogin_packet(&prepared, cgi, profile.version, cookie)?;
        let reply = mmtls::short_exchange(
            &session.short,
            short_host,
            path,
            &packet,
            self.options.timeout,
        )?;
        let envelope = business::split(&reply)?;
        ensure!(
            envelope.return_code == 0,
            "Native envelope returned {}",
            envelope.return_code
        );
        ensure!(
            envelope.cgi == cgi as u64 && envelope.encryption == 12,
            "Unexpected prelogin response policy"
        );
        let plain = prepared.decrypt_reply(&envelope.payload)?;
        Ok((plain, envelope.cookie, profile.version))
    }
    pub fn start_qr_login(
        &self,
        device: DeviceProfile,
        computer_name: &str,
    ) -> Result<LoginAttempt> {
        device.validate()?;
        let plain = wire::qr_request(&device, computer_name)?;
        let (reply, cookie, key_version) = self.prelogin_rpc(
            &self.options.long_host,
            &self.options.short_host,
            502,
            "/cgi-bin/micromsg-bin/getloginqrcode",
            &plain,
            &[],
            &[],
            None,
        )?;
        let qr = wire::decode_qr(&reply)?;
        let now = wire::unix_time()?;
        Ok(LoginAttempt {
            schema: 1,
            device,
            computer_name: computer_name.to_string(),
            qr_png: qr.image,
            width: qr.width,
            height: qr.height,
            created_at: now,
            expires_at: now.saturating_add(240),
            identifier: qr.identifier,
            notify_key: qr.notify_key,
            cookie,
            key_version,
            long_host: self.options.long_host.clone(),
            short_host: self.options.short_host.clone(),
            status: LoginStatus::Pending,
            credential3: None,
            credential4: None,
        })
    }
    /// Initiates phone approval for an existing remembered device without a QR.
    /// The caller chooses this fallback explicitly; no automatic auth retries occur.
    pub fn start_saved_device_login(
        &self,
        state: &AccountState,
        computer_name: &str,
    ) -> Result<LoginAttempt> {
        state.validate()?;
        self.start_saved_login(
            &state.device,
            &state.own_username,
            &state.auto_auth_key,
            &state.cookie,
            &state.long_host,
            &state.short_host,
            computer_name,
        )
    }
    /// Use remembered official-profile trust without inventing active session keys.
    pub fn start_profile_saved_device_login(
        &self,
        device: &DeviceProfile,
        username: &str,
        auto_auth_key: &[u8],
        cookie: &[u8],
        computer_name: &str,
    ) -> Result<LoginAttempt> {
        self.start_saved_login(
            device,
            username,
            auto_auth_key,
            cookie,
            &self.options.long_host,
            &self.options.short_host,
            computer_name,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn start_saved_login(
        &self,
        device: &DeviceProfile,
        username: &str,
        auto_auth_key: &[u8],
        cookie: &[u8],
        long_host: &str,
        short_host: &str,
        computer_name: &str,
    ) -> Result<LoginAttempt> {
        device.validate()?;
        ensure!(
            !username.is_empty()
                && username.len() <= 256
                && !username.contains('\0')
                && !auto_auth_key.is_empty()
                && auto_auth_key.len() <= 1024 * 1024
                && cookie.len() <= 15,
            "Invalid saved-device credentials"
        );
        let rsa = Rsa::generate(1024)?;
        let public = PKey::from_rsa(rsa)?.public_key_to_pem()?;
        let request = wire::push_login(device, username, auto_auth_key, &public)?;
        let (reply, cookie, key_version) = self.prelogin_rpc(
            long_host,
            short_host,
            654,
            "/cgi-bin/micromsg-bin/pushloginurl",
            &request,
            cookie,
            &[],
            None,
        )?;
        let (identifier, notify_key) = wire::decode_push(&reply)?;
        let now = wire::unix_time()?;
        Ok(LoginAttempt {
            schema: 1,
            device: device.clone(),
            computer_name: computer_name.to_string(),
            qr_png: vec![],
            width: 0,
            height: 0,
            created_at: now,
            expires_at: now.saturating_add(240),
            identifier,
            notify_key,
            cookie,
            key_version,
            long_host: long_host.into(),
            short_host: short_host.into(),
            status: LoginStatus::Pending,
            credential3: None,
            credential4: None,
        })
    }
    pub fn poll_login(&self, attempt: &mut LoginAttempt) -> Result<LoginStatus> {
        if wire::unix_time()? >= attempt.expires_at {
            attempt.status = LoginStatus::Expired;
            return Ok(attempt.status);
        }
        attempt.validate()?;
        ensure!(
            !matches!(
                attempt.status,
                LoginStatus::Completed | LoginStatus::Cancelled | LoginStatus::Rejected
            ),
            "Login attempt is terminal"
        );
        let request = wire::poll_request(&attempt.device, &attempt.identifier)?;
        let (reply, cookie, _) = self.prelogin_rpc(
            &attempt.long_host,
            &attempt.short_host,
            503,
            "/cgi-bin/micromsg-bin/checkloginqrcode",
            &request,
            &attempt.cookie,
            &[],
            Some(attempt.key_version),
        )?;
        let response = wire::decode_poll(&reply, &attempt.notify_key)?;
        if !cookie.is_empty() {
            attempt.cookie = cookie;
        }
        if let Some(status) = response.status {
            attempt.status = match status {
                0 => LoginStatus::Pending,
                1 => LoginStatus::Scanned,
                2 => LoginStatus::Approved,
                // Unrecognized terminal notification values do not establish
                // an expiry/cancellation reason. Only our own timer expires it.
                _ => LoginStatus::Rejected,
            };
            if attempt.status == LoginStatus::Approved {
                attempt.credential3 = response.credential3;
                attempt.credential4 = response.credential4;
            }
        }
        Ok(attempt.status)
    }
    pub fn complete_login(&self, attempt: &mut LoginAttempt) -> Result<AccountState> {
        attempt.validate()?;
        ensure!(
            attempt.status == LoginStatus::Approved,
            "Login has not been approved"
        );
        let inner = Ecdh::generate(Nid::SECP224R1)?;
        let request = wire::manual_auth(
            &attempt.device,
            &attempt.computer_name,
            attempt
                .credential3
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing approval"))?,
            attempt
                .credential4
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing approval"))?,
            &inner.public()?,
            &[],
        )?;
        let mut long_host = attempt.long_host.clone();
        let mut short_host = attempt.short_host.clone();
        let mut cookie = attempt.cookie.clone();
        for redirected in 0..=1 {
            let (reply, next_cookie, _) = self.prelogin_rpc(
                &long_host,
                &short_host,
                252,
                "/cgi-bin/micromsg-bin/secmanualauth",
                &request,
                &cookie,
                &[],
                None,
            )?;
            let fields = p::decode(&reply)?;
            let code = p::base_code(&fields)?;
            if code == -301 && redirected == 0 {
                (long_host, short_host) = redirect(&fields)?;
                cookie = next_cookie;
                continue;
            }
            ensure!(code == 0, "Account authentication returned {code}");
            let mut state = reduce_auth(&reply, None, &inner, &attempt.device, &next_cookie)?;
            state.long_host = long_host;
            state.short_host = short_host;
            state.validate()?;
            attempt.status = LoginStatus::Completed;
            attempt.credential3.zeroize();
            attempt.credential4.zeroize();
            return Ok(state);
        }
        bail!("Native authentication redirect limit reached")
    }
    /// One trusted AutoAuth exchange, retaining identity and replacing selected key
    /// sections atomically in the returned value. Does not silently fall back to QR.
    pub fn renew_session(
        &self,
        previous: &AccountState,
        computer_name: &str,
    ) -> Result<AccountState> {
        previous.validate()?;
        let inner = Ecdh::generate(Nid::SECP224R1)?;
        let request = wire::auto_auth(previous, &inner.public()?, computer_name)?;
        let mut long = previous.long_host.clone();
        let mut short = previous.short_host.clone();
        let mut cookie = previous.cookie.clone();
        for redirected in 0..=1 {
            let (reply, next_cookie, _) = self.prelogin_rpc(
                &long,
                &short,
                763,
                "/cgi-bin/micromsg-bin/secautoauth",
                &request,
                &cookie,
                &previous.auto_auth_key,
                None,
            )?;
            let fields = p::decode(&reply)?;
            let code = p::base_code(&fields)?;
            if code == -301 && redirected == 0 {
                (long, short) = redirect(&fields)?;
                cookie = next_cookie;
                continue;
            }
            ensure!(code == 0, "Automatic authentication returned {code}");
            let mut next = reduce_auth(
                &reply,
                Some(previous),
                &inner,
                &previous.device,
                &next_cookie,
            )?;
            next.long_host = long;
            next.short_host = short;
            next.validate()?;
            return Ok(next);
        }
        bail!("AutoAuth redirect limit reached")
    }
    fn authenticated_rpc(
        &self,
        state: &mut AccountState,
        cgi: u16,
        path: &str,
        request: &[u8],
    ) -> Result<Vec<u8>> {
        let session = self.session(&state.long_host)?;
        let packet = AuthenticatedPacket::prepare(state, cgi, request)?;
        let reply = mmtls::short_exchange(
            &session.short,
            &state.short_host,
            path,
            &packet.packet,
            self.options.timeout,
        )?;
        let (plain, cookie) = packet.decode_verified_transport_reply(&reply)?;
        if !cookie.is_empty() {
            state.cookie = cookie;
        }
        Ok(plain)
    }
    pub fn sync(&self, state: &mut AccountState, cursor: &[u8]) -> Result<SyncBatch> {
        self.sync_scene(state, cursor, 6)
    }
    pub fn sync_scene(
        &self,
        state: &mut AccountState,
        cursor: &[u8],
        scene: u32,
    ) -> Result<SyncBatch> {
        ensure!([6, 7].contains(&scene), "Unsupported sync scene");
        let wire_cursor = if cursor.is_empty() { &[0][..] } else { cursor };
        let request = wire::sync_request(wire_cursor, &state.device.device_type, scene)?;
        wire::decode_sync(
            &self.authenticated_rpc(state, 138, "/cgi-bin/micromsg-bin/newsync", &request)?,
            cursor,
        )
    }
    pub fn heartbeat(&self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat> {
        let request = wire::heartbeat_request(state, cursor)?;
        wire::decode_heartbeat(&self.authenticated_rpc(
            state,
            518,
            "/cgi-bin/micromsg-bin/heartbeat",
            &request,
        )?)
    }
    /// Submit exactly once. An I/O error after submission is an uncertain outcome;
    /// services must persist the intent and must not automatically retry this call.
    pub fn send_text(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
    ) -> Result<TextSendResult> {
        let request = wire::text_send_request(destination, content, created_at, client_message_id)?;
        wire::decode_text_send(
            &self.authenticated_rpc(state, 522, "/cgi-bin/micromsg-bin/newsendmsg", &request)?,
            destination,
            client_message_id,
        )
    }
    pub fn lookup_contacts(
        &self,
        state: &mut AccountState,
        usernames: &[String],
    ) -> Result<ContactLookup> {
        let request = wire::contact_request(state, usernames)?;
        wire::decode_contacts(
            &self.authenticated_rpc(state, 182, "/cgi-bin/micromsg-bin/getcontact", &request)?,
            usernames,
        )
    }
    pub fn lookup_room(
        &self,
        state: &mut AccountState,
        room_id: &str,
        version: u32,
    ) -> Result<RoomRoster> {
        let request = wire::room_request(state, room_id, version)?;
        wire::decode_room(
            &self.authenticated_rpc(
                state,
                551,
                "/cgi-bin/micromsg-bin/getchatroommemberdetail",
                &request,
            )?,
            room_id,
        )
    }
    pub fn init_contacts(
        &self,
        state: &mut AccountState,
        contact_sequence: u32,
        room_sequence: u32,
    ) -> Result<InitContactPage> {
        let request =
            wire::init_contacts_request(&state.own_username, contact_sequence, room_sequence)?;
        let page = wire::decode_init_contacts(&self.authenticated_rpc(
            state,
            851,
            "/cgi-bin/micromsg-bin/initcontact",
            &request,
        )?)?;
        ensure!(
            !page.more
                || page.contact_sequence != contact_sequence
                || page.room_sequence != room_sequence,
            "Contact bootstrap made no cursor progress"
        );
        Ok(page)
    }
    pub fn download_avatars(
        &self,
        state: &mut AccountState,
        usernames: &[String],
    ) -> Result<Vec<AvatarBytes>> {
        let request = wire::avatar_request(state, usernames)?;
        wire::decode_avatars(
            &self.authenticated_rpc(
                state,
                123,
                "/cgi-bin/micromsg-bin/batchgetheadimg",
                &request,
            )?,
            usernames,
        )
    }
    pub fn download_voice_chunk(
        &self,
        state: &mut AccountState,
        server_message_id: u64,
        total_length: u32,
        offset: u32,
    ) -> Result<VoiceChunk> {
        let request = wire::voice_request(state, server_message_id, total_length, offset)?;
        wire::decode_voice(
            &self.authenticated_rpc(state, 128, "/cgi-bin/micromsg-bin/downloadvoice", &request)?,
            server_message_id,
            total_length,
            offset,
        )
    }
    pub fn cdn_config(&self, state: &mut AccountState) -> Result<crate::media::CdnConfig> {
        let request = p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?);
        let reply =
            self.authenticated_rpc(state, 379, "/cgi-bin/micromsg-bin/getcdndns", &request)?;
        crate::media::decode_cdn_config(&reply, state)
    }
    pub fn send_file(
        &self,
        state: &mut AccountState,
        destination: &str,
        client_id: &str,
        created_at: u32,
        upload: &crate::media::UploadedMedia,
    ) -> Result<crate::media_send::MediaSendResult> {
        let request = crate::media_send::file_send_request(
            state,
            destination,
            client_id,
            created_at,
            upload,
        )?;
        crate::media_send::decode_file_send(
            &self.authenticated_rpc(state, 222, "/cgi-bin/micromsg-bin/sendappmsg", &request)?,
            client_id,
        )
    }
    pub fn send_image(
        &self,
        state: &mut AccountState,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedImage,
    ) -> Result<crate::media_send::MediaSendResult> {
        let request = crate::media_send::image_send_request(state, destination, client_id, upload)?;
        crate::media_send::decode_image_send(
            &self.authenticated_rpc(state, 110, "/cgi-bin/micromsg-bin/uploadmsgimg", &request)?,
            client_id,
            &state.own_username,
            destination,
        )
    }
    pub fn send_video(
        &self,
        state: &mut AccountState,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedVideo,
        play_seconds: u32,
    ) -> Result<crate::media_send::MediaSendResult> {
        let request = crate::media_send::video_send_request(
            state,
            destination,
            client_id,
            upload,
            play_seconds,
        )?;
        crate::media_send::decode_video_send(
            &self.authenticated_rpc(state, 149, "/cgi-bin/micromsg-bin/uploadvideo", &request)?,
            client_id,
        )
    }
    pub fn send_voice(
        &self,
        state: &mut AccountState,
        destination: &str,
        client_id: &str,
        created_at: u32,
        duration_ms: u32,
        encoded: &[u8],
    ) -> Result<crate::media_send::MediaSendResult> {
        let request = crate::media_send::voice_send_request(
            state,
            destination,
            client_id,
            created_at,
            duration_ms,
            encoded,
        )?;
        crate::media_send::decode_voice_send(
            &self.authenticated_rpc(state, 127, "/cgi-bin/micromsg-bin/uploadvoice", &request)?,
            client_id,
        )
    }
}

fn redirect(fields: &[p::Field]) -> Result<(String, String)> {
    let network = p::decode(p::data(fields, 5)?)?;
    let hosts = p::decode(p::data(&network, 1)?)?;
    let target = |source: &[u8]| -> Result<String> {
        let mut out = None;
        for f in hosts.iter().filter(|f| f.number == 2) {
            let p::Value::Bytes(raw) = &f.value else {
                bail!("Invalid redirect map");
            };
            let item = p::decode(raw)?;
            if p::data(&item, 1)? == source {
                ensure!(out.is_none(), "Ambiguous redirect");
                let name = std::str::from_utf8(p::data(&item, 2)?)?.to_string();
                mmtls::validate_host(&name)?;
                out = Some(name);
            }
        }
        out.ok_or_else(|| anyhow::anyhow!("Missing redirect mapping"))
    };
    Ok((
        target(b"long.weixin.qq.com")?,
        target(b"short.weixin.qq.com")?,
    ))
}
fn reduce_auth(
    plain: &[u8],
    previous: Option<&AccountState>,
    inner: &Ecdh,
    device: &DeviceProfile,
    cookie: &[u8],
) -> Result<AccountState> {
    let f = p::decode(plain)?;
    ensure!(p::base_code(&f)? == 0, "Rejected auth response");
    let mask = u32::try_from(p::scalar(&f, 2)?)?;
    for (bit, n) in [(1, 3), (2, 4), (4, 5)] {
        if mask & bit != 0 {
            p::data(&f, n)?;
        }
    }
    let mut state = previous.cloned().unwrap_or_else(|| AccountState {
        schema: 1,
        device: device.clone(),
        uin: 0,
        own_username: String::new(),
        cookie: vec![],
        session_key: vec![],
        request_key: vec![],
        response_key: vec![],
        auto_auth_key: vec![],
        key_info: vec![],
        database_password: vec![],
        auth_snapshot: AuthSnapshot::default(),
        long_host: "long.weixin.qq.com".into(),
        short_host: "short.weixin.qq.com".into(),
    });
    ensure!(
        plain.len() <= 1024 * 1024,
        "Auth response snapshot too large"
    );
    state.auth_snapshot.update_mask = mask;
    state.auth_snapshot.response = plain.to_vec();
    for (number, target) in [
        (3, &mut state.auth_snapshot.auth),
        (4, &mut state.auth_snapshot.account),
        (5, &mut state.auth_snapshot.network),
        (6, &mut state.auth_snapshot.ax_auth),
    ] {
        if let Some(section) = p::optional_data(&f, number)? {
            *target = section.to_vec();
        }
    }
    if let Some(account) = p::optional_data(&f, 4)? {
        let a = p::decode(account)?;
        if let Some(name) = p::optional_data(&a, 1)? {
            let name = std::str::from_utf8(name)?;
            ensure!(
                previous.is_none_or(|s| s.own_username == name),
                "Auth account identity changed"
            );
            state.own_username = name.to_string();
        }
    }
    if mask & 1 != 0 {
        let auth = p::data(&f, 3)?;
        let a = p::decode(auth)?;
        let uin = u32::try_from(p::scalar(&a, 1)?)?;
        ensure!(
            uin != 0 && previous.is_none_or(|s| s.uin == uin),
            "Auth UIN changed"
        );
        let (session, request, response) = business::account_keys(inner, auth)?;
        state.uin = uin;
        state.session_key = session;
        state.request_key = request;
        state.response_key = response;
        state.auto_auth_key = p::optional_data(&a, 4)?
            .map(p::unbuffer)
            .transpose()?
            .unwrap_or_default();
        if let Some(password) = p::optional_data(&a, 9)? {
            let password = p::unbuffer(password)?;
            ensure!(
                password.is_empty() || password.len() == 32,
                "Invalid native database password"
            );
            state.database_password.zeroize();
            state.database_password = password;
        }
        state.key_info = p::optional_data(&a, 10)?
            .map(p::unbuffer)
            .transpose()?
            .unwrap_or_default();
    } else {
        ensure!(previous.is_some(), "Initial auth has no key section");
    }
    ensure!(cookie.len() <= 15, "Invalid auth cookie");
    if !cookie.is_empty() {
        state.cookie = cookie.to_vec();
    }
    state.validate()?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![1; 16],
                device_type: "UnifiedPCLinux synthetic x86_64".into(),
                client_version: -227271398,
            },
            uin: 123,
            own_username: "synthetic-user".into(),
            cookie: vec![5],
            session_key: vec![6; 16],
            request_key: vec![7; 24],
            response_key: vec![8; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: AuthSnapshot::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    #[test]
    fn challenge_poll_retries_only_transport_availability_failures() {
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::WouldBlock,
            ErrorKind::ConnectionReset,
            ErrorKind::UnexpectedEof,
        ] {
            assert!(is_retryable_login_poll_error(
                &anyhow::Error::new(Error::from(kind)).context("polling challenge")
            ));
        }
        for message in [
            "MMTLS deadline exceeded",
            "MMTLS connection closed",
            "Native shortlink HTTP status 503",
        ] {
            assert!(is_retryable_login_poll_error(&anyhow::anyhow!("{message}")));
        }
        for message in [
            "Login poll returned -13",
            "Native envelope returned -13",
            "Native shortlink HTTP status 403",
            "Native shortlink HTTP status 5030",
            "Missing authentication tag",
            "Unexpected MMTLS header",
            "Truncated structure",
        ] {
            assert!(!is_retryable_login_poll_error(&anyhow::anyhow!(
                "{message}"
            )));
        }
        assert!(!is_retryable_login_poll_error(
            &Error::from(ErrorKind::PermissionDenied).into()
        ));
    }
    #[test]
    fn secrets_not_debugged() {
        let s = state();
        let text = format!("{s:?}");
        assert!(!text.contains("synthetic-user"));
        assert!(!text.contains("123"));
        assert!(text.contains("redacted"));
    }
    #[test]
    fn account_state_roundtrips_without_number_loss() {
        let s = state();
        let json = serde_json::to_vec(&s).unwrap();
        let restored: AccountState = serde_json::from_slice(&json).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.uin, s.uin);
    }
    #[test]
    fn database_password_follows_auth_update_gate_and_optional_presence() {
        let mut old = state();
        old.database_password = vec![11; 32];
        let inner = Ecdh::generate(Nid::SECP224R1).unwrap();
        let response = |mask, password: Option<&[u8]>| {
            let mut fields = vec![
                p::uint(1, old.uin as u64),
                p::bytes(3, &p::buffer(&[1; 16])),
                p::uint(19, 0),
            ];
            if let Some(bytes) = password {
                fields.push(p::bytes(9, &p::buffer(bytes)));
            }
            p::message(&[
                p::bytes(1, &p::uint(1, 0)),
                p::uint(2, mask),
                p::bytes(3, &p::message(&fields).unwrap()),
                p::bytes(6, b"synthetic-ax"),
            ])
            .unwrap()
        };
        let gated_off = response(0, Some(&[22; 32]));
        let unchanged = reduce_auth(&gated_off, Some(&old), &inner, &old.device, &[]).unwrap();
        assert_eq!(unchanged.database_password, old.database_password);
        assert_eq!(unchanged.auth_snapshot.update_mask, 0);
        assert_eq!(unchanged.auth_snapshot.response, gated_off);
        assert_eq!(unchanged.auth_snapshot.ax_auth, b"synthetic-ax");
        let accepted = reduce_auth(
            &response(1, Some(&[22; 32])),
            Some(&old),
            &inner,
            &old.device,
            &[],
        )
        .unwrap();
        assert_eq!(accepted.database_password, vec![22; 32]);
        let absent = reduce_auth(
            &response(1, None),
            Some(&accepted),
            &inner,
            &old.device,
            &[],
        )
        .unwrap();
        assert_eq!(absent.database_password, accepted.database_password);
        let empty = reduce_auth(
            &response(1, Some(&[])),
            Some(&absent),
            &inner,
            &old.device,
            &[],
        )
        .unwrap();
        assert!(empty.database_password.is_empty());
        assert!(reduce_auth(
            &response(1, Some(&[22; 31])),
            Some(&old),
            &inner,
            &old.device,
            &[]
        )
        .is_err());
        let mut legacy = serde_json::to_value(&old).unwrap();
        legacy.as_object_mut().unwrap().remove("database_password");
        legacy.as_object_mut().unwrap().remove("auth_snapshot");
        let restored: AccountState = serde_json::from_value(legacy).unwrap();
        restored.validate().unwrap();
        assert!(restored.database_password.is_empty());
    }
    #[test]
    fn refresh_identity_and_clear_tokens() {
        let old = state();
        let inner = Ecdh::generate(Nid::SECP224R1).unwrap();
        let auth = p::message(&[
            p::uint(1, old.uin as u64),
            p::bytes(3, &p::buffer(&[1; 16])),
            p::uint(19, 0),
        ])
        .unwrap();
        let reply = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 1),
            p::bytes(3, &auth),
            p::bytes(4, &p::bytes(1, old.own_username.as_bytes())),
        ])
        .unwrap();
        let next = reduce_auth(&reply, Some(&old), &inner, &old.device, &[]).unwrap();
        assert!(next.auto_auth_key.is_empty());
        assert!(next.request_key.is_empty());
        let bad = p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::uint(2, 0),
            p::bytes(4, &p::bytes(1, b"foreign")),
        ])
        .unwrap();
        assert!(reduce_auth(&bad, Some(&old), &inner, &old.device, &[]).is_err());
    }
}
