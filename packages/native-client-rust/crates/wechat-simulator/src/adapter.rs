use crate::contract::*;
use anyhow::{bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use std::{
    io::Read,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use wechat_core::upstream::{LoginChallenge, PushConnection, Upstream, UpstreamKind};
use wechat_protocol::{
    push::Notification,
    wire::{Heartbeat, SyncBatch, TextSendResult},
    AccountState, DeviceProfile, LoginStatus, NativeOptions,
};

#[path = "media_adapter.rs"]
mod media;

#[derive(Clone)]
pub struct SimulatorUpstream {
    address: SocketAddr,
    token: Arc<String>,
}
impl SimulatorUpstream {
    pub fn new(address: SocketAddr, token: String) -> Result<Self> {
        ensure!(
            address.ip().is_loopback() && address.port() != 0,
            "simulator_requires_loopback"
        );
        ensure!(
            token.len() >= 32 && token.len() <= 256 && !token.chars().any(char::is_control),
            "invalid_simulator_token"
        );
        Ok(Self {
            address,
            token: Arc::new(token),
        })
    }
    fn rpc<T: DeserializeOwned>(&self, request: &Request) -> Result<T> {
        // Numeric loopback only: do not send local simulator credentials to a proxy,
        // follow redirects or resolve an externally supplied hostname.
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()?;
        let response = client
            .post(format!("http://{}/rpc", self.address))
            .bearer_auth(self.token.as_str())
            .json(request)
            .send()?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .take(MAX_MEDIA_BODY as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_MEDIA_BODY,
            "simulator_response_too_large"
        );
        if !status.is_success() {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let code = value["error"]
                .as_str()
                .unwrap_or("simulator_request_failed");
            if code == "auth_required" {
                bail!("Heartbeat returned -13");
            }
            bail!("simulator_request_failed: {code}");
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn session(&self, state: &AccountState) -> Result<Session> {
        self.validate_state(state)?;
        Ok(Session {
            account: state
                .own_username
                .strip_prefix("sim_")
                .context("invalid_simulator_identity")?
                .into(),
            generation: u64::from_le_bytes(state.cookie.as_slice().try_into()?),
        })
    }
    fn challenge(&self, request: Request) -> Result<Box<dyn LoginChallenge>> {
        let challenge = self.rpc(&request)?;
        Ok(Box::new(SimulatorChallenge {
            upstream: self.clone(),
            challenge,
        }))
    }
}
impl Upstream for SimulatorUpstream {
    fn notify_read(
        &self,
        state: &mut AccountState,
        notification: &wechat_protocol::SessionReadNotification,
    ) -> Result<wechat_protocol::SessionReadReply> {
        let _: serde_json::Value = self.rpc(&Request::Read {
            session: self.session(state)?,
            notification: notification.clone(),
        })?;
        Ok(wechat_protocol::SessionReadReply {
            accepted: true,
            service_code: 0,
        })
    }
    fn lookup_contacts_raw(&self, state: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
        self.rpc(&Request::Contacts {
            session: self.session(state)?,
            ids: ids.to_vec(),
        })
    }
    fn download_avatar(&self, state: &mut AccountState, source: &str) -> Result<Vec<u8>> {
        let username = source
            .strip_prefix("agentwechat-simulator://avatar/")
            .context("unsupported_simulator_avatar")?;
        self.rpc(&Request::Avatar {
            session: self.session(state)?,
            username: username.into(),
        })
    }
    fn media_transport(
        &self,
        state: &mut AccountState,
    ) -> Result<Box<dyn wechat_core::upstream::media::MediaTransport>> {
        Ok(Box::new(media::SimulatorMedia::new(
            self.clone(),
            self.session(state)?,
        )))
    }
    fn submit_media(
        &self,
        state: &mut AccountState,
        input: wechat_core::upstream::media::MediaSubmission<'_>,
    ) -> Result<wechat_protocol::media_send::MediaSendResult> {
        media::submit(self, self.session(state)?, input)
    }
    fn download_voice(
        &self,
        state: &mut AccountState,
        server_id: u64,
        length: u32,
        offset: u32,
    ) -> Result<wechat_protocol::wire::VoiceChunk> {
        media::voice(self, self.session(state)?, server_id, length, offset)
    }
    fn kind(&self) -> UpstreamKind {
        UpstreamKind::Simulator
    }
    fn download_sticker(
        &self,
        state: &mut AccountState,
        descriptor: &wechat_protocol::media::StickerDescriptor,
        progress: wechat_core::upstream::media::Progress<'_>,
    ) -> Result<(Vec<u8>, wechat_protocol::media::ImageInspection)> {
        media::sticker(self, self.session(state)?, descriptor, progress)
    }
    fn validate_state(&self, state: &AccountState) -> Result<()> {
        state.validate()?;
        ensure!(
            state.own_username.starts_with("sim_")
                && state.cookie.len() == 8
                && state.long_host == HOST
                && state.short_host == HOST,
            "invalid_simulator_identity"
        );
        Ok(())
    }
    fn start_qr_login(
        &self,
        account: &str,
        _options: NativeOptions,
        device: DeviceProfile,
        computer_name: &str,
        _key_info: &[u8],
    ) -> Result<Box<dyn LoginChallenge>> {
        self.challenge(Request::StartLogin {
            account: account.into(),
            device,
            computer_name: computer_name.into(),
            saved: false,
        })
    }
    fn start_saved_device_login(
        &self,
        account: &str,
        state: &AccountState,
        computer_name: &str,
    ) -> Result<Box<dyn LoginChallenge>> {
        let _: Heartbeat = self.rpc(&Request::Heartbeat {
            session: self.session(state)?,
            cursor: vec![],
        })?;
        self.challenge(Request::StartLogin {
            account: account.into(),
            device: state.device.clone(),
            computer_name: computer_name.into(),
            saved: true,
        })
    }
    fn logout(&self, state: &mut AccountState) -> Result<(bool, i32)> {
        let reply: SendReply = self.rpc(&Request::Logout {
            session: self.session(state)?,
        })?;
        Ok((reply.accepted, 0))
    }
    #[allow(clippy::too_many_arguments)]
    fn start_profile_saved_device_login(
        &self,
        _options: NativeOptions,
        device: &DeviceProfile,
        username: &str,
        _auto_auth_key: &[u8],
        cookie: &[u8],
        _key_info: &[u8],
        computer_name: &str,
    ) -> Result<Box<dyn LoginChallenge>> {
        let account = username
            .strip_prefix("sim_")
            .context("invalid_simulator_identity")?;
        let session = Session {
            account: account.into(),
            generation: u64::from_le_bytes(cookie.try_into()?),
        };
        let _: Heartbeat = self.rpc(&Request::Heartbeat {
            session,
            cursor: vec![],
        })?;
        self.challenge(Request::StartLogin {
            account: account.into(),
            device: device.clone(),
            computer_name: computer_name.into(),
            saved: true,
        })
    }
    fn renew_session(&self, state: &AccountState, _computer_name: &str) -> Result<AccountState> {
        self.heartbeat(&mut state.clone(), &[])?;
        Ok(state.clone())
    }
    fn heartbeat(&self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat> {
        self.rpc(&Request::Heartbeat {
            session: self.session(state)?,
            cursor: cursor.into(),
        })
    }
    fn sync(&self, state: &mut AccountState, cursor: &[u8]) -> Result<SyncBatch> {
        self.rpc(&Request::Sync {
            session: self.session(state)?,
            cursor: cursor.into(),
        })
    }
    fn send_text(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
    ) -> Result<TextSendResult> {
        let reply: SendReply = self.rpc(&Request::SendText {
            session: self.session(state)?,
            destination: destination.into(),
            content: content.into(),
            created_at,
            client_id: client_message_id,
        })?;
        Ok(TextSendResult {
            accepted: reply.accepted,
            outer_code: 0,
            item_code: Some(if reply.accepted { 0 } else { -1 }),
            server_message_id: reply.server_id,
            returned_destination_matches: true,
            returned_client_id_matches: true,
        })
    }
    fn push_connection(&self, state: &AccountState) -> Result<Box<dyn PushConnection>> {
        Ok(Box::new(SimulatorPush {
            upstream: self.clone(),
            session: self.session(state)?,
            sequence: 0,
        }))
    }
}

struct SimulatorChallenge {
    upstream: SimulatorUpstream,
    challenge: Challenge,
}
impl LoginChallenge for SimulatorChallenge {
    fn qr_png(&self) -> &[u8] {
        &self.challenge.qr_png
    }
    fn expires_at(&self) -> u32 {
        self.challenge.expires_at
    }
    fn poll(
        &mut self,
        deadline: Instant,
        cancelled: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<LoginStatus> {
        ensure!(
            !cancelled() && Instant::now() < deadline,
            "simulator_login_cancelled"
        );
        self.challenge = self.upstream.rpc(&Request::PollLogin {
            challenge: self.challenge.id.clone(),
        })?;
        Ok(self.challenge.status)
    }
    fn complete(&mut self) -> Result<AccountState> {
        let reply: Authorization = self.upstream.rpc(&Request::CompleteLogin {
            challenge: self.challenge.id.clone(),
        })?;
        self.upstream.validate_state(&reply.state)?;
        Ok(reply.state)
    }
    fn refresh(&self) -> Result<Box<dyn LoginChallenge>> {
        self.upstream.challenge(Request::RefreshLogin {
            challenge: self.challenge.id.clone(),
        })
    }
}
struct SimulatorPush {
    upstream: SimulatorUpstream,
    session: Session,
    sequence: u64,
}
impl PushConnection for SimulatorPush {
    fn heartbeat(&mut self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat> {
        self.upstream.heartbeat(state, cursor)
    }
    fn sync(&mut self, state: &mut AccountState, cursor: &[u8], _scene: u32) -> Result<SyncBatch> {
        self.upstream.sync(state, cursor)
    }
    fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>> {
        let session = Session {
            account: self.session.account.clone(),
            generation: self.session.generation,
        };
        let reply: PollReply = self.upstream.rpc(&Request::Poll {
            session,
            after: self.sequence,
        })?;
        self.sequence = reply.sequence;
        if reply.notified {
            return Ok(Some(Notification {
                command: 24,
                selector: 1,
            }));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(100)));
        Ok(None)
    }
}
