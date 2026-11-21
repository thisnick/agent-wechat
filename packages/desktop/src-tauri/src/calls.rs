//! Native desktop voice and video calling bridge built on `wechat-call-client`
//! and `wechat-audio`.
//!
//! All bearer credentials, custom CA roots, and `ownerToken` values stay inside
//! Rust. The WebView receives sanitized Calls Protocol v3 events, track lists,
//! normalized audio levels, and raw JPEG video frames over Tauri IPC channels.

use crate::{hub, Desktop};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{ipc::Channel, State};
use tokio::{
    sync::{mpsc, oneshot, Mutex},
    time::{interval, timeout, MissedTickBehavior},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use wechat_call_client::{
    audio::{AudioDeviceOptions, LocalAudio, SignalLevel},
    media::MediaClient,
    transport::{
        debug_call, event_json, send_json, CallClientError, CallEndpoint, ControlClient,
        OwnerResult,
    },
    video::{self, extract_jpeg_frame, CameraSource, CapturedVideoFrame, LocalVideoCapture},
};
use wechat_call_contracts::{
    call_types::{
        CallCommand, CallCommandStatus, CallEvent, CallReadyScope, CallTarget, CallTrack, CallType,
        CallVideoCodec,
    },
    pcm_envelope::{
        MediaPayloadKind, WireMediaFrame, FLAG_CODEC_CONFIG, FLAG_KEYFRAME, MAX_VIDEO_PAYLOAD_BYTES,
    },
};
use wechat_calls::audio::AudioFormat;

const MAX_ID_LEN: usize = 256;
const MAX_REQUEST_ID_LEN: usize = 128;
const MAX_CAMERA_ID_LEN: usize = 256;
const MAX_INVITEES: usize = 100;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const SELF_VIEW_MIN_INTERVAL: Duration = Duration::from_millis(80);
const PREVIEW_POLL_INTERVAL: Duration = Duration::from_millis(66);
const LEVEL_INTERVAL: Duration = Duration::from_millis(80);

/// Packet pushed from an account's native call actor to the WebView.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DesktopCallPacket {
    Event {
        event: Value,
    },
    TracksSynced {
        tracks: Vec<CallTrack>,
    },
    RemoteFrame {
        #[serde(rename = "trackId")]
        track_id: u32,
        #[serde(rename = "participantId", skip_serializing_if = "Option::is_none")]
        participant_id: Option<String>,
        #[serde(rename = "rotationDegrees")]
        rotation_degrees: u16,
        jpeg: String,
    },
    LocalJpeg {
        jpeg: String,
    },
    TrackCleared {
        #[serde(rename = "trackId")]
        track_id: u32,
    },
    Levels {
        #[serde(rename = "micLevel")]
        mic_level: f32,
        #[serde(rename = "speakerLevel")]
        speaker_level: f32,
        #[serde(rename = "trackLevels")]
        track_levels: BTreeMap<u32, f32>,
    },
    MediaError {
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Disconnected {
        code: String,
    },
}

/// Packet pushed from the standalone pre-call camera preview to the WebView.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DesktopPreviewPacket {
    Frame { jpeg: String },
    Ended,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSelection {
    #[serde(default)]
    pub input_device_id: Option<String>,
    #[serde(default)]
    pub output_device_id: Option<String>,
    #[serde(default)]
    pub camera_device_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallDeviceDto {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallDevicesSnapshotDto {
    pub audio_inputs: Vec<CallDeviceDto>,
    pub audio_outputs: Vec<CallDeviceDto>,
    pub video_inputs: Vec<CallDeviceDto>,
    pub permission_state: &'static str,
    pub supports_output_selection: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallStartResponse {
    pub call_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialOptions {
    pub call_type: CallType,
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub camera_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinOptions {
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub camera_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerOptions {
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub camera_enabled: Option<bool>,
}

#[derive(Default)]
pub struct CallManager {
    next_session: AtomicU64,
    accounts: Mutex<HashMap<String, AccountHandle>>,
    preview: Mutex<Option<PreviewSession>>,
}

struct AccountHandle {
    session_id: u64,
    connection_id: String,
    cancel: CancellationToken,
    tx: mpsc::UnboundedSender<ActorRequest>,
    task: tauri::async_runtime::JoinHandle<()>,
}

struct PreviewSession {
    cancel: CancellationToken,
    task: tauri::async_runtime::JoinHandle<()>,
}

enum ActorRequest {
    Command {
        command: CallCommand,
        reply: oneshot::Sender<Result<OwnerResult, String>>,
    },
    AttachMedia {
        call_id: String,
        call_type: CallType,
        initial_muted: bool,
        initial_camera_enabled: bool,
        devices: DeviceSelection,
        speaker_muted: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SetLocalMute {
        muted: bool,
    },
    SetSpeakerMuted {
        muted: bool,
    },
    SetLocalCamera {
        enabled: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    SwitchDevices {
        selection: DeviceSelection,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StopLocalMedia,
    StopMedia,
}

impl CallManager {
    pub async fn stop_preview(&self) {
        if let Some(preview) = self.preview.lock().await.take() {
            preview.cancel.cancel();
            let _ = timeout(Duration::from_secs(3), preview.task).await;
        }
    }

    pub async fn disconnect_account(&self, account_id: &str) {
        if let Some(handle) = self.accounts.lock().await.remove(account_id) {
            handle.cancel.cancel();
            let _ = timeout(Duration::from_secs(4), handle.task).await;
        }
    }

    /// Tear down all active call sessions and camera previews (called on hub switch,
    /// disconnect, or app shutdown so audio/camera hardware is never left open).
    pub async fn shutdown_all(&self) {
        self.stop_preview().await;
        let drained: Vec<AccountHandle> = {
            let mut accounts = self.accounts.lock().await;
            accounts.drain().map(|(_, handle)| handle).collect()
        };
        for handle in &drained {
            handle.cancel.cancel();
        }
        for handle in drained {
            let _ = timeout(Duration::from_secs(4), handle.task).await;
        }
    }

    async fn sender(
        &self,
        connection_id: &str,
        account_id: &str,
    ) -> Result<mpsc::UnboundedSender<ActorRequest>, String> {
        let accounts = self.accounts.lock().await;
        let handle = accounts
            .get(account_id)
            .filter(|handle| handle.connection_id == connection_id)
            .ok_or_else(|| "not_connected".to_string())?;
        Ok(handle.tx.clone())
    }
}

pub(crate) fn resolve_ffmpeg(binaries: &Path) -> Option<PathBuf> {
    hub::executable(binaries, "ffmpeg")
        .ok()
        .or_else(|| video::find_ffmpeg(None))
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty()
        || account_id.len() > 128
        || !account_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err("Invalid account identity".into());
    }
    Ok(())
}

fn validate_non_empty_id(value: &str, label: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max_len || value.chars().any(char::is_control) {
        return Err(format!("Invalid {label}"));
    }
    Ok(())
}

/// Convert an IPC camera device ID into a safe `CameraSource`.
/// Never allows `file:` or `test` synthetic sources from WebView IPC.
fn camera_source_from_ipc(camera_id: Option<&str>) -> Result<CameraSource, String> {
    let Some(raw) = camera_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(CameraSource::Auto);
    };
    if raw.len() > MAX_CAMERA_ID_LEN
        || raw.chars().any(char::is_control)
        || raw.starts_with('-')
        || raw.eq_ignore_ascii_case("off")
        || raw.eq_ignore_ascii_case("none")
        || raw.eq_ignore_ascii_case("test")
        || raw.to_ascii_lowercase().starts_with("file:")
    {
        return Err("Invalid camera device".into());
    }
    #[cfg(target_os = "windows")]
    if raw.contains(':') {
        return Err("Invalid camera device".into());
    }
    Ok(CameraSource::Device(raw.to_owned()))
}

fn sanitize_device_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|id| {
            !id.is_empty() && id.len() <= MAX_CAMERA_ID_LEN && !id.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn map_connect_error(error: &anyhow::Error) -> String {
    if let Some(CallClientError::SocketRejected { status }) =
        error.downcast_ref::<CallClientError>()
    {
        if matches!(*status, 401 | 403) {
            return "forbidden".into();
        }
        return format!("socket_rejected_{status}");
    }
    error.to_string()
}

fn sanitized_event_value(event: &CallEvent) -> Result<Value> {
    let json = event_json(event)?;
    Ok(serde_json::from_str(&json)?)
}

fn all_media_tracks(media: &MediaClient) -> Vec<CallTrack> {
    let mut tracks = Vec::with_capacity(
        1 + usize::from(media.video_send_track.is_some())
            + media.receive.track_count()
            + media.receive_video.tracks().len(),
    );
    tracks.push(media.send_track.clone());
    if let Some(video_send) = &media.video_send_track {
        tracks.push(video_send.clone());
    }
    tracks.extend(media.receive.tracks().cloned());
    tracks.extend(
        media
            .receive_video
            .tracks()
            .values()
            .map(|entry| entry.track.clone()),
    );
    tracks
}

/// Optional secondary FFmpeg decoder that turns outgoing Annex-B H.264/H.265 frames
/// into low-resolution self-view JPEGs without opening the camera twice.
struct SelfViewDecoder {
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    input_tx: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    output_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    wait_for_keyframe: bool,
}

impl SelfViewDecoder {
    fn start(ffmpeg: &Path, codec: CallVideoCodec) -> Option<Self> {
        let input_fmt = match codec {
            CallVideoCodec::H264 => "h264",
            CallVideoCodec::H265 => "hevc",
            CallVideoCodec::Jpeg => return None,
        };
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-f",
            input_fmt,
            "-i",
            "pipe:0",
            "-vf",
            "fps=10,scale=w='min(iw,320)':h='min(ih,240)':force_original_aspect_ratio=decrease",
            "-c:v",
            "mjpeg",
            "-q:v",
            "8",
            "-flush_packets",
            "1",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
        let (output_tx, output_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);

        let writer_stop = stop.clone();
        let writer = std::thread::Builder::new()
            .name("desktop-selfview-in".into())
            .spawn(move || {
                while !writer_stop.load(Ordering::Acquire) {
                    let Ok(bytes) = input_rx.recv() else {
                        break;
                    };
                    if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                        break;
                    }
                }
            })
            .ok()?;

        let reader_stop = stop.clone();
        let reader = std::thread::Builder::new()
            .name("desktop-selfview-out".into())
            .spawn(move || {
                let mut buf = Vec::with_capacity(32 * 1024);
                let mut chunk = [0u8; 8192];
                while !reader_stop.load(Ordering::Acquire) {
                    let n = match stdout.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(frame) = extract_jpeg_frame(&mut buf) {
                        if frame.len() <= MAX_VIDEO_PAYLOAD_BYTES {
                            let _ = output_tx.try_send(frame);
                        }
                    }
                }
            })
            .ok()?;

        Some(Self {
            child: Some(child),
            stop,
            input_tx: Some(input_tx),
            output_rx,
            writer: Some(writer),
            reader: Some(reader),
            wait_for_keyframe: true,
        })
    }

    fn feed(&mut self, frame: &CapturedVideoFrame) {
        let is_idr_with_config = frame.flags & (FLAG_KEYFRAME | FLAG_CODEC_CONFIG)
            == (FLAG_KEYFRAME | FLAG_CODEC_CONFIG);
        if self.wait_for_keyframe && !is_idr_with_config {
            return;
        }
        let Some(tx) = &self.input_tx else {
            return;
        };
        match tx.try_send(frame.bytes.clone()) {
            Ok(()) => self.wait_for_keyframe = false,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                self.wait_for_keyframe = true;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                self.input_tx = None;
            }
        }
    }

    fn drain_latest(&mut self) -> Option<Vec<u8>> {
        let mut latest = None;
        while let Ok(frame) = self.output_rx.try_recv() {
            latest = Some(frame);
        }
        latest
    }
}

impl Drop for SelfViewDecoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.input_tx.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

struct ActiveMediaState {
    call_id: String,
    call_type: CallType,
    media: MediaClient,
    audio_format: AudioFormat,
    local_audio: Option<LocalAudio>,
    local_video: Option<LocalVideoCapture>,
    self_view: Option<SelfViewDecoder>,
    devices: DeviceSelection,
    muted: bool,
    camera_enabled: bool,
    speaker_muted: bool,
    mic_level: SignalLevel,
    speaker_level: SignalLevel,
    track_levels: BTreeMap<u32, SignalLevel>,
    last_local_jpeg: Instant,
}

impl ActiveMediaState {
    async fn stop_local_devices(&mut self) {
        self.self_view = None;
        self.local_video = None;
        if let Some(mut audio) = self.local_audio.take() {
            let _ = audio.stop().await;
        }
        self.mic_level.reset();
        self.speaker_level.reset();
    }

    async fn start_camera(&mut self, ffmpeg: Option<PathBuf>) -> Result<()> {
        self.self_view = None;
        self.local_video = None;
        let Some(video_format) = self
            .media
            .video_send_track
            .as_ref()
            .and_then(|track| track.video_format())
        else {
            return Ok(());
        };
        let source = camera_source_from_ipc(self.devices.camera_device_id.as_deref())
            .map_err(|message| anyhow::anyhow!(message))?;
        let ffmpeg_for_capture = ffmpeg.clone();
        let capture = tokio::task::spawn_blocking(move || {
            LocalVideoCapture::start_with_ffmpeg(
                &source,
                video_format,
                ffmpeg_for_capture.as_deref(),
            )
        })
        .await
        .context("camera capture task panicked")??;
        let self_view = if video_format.codec != CallVideoCodec::Jpeg {
            ffmpeg
                .as_deref()
                .and_then(|path| SelfViewDecoder::start(path, video_format.codec))
        } else {
            None
        };
        self.local_video = Some(capture);
        self.self_view = self_view;
        Ok(())
    }

    async fn close(mut self) {
        self.stop_local_devices().await;
        let _ = timeout(Duration::from_secs(2), self.media.socket.close(None)).await;
    }
}

async fn resolve_active_endpoint(
    state: &Desktop,
    connection_id: &str,
    account_id: &str,
) -> Result<CallEndpoint, String> {
    validate_account_id(account_id)?;
    let active = {
        let inner = state.inner.lock().await;
        inner
            .active
            .clone()
            .filter(|active| active.connection.id == connection_id)
            .ok_or_else(|| "Reconnect to the selected hub".to_string())?
    };
    let base =
        crate::config::endpoint(&active.connection.endpoint).map_err(|_| "Invalid hub address")?;
    Ok(CallEndpoint {
        http: active.http.clone(),
        base,
        token: active.key.as_str().to_owned(),
        account: account_id.to_owned(),
    })
}

async fn send_actor_command(
    tx: &mpsc::UnboundedSender<ActorRequest>,
    command: CallCommand,
) -> Result<OwnerResult, String> {
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(ActorRequest::Command {
        command,
        reply: reply_tx,
    })
    .map_err(|_| "not_connected".to_string())?;
    match timeout(COMMAND_TIMEOUT, reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("closed".into()),
        Err(_) => Err("outcome_unknown".into()),
    }
}

/// Runs one account's persistent Calls Protocol v3 control socket, pending command
/// correlations, and optional connected media loop.
async fn run_account_actor(
    endpoint: CallEndpoint,
    ffmpeg: Option<PathBuf>,
    mut control: ControlClient,
    mut requests: mpsc::UnboundedReceiver<ActorRequest>,
    channel: Channel<DesktopCallPacket>,
    cancel: CancellationToken,
) {
    let mut pending: HashMap<String, oneshot::Sender<Result<OwnerResult, String>>> = HashMap::new();
    let mut owner_tokens: HashMap<String, String> = HashMap::new();
    let mut active_media: Option<ActiveMediaState> = None;

    let mut audio_tick = interval(Duration::from_millis(20));
    audio_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut level_tick = interval(LEVEL_INTERVAL);
    level_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let disconnect_code = loop {
        let has_media = active_media.is_some();
        let has_local_audio = active_media
            .as_ref()
            .is_some_and(|state| state.local_audio.is_some());

        tokio::select! {
            _ = cancel.cancelled() => {
                break None;
            }
            req = requests.recv() => {
                let Some(req) = req else {
                    break None;
                };
                match req {
                    ActorRequest::Command { command, reply } => {
                        let req_id = command.request_id().to_owned();
                        if pending.len() >= 64 {
                            let _ = reply.send(Err("too_many_pending_commands".into()));
                            continue;
                        }
                        if let Err(error) = send_json(&mut control.socket, &command).await {
                            let _ = reply.send(Err(error.to_string()));
                            break Some("closed".to_string());
                        }
                        pending.insert(req_id, reply);
                    }
                    ActorRequest::AttachMedia {
                        call_id,
                        call_type,
                        initial_muted,
                        initial_camera_enabled,
                        devices,
                        speaker_muted,
                        reply,
                    } => {
                        if active_media.as_ref().is_some_and(|m| m.call_id == call_id) {
                            let _ = reply.send(Ok(()));
                            continue;
                        }
                        if let Some(old) = active_media.take() {
                            old.close().await;
                        }
                        let Some(owner_token) = owner_tokens.get(&call_id).cloned() else {
                            let _ = channel.send(DesktopCallPacket::MediaError {
                                code: "missing_owner_token".into(),
                                message: None,
                            });
                            let _ = reply.send(Err("missing_owner_token".into()));
                            continue;
                        };
                        let client_id = control.client_id.clone();
                        let control_format = control.format;
                        match MediaClient::attach_passthrough_parts(
                            &endpoint,
                            &client_id,
                            control_format,
                            &call_id,
                            &owner_token,
                        )
                        .await
                        {
                            Ok(mut media) => {
                                let _ = media.synchronize_tracks(&control, &call_id);
                                if let Some(call) = control.call.as_ref().filter(|c| c.call_id == call_id) {
                                    let _ = media.receive.roster(call);
                                    media.receive_video.roster(call);
                                }
                                let _ = channel.send(DesktopCallPacket::TracksSynced {
                                    tracks: all_media_tracks(&media),
                                });
                                let audio_format = match AudioFormat::new(
                                    control.format.sample_rate,
                                    control.format.channels,
                                    control.format.frame_ms,
                                ) {
                                    Ok(fmt) => fmt,
                                    Err(error) => {
                                        let _ = timeout(Duration::from_secs(2), media.socket.close(None)).await;
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "unsupported_audio_format".into(),
                                            message: Some(error.to_string()),
                                        });
                                        let _ = reply.send(Err("unsupported_audio_format".into()));
                                        continue;
                                    }
                                };
                                let audio_opts = AudioDeviceOptions {
                                    input_device: sanitize_device_id(devices.input_device_id.as_deref()),
                                    output_device: sanitize_device_id(devices.output_device_id.as_deref()),
                                };
                                let local_audio = match LocalAudio::open(&audio_opts).await {
                                    Ok(audio) => Some(audio),
                                    Err(error) => {
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "audio_open_failed".into(),
                                            message: Some(error.to_string()),
                                        });
                                        None
                                    }
                                };
                                let mut state = ActiveMediaState {
                                    call_id,
                                    call_type,
                                    media,
                                    audio_format,
                                    local_audio,
                                    local_video: None,
                                    self_view: None,
                                    devices,
                                    muted: initial_muted,
                                    camera_enabled: initial_camera_enabled && call_type == CallType::Video,
                                    speaker_muted,
                                    mic_level: SignalLevel::default(),
                                    speaker_level: SignalLevel::default(),
                                    track_levels: BTreeMap::new(),
                                    last_local_jpeg: Instant::now() - SELF_VIEW_MIN_INTERVAL,
                                };
                                if state.camera_enabled && state.media.video_send_track.is_some() {
                                    if let Err(error) = state.start_camera(ffmpeg.clone()).await {
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "camera_failed".into(),
                                            message: Some(error.to_string()),
                                        });
                                    }
                                }
                                active_media = Some(state);
                                let _ = reply.send(Ok(()));
                            }
                            Err(error) => {
                                let _ = channel.send(DesktopCallPacket::MediaError {
                                    code: "media_attach_failed".into(),
                                    message: Some(error.to_string()),
                                });
                                let _ = reply.send(Err(error.to_string()));
                            }
                        }
                    }
                    ActorRequest::SetLocalMute { muted } => {
                        if let Some(state) = active_media.as_mut() {
                            state.muted = muted;
                            if muted {
                                state.mic_level.reset();
                            }
                        }
                    }
                    ActorRequest::SetSpeakerMuted { muted } => {
                        if let Some(state) = active_media.as_mut() {
                            state.speaker_muted = muted;
                            if muted {
                                state.speaker_level.reset();
                                if let Some(audio) = state.local_audio.as_mut() {
                                    if let Ok(token) = audio.playback.clear() {
                                        audio.playback_token = token;
                                    }
                                }
                            }
                        }
                    }
                    ActorRequest::SetLocalCamera { enabled, reply } => {
                        let Some(state) = active_media.as_mut() else {
                            let _ = reply.send(Ok(()));
                            continue;
                        };
                        state.camera_enabled = enabled && state.call_type == CallType::Video;
                        if !state.camera_enabled {
                            state.self_view = None;
                            state.local_video = None;
                            let _ = reply.send(Ok(()));
                        } else if state.media.video_send_track.is_some() {
                            match state.start_camera(ffmpeg.clone()).await {
                                Ok(()) => {
                                    let _ = reply.send(Ok(()));
                                }
                                Err(error) => {
                                    let _ = channel.send(DesktopCallPacket::MediaError {
                                        code: "camera_failed".into(),
                                        message: Some(error.to_string()),
                                    });
                                    let _ = reply.send(Err(error.to_string()));
                                }
                            }
                        } else {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    ActorRequest::SwitchDevices { selection, reply } => {
                        let Some(state) = active_media.as_mut() else {
                            let _ = reply.send(Ok(()));
                            continue;
                        };
                        let audio_changed = state.devices.input_device_id != selection.input_device_id
                            || state.devices.output_device_id != selection.output_device_id;
                        let camera_changed = state.devices.camera_device_id != selection.camera_device_id;
                        state.devices = selection;
                        let mut first_err: Option<String> = None;

                        if audio_changed && state.local_audio.is_some() {
                            if let Some(mut old) = state.local_audio.take() {
                                let _ = old.stop().await;
                            }
                            let audio_opts = AudioDeviceOptions {
                                input_device: sanitize_device_id(state.devices.input_device_id.as_deref()),
                                output_device: sanitize_device_id(state.devices.output_device_id.as_deref()),
                            };
                            match LocalAudio::open(&audio_opts).await {
                                Ok(audio) => state.local_audio = Some(audio),
                                Err(error) => {
                                    let message = error.to_string();
                                    let _ = channel.send(DesktopCallPacket::MediaError {
                                        code: "audio_switch_failed".into(),
                                        message: Some(message.clone()),
                                    });
                                    first_err = Some(message);
                                }
                            }
                        }
                        if camera_changed && state.camera_enabled && state.media.video_send_track.is_some() {
                            if let Err(error) = state.start_camera(ffmpeg.clone()).await {
                                let message = error.to_string();
                                let _ = channel.send(DesktopCallPacket::MediaError {
                                    code: "camera_switch_failed".into(),
                                    message: Some(message.clone()),
                                });
                                if first_err.is_none() {
                                    first_err = Some(message);
                                }
                            }
                        }
                        let _ = reply.send(match first_err {
                            Some(err) => Err(err),
                            None => Ok(()),
                        });
                    }
                    ActorRequest::StopLocalMedia => {
                        if let Some(state) = active_media.as_mut() {
                            state.stop_local_devices().await;
                        }
                    }
                    ActorRequest::StopMedia => {
                        if let Some(old) = active_media.take() {
                            old.close().await;
                        }
                    }
                }
            }
            event = control.next_event() => {
                match event {
                    Ok(event) => {
                        control.update(&event);
                        if let CallEvent::CommandResult {
                            request_id,
                            status,
                            call_id,
                            owner_token,
                            code,
                        } = &event
                        {
                            if matches!(
                                status,
                                CallCommandStatus::Accepted | CallCommandStatus::Replayed
                            ) {
                                if let (Some(cid), Some(token)) = (call_id.as_ref(), owner_token.as_ref()) {
                                    owner_tokens.insert(cid.clone(), token.clone());
                                }
                            }
                            if let Some(waiter) = pending.remove(request_id) {
                                if *status == CallCommandStatus::Error {
                                    let err_code = serde_json::to_value(code)
                                        .ok()
                                        .and_then(|v| v.as_str().map(str::to_owned))
                                        .unwrap_or_else(|| "request_failed".into());
                                    let _ = waiter.send(Err(err_code));
                                } else {
                                    let _ = waiter.send(Ok(OwnerResult {
                                        call_id: call_id.clone(),
                                        owner_token: owner_token.clone(),
                                    }));
                                }
                            }
                        }
                        if let CallEvent::State { call } = &event {
                            if matches!(
                                call.state,
                                wechat_call_contracts::call_types::CallState::Ended
                                    | wechat_call_contracts::call_types::CallState::Uncertain
                            ) {
                                owner_tokens.remove(&call.call_id);
                            }
                        }
                        if let Some(state) = active_media.as_mut() {
                            let call_id = state.call_id.clone();
                            let _ = state.media.receive.event(&event, &call_id);
                            let video_changed = state.media.receive_video.event(&event, &call_id);
                            if let CallEvent::TrackRemoved { call_id: cid, track_id, .. } = &event {
                                if cid == &call_id {
                                    state.track_levels.remove(track_id);
                                    if video_changed {
                                        let _ = channel.send(DesktopCallPacket::TrackCleared {
                                            track_id: *track_id,
                                        });
                                    }
                                }
                            }
                            if let CallEvent::State { call } = &event {
                                if call.call_id == call_id {
                                    let _ = state.media.receive.roster(call);
                                    state.media.receive_video.roster(call);
                                }
                            }
                            if let Ok((_, video_send_changed)) = state.media.synchronize_tracks(&control, &call_id) {
                                if video_send_changed && state.camera_enabled && state.media.video_send_track.is_some() {
                                    if let Err(error) = state.start_camera(ffmpeg.clone()).await {
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "camera_failed".into(),
                                            message: Some(error.to_string()),
                                        });
                                    }
                                }
                            }
                        }
                        if let Ok(value) = sanitized_event_value(&event) {
                            if channel.send(DesktopCallPacket::Event { event: value }).is_err() {
                                break None;
                            }
                        }
                    }
                    Err(error) => {
                        debug_call("desktop-control", format_args!("control socket closed: {error:#}"));
                        break Some("closed".to_string());
                    }
                }
            }
            media_msg = async {
                match active_media.as_mut() {
                    Some(state) => state.media.next_message().await,
                    None => std::future::pending().await,
                }
            }, if has_media => {
                match media_msg {
                    Ok(Message::Binary(bytes)) => {
                        if let Some(state) = active_media.as_mut() {
                            match WireMediaFrame::decode(&bytes) {
                                Ok(wire) => match wire.kind {
                                    MediaPayloadKind::PcmS16le => {
                                        let elapsed = state.media.attached_elapsed();
                                        let track_levels = &mut state.track_levels;
                                        let _ = state.media.receive.wire_frame(
                                            &wire,
                                            elapsed,
                                            |track_id, _, samples| {
                                                track_levels
                                                    .entry(track_id)
                                                    .or_default()
                                                    .observe_i16(samples);
                                            },
                                        );
                                    }
                                    MediaPayloadKind::Jpeg => {
                                        if state.media.receive_video.binary_frame(&wire) {
                                            for frame in state.media.receive_video.drain_raw_frames() {
                                                let _ = channel.send(DesktopCallPacket::RemoteFrame {
                                                    track_id: frame.track_id,
                                                    participant_id: frame.participant_id,
                                                    rotation_degrees: frame.rotation_degrees,
                                                    jpeg: STANDARD.encode(&frame.jpeg),
                                                });
                                            }
                                        }
                                    }
                                    MediaPayloadKind::H265AnnexB | MediaPayloadKind::H264AnnexB => {}
                                },
                                Err(error) => {
                                    debug_call("desktop-media", format_args!("dropped malformed media frame: {error:#}"));
                                }
                            }
                        }
                    }
                    Ok(Message::Text(text)) => {
                        if let Ok(event) = serde_json::from_str::<CallEvent>(&text) {
                            match event {
                                CallEvent::VideoKeyframeRequested { .. } => {
                                    if let Some(capture) = active_media.as_mut().and_then(|s| s.local_video.as_mut()) {
                                        let _ = capture.request_keyframe();
                                    }
                                }
                                CallEvent::Error { code } => {
                                    let code = serde_json::to_value(code)
                                        .ok()
                                        .and_then(|v| v.as_str().map(str::to_owned))
                                        .unwrap_or_else(|| "media_error".into());
                                    let _ = channel.send(DesktopCallPacket::MediaError {
                                        code,
                                        message: None,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        debug_call("desktop-media", format_args!("media socket closed: {error:#}"));
                        if let Some(old) = active_media.take() {
                            old.close().await;
                        }
                        let _ = channel.send(DesktopCallPacket::MediaError {
                            code: "media_closed".into(),
                            message: Some(error.to_string()),
                        });
                    }
                }
            }
            _ = audio_tick.tick(), if has_local_audio => {
                if let Some(state) = active_media.as_mut() {
                    let frame_samples = state.audio_format.samples_per_frame();
                    let send_result = if let Some(audio) = state.local_audio.as_mut() {
                        match audio.frame(frame_samples, state.muted) {
                            Ok(samples) => {
                                if !state.muted {
                                    state.mic_level.observe_i16(&samples);
                                }
                                state.media.send_pcm(&samples, state.audio_format).await
                            }
                            Err(error) => {
                                let _ = channel.send(DesktopCallPacket::MediaError {
                                    code: "microphone_failed".into(),
                                    message: Some(error.to_string()),
                                });
                                if let Some(mut failed) = state.local_audio.take() {
                                    let _ = failed.stop().await;
                                }
                                Ok(())
                            }
                        }
                    } else {
                        Ok(())
                    };
                    if let Err(error) = send_result {
                        debug_call("desktop-media", format_args!("pcm send failed: {error:#}"));
                    }

                    // Render due receive audio frames into native playback.
                    let elapsed = state.media.attached_elapsed();
                    if let Ok(frames) = state.media.receive.render_due(elapsed) {
                        for frame in frames {
                            if state.speaker_muted {
                                continue;
                            }
                            state.speaker_level.observe_i16(&frame);
                            if let Some(audio) = &state.local_audio {
                                let floats: Vec<f32> = frame
                                    .iter()
                                    .map(|&sample| f32::from(sample) / 32768.0)
                                    .collect();
                                let _ = audio.playback.enqueue(
                                    audio.playback_token,
                                    &floats,
                                    state.audio_format.sample_rate(),
                                );
                            }
                        }
                    }

                    // Pump outgoing video frames (capped per audio tick so audio never starves).
                    for _ in 0..4 {
                        let next_frame = state
                            .local_video
                            .as_mut()
                            .and_then(LocalVideoCapture::try_recv);
                        let Some(frame) = next_frame else {
                            break;
                        };
                        if let Err(error) = state.media.send_video(&frame).await {
                            debug_call("desktop-media", format_args!("video send failed: {error:#}"));
                            break;
                        }
                        if frame.codec == CallVideoCodec::Jpeg {
                            if state.last_local_jpeg.elapsed() >= SELF_VIEW_MIN_INTERVAL {
                                state.last_local_jpeg = Instant::now();
                                let _ = channel.send(DesktopCallPacket::LocalJpeg {
                                    jpeg: STANDARD.encode(&frame.bytes),
                                });
                            }
                        } else if let Some(decoder) = state.self_view.as_mut() {
                            decoder.feed(&frame);
                        }
                    }
                    if let Some(decoder) = state.self_view.as_mut() {
                        if let Some(jpeg) = decoder.drain_latest() {
                            if state.last_local_jpeg.elapsed() >= SELF_VIEW_MIN_INTERVAL {
                                state.last_local_jpeg = Instant::now();
                                let _ = channel.send(DesktopCallPacket::LocalJpeg {
                                    jpeg: STANDARD.encode(&jpeg),
                                });
                            }
                        }
                    }
                }
            }
            _ = level_tick.tick(), if has_media => {
                if let Some(state) = active_media.as_mut() {
                    state.mic_level.advance();
                    state.speaker_level.advance();
                    let mut track_levels = BTreeMap::new();
                    for (&track_id, level) in &mut state.track_levels {
                        level.advance();
                        track_levels.insert(track_id, level.normalized());
                    }
                    let _ = channel.send(DesktopCallPacket::Levels {
                        mic_level: state.mic_level.normalized(),
                        speaker_level: state.speaker_level.normalized(),
                        track_levels,
                    });
                }
            }
        }
    };

    if let Some(old) = active_media.take() {
        old.close().await;
    }
    for (_, waiter) in pending.drain() {
        let _ = waiter.send(Err("closed".into()));
    }
    control.close().await;
    if let Some(code) = disconnect_code {
        let _ = channel.send(DesktopCallPacket::Disconnected { code });
    }
}

#[tauri::command]
pub async fn call_connect(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    channel: Channel<DesktopCallPacket>,
) -> Result<Value, String> {
    let endpoint = resolve_active_endpoint(&state, &connection_id, &account_id).await?;
    let ffmpeg = resolve_ffmpeg(&state.binaries);

    let (mut control, hello_event) = ControlClient::connect(&endpoint)
        .await
        .map_err(|error| map_connect_error(&error))?;

    let daemon_supports_video = control.capabilities.direct_video
        || control.capabilities.conference_video
        || control.video_capabilities.is_some();
    let ffmpeg_for_probe = ffmpeg.clone();
    let client_video_caps = tokio::task::spawn_blocking(move || {
        video::client_video_capabilities(daemon_supports_video, ffmpeg_for_probe.as_deref())
    })
    .await
    .ok()
    .flatten();

    let ready_cmd = CallCommand::Ready {
        request_id: uuid::Uuid::new_v4().to_string(),
        ready: true,
        format: control.format,
        scope: Some(CallReadyScope::All),
        video: client_video_caps,
    };
    if let Err(error) = control.command(ready_cmd).await {
        debug_call(
            "desktop-control",
            format_args!("call.ready rejected or failed (staying connected): {error:#}"),
        );
    }

    let hello_json = sanitized_event_value(&hello_event).map_err(|error| error.to_string())?;

    let session_id = state.calls.next_session.fetch_add(1, Ordering::AcqRel) + 1;
    let cancel = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let actor_cancel = cancel.clone();
    let task = tauri::async_runtime::spawn(run_account_actor(
        endpoint,
        ffmpeg,
        control,
        rx,
        channel,
        actor_cancel,
    ));

    let previous = {
        let mut accounts = state.calls.accounts.lock().await;
        accounts.insert(
            account_id,
            AccountHandle {
                session_id,
                connection_id,
                cancel,
                tx,
                task,
            },
        )
    };
    if let Some(old) = previous {
        let _ = old.session_id;
        old.cancel.cancel();
        let _ = timeout(Duration::from_secs(4), old.task).await;
    }

    Ok(hello_json)
}

#[tauri::command]
pub async fn call_disconnect(state: State<'_, Desktop>, account_id: String) -> Result<(), String> {
    validate_account_id(&account_id)?;
    state.calls.disconnect_account(&account_id).await;
    Ok(())
}

#[tauri::command]
pub async fn call_devices(state: State<'_, Desktop>) -> Result<CallDevicesSnapshotDto, String> {
    let ffmpeg = resolve_ffmpeg(&state.binaries);
    tokio::task::spawn_blocking(move || {
        let mut audio_inputs = Vec::new();
        let mut audio_outputs = Vec::new();
        if let Ok(list) = wechat_audio::AudioSession::devices() {
            for dev in list {
                let dto = CallDeviceDto {
                    id: dev.id.unwrap_or_default(),
                    label: dev.name,
                    kind: if dev.input {
                        "audioinput"
                    } else {
                        "audiooutput"
                    },
                    is_default: dev.is_default,
                };
                if dev.input {
                    audio_inputs.push(dto);
                } else {
                    audio_outputs.push(dto);
                }
            }
        }
        let video_inputs = video::list_cameras_with_ffmpeg(ffmpeg.as_deref())
            .into_iter()
            .map(|cam| CallDeviceDto {
                id: cam.id,
                label: cam.name,
                kind: "videoinput",
                is_default: cam.is_default,
            })
            .collect();
        CallDevicesSnapshotDto {
            audio_inputs,
            audio_outputs,
            video_inputs,
            permission_state: "granted",
            supports_output_selection: true,
        }
    })
    .await
    .map_err(|_| "Device listing failed".into())
}

#[tauri::command]
pub async fn call_preview_start(
    state: State<'_, Desktop>,
    camera_id: Option<String>,
    channel: Channel<DesktopPreviewPacket>,
) -> Result<(), String> {
    let source = camera_source_from_ipc(camera_id.as_deref())?;
    let ffmpeg = resolve_ffmpeg(&state.binaries);
    state.calls.stop_preview().await;

    let capture = tokio::task::spawn_blocking(move || {
        LocalVideoCapture::start_preview(&source, 640, 480, 15, ffmpeg.as_deref())
    })
    .await
    .map_err(|_| "Camera preview task failed".to_string())?
    .map_err(|error| error.to_string())?;

    let cancel = CancellationToken::new();
    let loop_cancel = cancel.clone();
    let task = tauri::async_runtime::spawn(async move {
        let mut capture = capture;
        let mut ticker = interval(PREVIEW_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = loop_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Some(frame) = capture.try_recv() {
                        if channel
                            .send(DesktopPreviewPacket::Frame {
                                jpeg: STANDARD.encode(&frame.bytes),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
        drop(capture);
        let _ = channel.send(DesktopPreviewPacket::Ended);
    });

    *state.calls.preview.lock().await = Some(PreviewSession { cancel, task });
    Ok(())
}

#[tauri::command]
pub async fn call_preview_stop(state: State<'_, Desktop>) -> Result<(), String> {
    state.calls.stop_preview().await;
    Ok(())
}

#[tauri::command]
pub async fn call_dial(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    contact_id: String,
    options: DialOptions,
) -> Result<CallStartResponse, String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&contact_id, "contactId", MAX_ID_LEN)?;
    state.calls.stop_preview().await;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    let result = send_actor_command(
        &tx,
        CallCommand::Dial {
            request_id,
            target: CallTarget::Direct { contact_id },
            call_type: options.call_type,
            camera_enabled: options.camera_enabled,
            muted: options.muted,
        },
    )
    .await?;
    let call_id = result.call_id.ok_or_else(|| "invalid_event".to_string())?;
    Ok(CallStartResponse { call_id })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn call_conference_start(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    group_id: String,
    invitees: Vec<String>,
    options: DialOptions,
) -> Result<CallStartResponse, String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&group_id, "groupId", MAX_ID_LEN)?;
    if invitees.is_empty() || invitees.len() > MAX_INVITEES {
        return Err("Select between 1 and 100 invitees".into());
    }
    for invitee in &invitees {
        validate_non_empty_id(invitee, "invitee", MAX_ID_LEN)?;
    }
    state.calls.stop_preview().await;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    let result = send_actor_command(
        &tx,
        CallCommand::Dial {
            request_id,
            target: CallTarget::Conference { group_id, invitees },
            call_type: options.call_type,
            camera_enabled: options.camera_enabled,
            muted: options.muted,
        },
    )
    .await?;
    let call_id = result.call_id.ok_or_else(|| "invalid_event".to_string())?;
    Ok(CallStartResponse { call_id })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn call_conference_join(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    invitation_id: String,
    invitation_revision: u32,
    options: JoinOptions,
) -> Result<CallStartResponse, String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&invitation_id, "invitationId", MAX_ID_LEN)?;
    state.calls.stop_preview().await;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    let result = send_actor_command(
        &tx,
        CallCommand::Join {
            request_id,
            invitation_id,
            invitation_revision,
            camera_enabled: options.camera_enabled,
            muted: options.muted,
        },
    )
    .await?;
    let call_id = result.call_id.ok_or_else(|| "invalid_event".to_string())?;
    Ok(CallStartResponse { call_id })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn call_answer(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    call_id: String,
    expected_revision: u32,
    options: AnswerOptions,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    state.calls.stop_preview().await;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    send_actor_command(
        &tx,
        CallCommand::Answer {
            request_id,
            call_id,
            expected_revision,
            camera_enabled: options.camera_enabled,
            muted: options.muted,
        },
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn call_reject(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    call_id: String,
    expected_revision: u32,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    send_actor_command(
        &tx,
        CallCommand::Reject {
            request_id,
            call_id,
            expected_revision,
        },
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn call_invite(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    call_id: String,
    contact_ids: Vec<String>,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    if contact_ids.is_empty() || contact_ids.len() > MAX_INVITEES {
        return Err("Select between 1 and 100 contacts".into());
    }
    for contact_id in &contact_ids {
        validate_non_empty_id(contact_id, "contactId", MAX_ID_LEN)?;
    }
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    send_actor_command(
        &tx,
        CallCommand::Invite {
            request_id,
            call_id,
            contact_ids,
        },
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn call_set_mute(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    call_id: String,
    muted: bool,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    let _ = tx.send(ActorRequest::SetLocalMute { muted });
    if let Err(error) = send_actor_command(
        &tx,
        CallCommand::Mute {
            request_id,
            call_id,
            muted,
        },
    )
    .await
    {
        let _ = tx.send(ActorRequest::SetLocalMute { muted: !muted });
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub async fn call_set_camera(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    request_id: String,
    call_id: String,
    enabled: bool,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    send_actor_command(
        &tx,
        CallCommand::Camera {
            request_id,
            call_id,
            enabled,
        },
    )
    .await?;
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(ActorRequest::SetLocalCamera {
        enabled,
        reply: reply_tx,
    })
    .map_err(|_| "not_connected".to_string())?;
    reply_rx.await.map_err(|_| "closed".to_string())?
}

#[tauri::command]
pub async fn call_set_speaker_muted(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    muted: bool,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    if let Ok(tx) = state.calls.sender(&connection_id, &account_id).await {
        let _ = tx.send(ActorRequest::SetSpeakerMuted { muted });
    }
    Ok(())
}

#[tauri::command]
pub async fn call_switch_devices(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    selection: DeviceSelection,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    if let Some(camera_id) = selection.camera_device_id.as_deref() {
        let _ = camera_source_from_ipc(Some(camera_id))?;
    }
    let Ok(tx) = state.calls.sender(&connection_id, &account_id).await else {
        return Ok(());
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(ActorRequest::SwitchDevices {
        selection,
        reply: reply_tx,
    })
    .map_err(|_| "not_connected".to_string())?;
    reply_rx.await.map_err(|_| "closed".to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn call_end(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    kind: String,
    request_id: String,
    call_id: String,
    expected_revision: u32,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&request_id, "requestId", MAX_REQUEST_ID_LEN)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    let command = match kind.as_str() {
        "call.cancel" => CallCommand::Cancel {
            request_id,
            call_id,
            expected_revision,
        },
        "call.hangup" => CallCommand::Hangup {
            request_id,
            call_id,
            expected_revision,
        },
        _ => return Err("Invalid call end action".into()),
    };
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    send_actor_command(&tx, command).await?;
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn call_attach_media(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    call_id: String,
    call_type: CallType,
    initial_muted: bool,
    initial_camera_enabled: bool,
    devices: DeviceSelection,
    speaker_muted: bool,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    validate_non_empty_id(&call_id, "callId", MAX_ID_LEN)?;
    if let Some(camera_id) = devices.camera_device_id.as_deref() {
        let _ = camera_source_from_ipc(Some(camera_id))?;
    }
    state.calls.stop_preview().await;
    let tx = state.calls.sender(&connection_id, &account_id).await?;
    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(ActorRequest::AttachMedia {
        call_id,
        call_type,
        initial_muted,
        initial_camera_enabled,
        devices,
        speaker_muted,
        reply: reply_tx,
    })
    .map_err(|_| "not_connected".to_string())?;
    timeout(Duration::from_secs(20), reply_rx)
        .await
        .map_err(|_| "media_attach_timeout".to_string())?
        .map_err(|_| "closed".to_string())?
}

#[tauri::command]
pub async fn call_stop_local_media(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    if let Ok(tx) = state.calls.sender(&connection_id, &account_id).await {
        let _ = tx.send(ActorRequest::StopLocalMedia);
    }
    Ok(())
}

#[tauri::command]
pub async fn call_stop_media(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    if let Ok(tx) = state.calls.sender(&connection_id, &account_id).await {
        let _ = tx.send(ActorRequest::StopMedia);
    }
    Ok(())
}

#[tauri::command]
pub fn call_set_fullscreen(window: tauri::WebviewWindow, enabled: bool) -> Result<bool, String> {
    window
        .set_fullscreen(enabled)
        .map_err(|error| error.to_string())?;
    Ok(window.is_fullscreen().unwrap_or(enabled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use wechat_call_contracts::call_types::{
        CallAudioFormat, CallCapabilities, CallDirection, CallKind, CallState, CallView,
        CallsAvailability, CALL_PROTOCOL_VERSION,
    };

    #[test]
    fn ipc_camera_validation_rejects_file_test_flags_and_control_chars() {
        assert_eq!(camera_source_from_ipc(None).unwrap(), CameraSource::Auto);
        assert_eq!(
            camera_source_from_ipc(Some("  ")).unwrap(),
            CameraSource::Auto
        );
        assert_eq!(
            camera_source_from_ipc(Some("/dev/video0")).unwrap(),
            CameraSource::Device("/dev/video0".into())
        );
        for bad in [
            "file:/etc/passwd",
            "FILE:/tmp/a.mp4",
            "test",
            "off",
            "none",
            "-i",
            "cam\n0",
            &"a".repeat(MAX_CAMERA_ID_LEN + 1),
        ] {
            assert!(
                camera_source_from_ipc(Some(bad)).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn account_id_validation_rejects_traversal_and_control_chars() {
        assert!(validate_account_id("acct_1-dev.local").is_ok());
        for bad in ["", "../etc", "a/b", "a b", "a\0b", &"x".repeat(129)] {
            assert!(
                validate_account_id(bad).is_err(),
                "expected {bad:?} to fail"
            );
        }
    }

    #[tokio::test]
    async fn actor_strips_owner_token_and_forwards_interleaved_state_events() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Receive call.hello
            let _ = ws.next().await.unwrap().unwrap();
            let hello = CallEvent::Hello {
                protocol_version: CALL_PROTOCOL_VERSION,
                client_id: "desktop-test".into(),
                capabilities: CallCapabilities {
                    direct_audio: true,
                    direct_video: true,
                    ..CallCapabilities::default()
                },
                availability: CallsAvailability::default(),
                current_offer: None,
                current_pending_offer: None,
                current_call: None,
                audio_format: CallAudioFormat::default(),
                video_capabilities: None,
                conference_invitations: vec![],
            };
            ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
                .await
                .unwrap();

            // Receive call.dial
            let Message::Text(cmd_text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected text command");
            };
            let cmd: Value = serde_json::from_str(&cmd_text).unwrap();
            assert_eq!(cmd["type"], "call.dial");
            let req_id = cmd["requestId"].as_str().unwrap().to_owned();

            // Send interleaved call.state BEFORE command.result!
            let state_evt = CallEvent::State {
                call: CallView {
                    call_id: "call-42".into(),
                    revision: 1,
                    kind: CallKind::Direct,
                    call_type: CallType::Voice,
                    direction: CallDirection::Outgoing,
                    state: CallState::Dialing,
                    owned_by_this_client: true,
                    group_id: None,
                    participants: vec![],
                    controls: vec![],
                },
            };
            ws.send(Message::Text(
                serde_json::to_string(&state_evt).unwrap().into(),
            ))
            .await
            .unwrap();

            // Send command.result carrying a secret ownerToken.
            let result_evt = CallEvent::CommandResult {
                request_id: req_id,
                status: CallCommandStatus::Accepted,
                call_id: Some("call-42".into()),
                owner_token: Some("secret-owner-token-never-expose".into()),
                code: None,
            };
            ws.send(Message::Text(
                serde_json::to_string(&result_evt).unwrap().into(),
            ))
            .await
            .unwrap();

            let _ = ws.next().await;
        });

        let endpoint = CallEndpoint {
            http: reqwest::Client::new(),
            base: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            token: "synthetic-token".into(),
            account: "acc".into(),
        };
        let (control, _hello) = ControlClient::connect(&endpoint).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        let captured = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let captured_clone = captured.clone();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(text) = body {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    captured_clone.lock().unwrap().push(value);
                }
            }
            Ok(())
        });

        let actor_task = tokio::spawn(run_account_actor(
            endpoint,
            None,
            control,
            rx,
            channel,
            cancel.clone(),
        ));

        let owner = send_actor_command(
            &tx,
            CallCommand::Dial {
                request_id: "req-1".into(),
                target: CallTarget::Direct {
                    contact_id: "alice".into(),
                },
                call_type: CallType::Voice,
                camera_enabled: None,
                muted: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(owner.call_id.as_deref(), Some("call-42"));
        assert_eq!(
            owner.owner_token.as_deref(),
            Some("secret-owner-token-never-expose")
        );

        // Give channel delivery a moment, then stop cleanly.
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        actor_task.await.unwrap();
        server.await.unwrap();

        let packets = captured.lock().unwrap();
        assert!(
            packets
                .iter()
                .any(|p| p["type"] == "event" && p["event"]["type"] == "call.state"),
            "interleaved call.state event must not be swallowed: {packets:?}"
        );
        for packet in packets.iter() {
            let serialized = serde_json::to_string(packet).unwrap();
            assert!(
                !serialized.contains("secret-owner-token-never-expose"),
                "ownerToken leaked to IPC channel: {serialized}"
            );
        }
    }
}
