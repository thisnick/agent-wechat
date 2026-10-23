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
    fn group_mutation_raw(
        &self,
        state: &mut AccountState,
        request: &wechat_protocol::GroupRpc,
    ) -> Result<Vec<u8>> {
        self.native(NativeOptions::default())?
            .group_mutation_raw(state, request)
    }
    fn group_detail_raw(&self, state: &mut AccountState, room: &str) -> Result<Vec<u8>> {
        self.native(NativeOptions::default())?
            .group_detail_raw(state, room)
    }
    fn lookup_room_raw(
        &self,
        state: &mut AccountState,
        room: &str,
        version: u32,
    ) -> Result<Vec<u8>> {
        self.native(NativeOptions::default())?
            .lookup_room_raw(state, room, version)
    }
    fn lookup_room_receipt(
        &self,
        state: &mut AccountState,
        room: &str,
        version: u32,
    ) -> Result<wechat_protocol::MetadataReply<wechat_protocol::RoomRoster>> {
        self.native(NativeOptions::default())?
            .lookup_room_receipt(state, room, version)
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
    fn media_transport_route(
        &self,
        state: &mut AccountState,
        route: Option<wechat_protocol::media::LargeCdnRoute>,
    ) -> Result<Box<dyn media::MediaTransport>> {
        let native = self.native(NativeOptions::default())?;
        let load = |state: &mut AccountState| match route {
            Some(route) => native.large_cdn_config(state, route),
            None => native.cdn_config(state),
        };
        let mut conf = match load(state) {
            Ok(conf) => conf,
            Err(error) if crate::media::is_media_auth_rejected(&error) => {
                *state = native.renew_session(state, "agent-wechat media")?;
                load(state)?
            }
            Err(error) => return Err(error),
        };
        let cdn = wechat_protocol::media::CdnClient::new(conf.clone(), Duration::from_secs(25))?;
        conf.auth_key.fill(0);
        Ok(Box::new(cdn))
    }
    fn submit_media(
        &self,
        state: &mut AccountState,
        input: media::MediaSubmission<'_>,
    ) -> Result<MediaSendResult> {
        let native = self.native(NativeOptions::default())?;
        match input.body {
            media::MediaBody::File {
                upload,
                reservation,
            } => native.send_file_with_reservation(
                state,
                input.destination,
                input.client_id,
                input.created_at,
                upload,
                reservation,
            ),
            media::MediaBody::Image(upload) => {
                native.send_image(state, input.destination, input.client_id, upload)
            }
            media::MediaBody::Video { upload, seconds } => {
                native.send_video(state, input.destination, input.client_id, upload, seconds)
            }
            media::MediaBody::Voice {
                encoded,
                duration_ms,
            } => native.send_voice(
                state,
                input.destination,
                input.client_id,
                input.created_at,
                duration_ms,
                encoded,
            ),
        }
    }
    fn download_voice(
        &self,
        state: &mut AccountState,
        server_id: u64,
        length: u32,
        offset: u32,
    ) -> Result<wechat_protocol::wire::VoiceChunk> {
        self.native(NativeOptions::default())?
            .download_voice_chunk(state, server_id, length, offset)
    }
    fn notify_read(
        &self,
        state: &mut AccountState,
        notification: &wechat_protocol::SessionReadNotification,
    ) -> Result<wechat_protocol::SessionReadReply> {
        self.native(NativeOptions::default())?
            .notify_read_session(state, notification)
    }
    fn lookup_contacts_raw(&self, state: &mut AccountState, ids: &[String]) -> Result<Vec<u8>> {
        self.native(NativeOptions::default())?
            .lookup_contacts_raw(state, ids)
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

/// Decode known contact fields without inventing local deletion state.
pub fn native_contact_patch(
    record: &wechat_protocol::ContactRecord,
) -> Result<wechat_profile::ContactProjection> {
    crate::runtime::metadata::contact_patch(record)
}
