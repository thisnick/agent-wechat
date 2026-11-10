//! Simulation and bootstrap dispatch. NativeClient owns live account state;
//! Core retains daemon permissions, journals and official-profile integration.
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

#[path = "upstream_media.rs"]
pub mod media;

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
    /// Native account dispatch is supplied by Core; simulators implement decoded operations.
    fn uses_account_owner(&self) -> bool {
        false
    }
    fn group_mutation_raw(
        &self,
        _state: &mut AccountState,
        _request: &wechat_protocol::GroupRpc,
    ) -> Result<Vec<u8>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn group_detail_raw(&self, _state: &mut AccountState, _room: &str) -> Result<Vec<u8>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn lookup_room_raw(
        &self,
        _state: &mut AccountState,
        _room: &str,
        _version: u32,
    ) -> Result<Vec<u8>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn lookup_room_receipt(
        &self,
        _state: &mut AccountState,
        _room: &str,
        _version: u32,
    ) -> Result<wechat_protocol::MetadataReply<wechat_protocol::RoomRoster>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn contact_patch(
        &self,
        record: &wechat_protocol::ContactRecord,
    ) -> Result<wechat_profile::ContactProjection> {
        native_contact_patch(record)
    }
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
    fn renew_session(&self, _state: &AccountState, _computer_name: &str) -> Result<AccountState> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn heartbeat(&self, _state: &mut AccountState, _cursor: &[u8]) -> Result<Heartbeat> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn sync(&self, _state: &mut AccountState, _cursor: &[u8]) -> Result<SyncBatch> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn contact_sync(
        &self,
        _state: &mut AccountState,
        _cursor: &[u8],
        _message_cursor: &[u8],
        _continuation: bool,
    ) -> Result<wechat_protocol::contact_sync::ContactSyncBatch> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn logout(&self, _state: &mut AccountState) -> Result<(bool, i32)> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn send_text(
        &self,
        _state: &mut AccountState,
        _destination: &str,
        _content: &str,
        _created_at: u32,
        _client_message_id: u32,
    ) -> Result<TextSendResult> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn send_text_with_source(
        &self,
        _state: &mut AccountState,
        _destination: &str,
        _content: &str,
        _created_at: u32,
        _client_message_id: u32,
        _source: &str,
    ) -> Result<TextSendResult> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn send_text_guarded(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
        before_submit: &mut dyn FnMut() -> Result<()>,
    ) -> Result<TextSendResult> {
        before_submit()?;
        self.send_text(state, destination, content, created_at, client_message_id)
    }
    fn send_quote(
        &self,
        _state: &mut AccountState,
        _destination: &str,
        _client_id: &str,
        _created_at: u32,
        _content: &str,
        _source: Option<&str>,
    ) -> Result<MediaSendResult> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn send_share(
        &self,
        _state: &mut AccountState,
        _destination: &str,
        _client_id: &str,
        _created_at: u32,
        _content: &str,
    ) -> Result<MediaSendResult> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn media_transport_route(
        &self,
        _state: &mut AccountState,
        _route: Option<wechat_protocol::media::LargeCdnRoute>,
    ) -> Result<Box<dyn media::MediaTransport>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn submit_media(
        &self,
        _state: &mut AccountState,
        _input: media::MediaSubmission<'_>,
    ) -> Result<MediaSendResult> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn download_voice(
        &self,
        _state: &mut AccountState,
        _server_id: u64,
        _length: u32,
        _offset: u32,
    ) -> Result<wechat_protocol::wire::VoiceChunk> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn notify_read(
        &self,
        _state: &mut AccountState,
        _notification: &wechat_protocol::SessionReadNotification,
    ) -> Result<wechat_protocol::SessionReadReply> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    fn lookup_contacts_raw(&self, _state: &mut AccountState, _ids: &[String]) -> Result<Vec<u8>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
    /// Fetch one known OpenIM identity using optional group and ticket context.
    fn lookup_openim_contact_raw(
        &self,
        _state: &mut AccountState,
        _username: &str,
        _room: Option<&str>,
        _ticket: Option<&str>,
    ) -> Result<Vec<u8>> {
        bail!("unsupported_openim_contact_lookup")
    }
    fn download_avatar(&self, _state: &mut AccountState, source: &str) -> Result<Vec<u8>> {
        require_native(self.kind())?;
        wechat_protocol::media::download_advertised_avatar(source)
    }
    fn download_sticker(
        &self,
        _state: &mut AccountState,
        descriptor: &wechat_protocol::media::StickerDescriptor,
        progress: media::Progress<'_>,
    ) -> Result<(Vec<u8>, wechat_protocol::media::ImageInspection)> {
        require_native(self.kind())?;
        wechat_protocol::media::fetch_sticker(descriptor, progress)
    }
    fn push_connection(&self, _state: &AccountState) -> Result<Box<dyn PushConnection>> {
        bail!(
            "{}",
            if self.kind() == UpstreamKind::Simulator {
                "unsupported_simulator_operation"
            } else {
                "account_runtime_required"
            }
        )
    }
}

pub(crate) struct NativeUpstream;
impl Upstream for NativeUpstream {
    fn uses_account_owner(&self) -> bool {
        true
    }
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

/// Decode known contact fields without inventing local deletion state.
pub fn native_contact_patch(
    record: &wechat_protocol::ContactRecord,
) -> Result<wechat_profile::ContactProjection> {
    crate::runtime::metadata::contact_patch(record)
}
