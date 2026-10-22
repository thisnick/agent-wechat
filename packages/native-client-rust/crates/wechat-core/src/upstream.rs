//! Decoded upstream operations. Account admission, persistence and projection stay in Core.
use anyhow::{bail, ensure, Result};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use wechat_protocol::{
    media_send::MediaSendResult,
    push::Notification,
    wire::{Heartbeat, SyncBatch, TextSendResult},
    AccountState, DeviceProfile, LoginAttempt, LoginStatus, NativeClient, NativeOptions,
};

pub const SIMULATOR_MARKER: &str = ".agent-wechat-simulator";
pub const SIMULATOR_MARKER_CONTENT: &str = "agent-wechat-simulator-v1\n";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UpstreamKind {
    Native,
    Simulator,
}

/// An issued challenge retains its transport state across poll failures.
pub trait LoginChallenge: Send {
    fn qr_png(&self) -> &[u8];
    fn expires_at(&self) -> u32;
    fn lan_login_state(&self) -> Option<&'static str> {
        None
    }
    fn poll(
        &mut self,
        deadline: Instant,
        cancelled: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<LoginStatus>;
    fn complete(&mut self) -> Result<AccountState>;
    fn refresh(&self) -> Result<Box<dyn LoginChallenge>>;
}

/// Notifications remain hints; Core performs and commits the corresponding sync.
pub trait PushConnection: Send {
    fn heartbeat(&mut self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat>;
    fn sync(&mut self, state: &mut AccountState, cursor: &[u8], scene: u32) -> Result<SyncBatch>;
    fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>>;
}

/// Simulation implements decoded operations explicitly. All remaining native
/// operations fail closed through `native`, including secondary media paths.
pub trait Upstream: Send + Sync {
    fn kind(&self) -> UpstreamKind;
    fn validate_state(&self, state: &AccountState) -> Result<()> {
        state.validate()
    }
    fn native(&self, _options: NativeOptions) -> Result<NativeClient> {
        bail!("unsupported_simulator_operation")
    }
    fn start_qr_login(
        &self,
        _account: &str,
        options: NativeOptions,
        device: DeviceProfile,
        computer_name: &str,
        key_info: &[u8],
    ) -> Result<Box<dyn LoginChallenge>> {
        let client = self.native(options)?;
        let attempt = client.start_profile_qr_login(device, computer_name, key_info)?;
        Ok(Box::new(NativeChallenge { client, attempt }))
    }
    fn start_saved_device_login(
        &self,
        _account: &str,
        state: &AccountState,
        computer_name: &str,
    ) -> Result<Box<dyn LoginChallenge>> {
        let client = self.native(NativeOptions::default())?;
        let attempt = client.start_saved_device_login(state, computer_name)?;
        Ok(Box::new(NativeChallenge { client, attempt }))
    }
    #[allow(clippy::too_many_arguments)]
    fn start_profile_saved_device_login(
        &self,
        options: NativeOptions,
        device: &DeviceProfile,
        username: &str,
        auto_auth_key: &[u8],
        cookie: &[u8],
        key_info: &[u8],
        computer_name: &str,
    ) -> Result<Box<dyn LoginChallenge>> {
        let client = self.native(options)?;
        let attempt = client.start_profile_saved_device_login(
            device,
            username,
            auto_auth_key,
            cookie,
            key_info,
            computer_name,
        )?;
        Ok(Box::new(NativeChallenge { client, attempt }))
    }
    fn renew_session(&self, state: &AccountState, computer_name: &str) -> Result<AccountState> {
        self.native(NativeOptions::default())?
            .renew_session(state, computer_name)
    }
    fn heartbeat(&self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat> {
        self.native(NativeOptions::default())?
            .heartbeat(state, cursor)
    }
    fn sync(&self, state: &mut AccountState, cursor: &[u8]) -> Result<SyncBatch> {
        self.native(NativeOptions::default())?.sync(state, cursor)
    }
    fn logout(&self, state: &mut AccountState) -> Result<(bool, i32)> {
        let reply = self.native(NativeOptions::default())?.logout(state)?;
        Ok((reply.accepted, reply.service_code))
    }
    fn send_text(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
    ) -> Result<TextSendResult> {
        self.native(NativeOptions::default())?.send_text(
            state,
            destination,
            content,
            created_at,
            client_message_id,
        )
    }
    fn send_share(
        &self,
        state: &mut AccountState,
        destination: &str,
        client_id: &str,
        created_at: u32,
        content: &str,
    ) -> Result<MediaSendResult> {
        self.native(NativeOptions::default())?.send_share(
            state,
            destination,
            client_id,
            created_at,
            content,
        )
    }
    fn push_connection(&self, state: &AccountState) -> Result<Box<dyn PushConnection>> {
        Ok(Box::new(
            self.native(NativeOptions::default())?
                .push_connection(state)?,
        ))
    }
}

pub(crate) struct NativeUpstream;
impl Upstream for NativeUpstream {
    fn kind(&self) -> UpstreamKind {
        UpstreamKind::Native
    }
    fn native(&self, options: NativeOptions) -> Result<NativeClient> {
        NativeClient::new(options)
    }
}

struct NativeChallenge {
    client: NativeClient,
    attempt: LoginAttempt,
}
impl LoginChallenge for NativeChallenge {
    fn qr_png(&self) -> &[u8] {
        &self.attempt.qr_png
    }
    fn expires_at(&self) -> u32 {
        self.attempt.expires_at
    }
    fn lan_login_state(&self) -> Option<&'static str> {
        self.attempt.lan_login_state()
    }
    fn poll(
        &mut self,
        deadline: Instant,
        cancelled: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<LoginStatus> {
        self.client
            .poll_login_guarded(&mut self.attempt, deadline, cancelled)
    }
    fn complete(&mut self) -> Result<AccountState> {
        self.client.complete_login(&mut self.attempt)
    }
    fn refresh(&self) -> Result<Box<dyn LoginChallenge>> {
        let client = NativeClient::new(NativeOptions::default())?;
        let attempt = client.refresh_qr_login(&self.attempt)?;
        Ok(Box::new(Self { client, attempt }))
    }
}
impl PushConnection for wechat_protocol::push::Connection {
    fn heartbeat(&mut self, state: &mut AccountState, cursor: &[u8]) -> Result<Heartbeat> {
        self.heartbeat(state, cursor)
    }
    fn sync(&mut self, state: &mut AccountState, cursor: &[u8], scene: u32) -> Result<SyncBatch> {
        self.sync(state, cursor, scene)
    }
    fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>> {
        self.poll(timeout)
    }
}

pub(crate) fn validate_store(store: &wechat_store::Store, kind: UpstreamKind) -> Result<()> {
    let marker = store.root.join(SIMULATOR_MARKER);
    match kind {
        UpstreamKind::Native => ensure!(
            !marker.try_exists()?,
            "simulator_store_requires_dev_runtime"
        ),
        UpstreamKind::Simulator => {
            let metadata = std::fs::symlink_metadata(&marker)?;
            ensure!(
                metadata.is_file()
                    && !wechat_platform::is_link(&metadata)
                    && metadata.len() == SIMULATOR_MARKER_CONTENT.len() as u64
                    && std::fs::read_to_string(marker)? == SIMULATOR_MARKER_CONTENT,
                "simulator_requires_isolated_store"
            );
            let root = std::fs::canonicalize(&store.root)?;
            for account in store.accounts()? {
                ensure!(
                    account.storage_mode == "managed",
                    "simulator_requires_managed_profiles"
                );
                ensure!(
                    std::fs::canonicalize(&account.profile_path)?.starts_with(&root),
                    "simulator_requires_isolated_profiles"
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn require_native(kind: UpstreamKind) -> Result<()> {
    ensure!(
        kind == UpstreamKind::Native,
        "unsupported_simulator_operation"
    );
    Ok(())
}

pub(crate) fn validate_profile_path(root: &Path, profile: &Path) -> Result<()> {
    ensure!(
        std::fs::canonicalize(profile)?.starts_with(std::fs::canonicalize(root)?),
        "simulator_requires_isolated_profiles"
    );
    Ok(())
}
