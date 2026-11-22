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
    sync::{mpsc, oneshot, watch, Mutex},
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
        CallCommand, CallCommandStatus, CallEvent, CallReadyScope, CallState, CallTarget,
        CallTrack, CallType, CallVideoCodec, CallVideoFormat,
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
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const SELF_VIEW_MIN_INTERVAL: Duration = Duration::from_millis(66);
const VIDEO_ACK_TIMEOUT: Duration = Duration::from_millis(200);
const LEVEL_INTERVAL: Duration = Duration::from_millis(80);
const HEVC_AUD_NALU: &[u8] = &[0, 0, 0, 1, 0x46, 0x01, 0x50];
const H264_AUD_NALU: &[u8] = &[0, 0, 0, 1, 0x09, 0xf0];

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
    OsCallAlert {
        #[serde(rename = "callId")]
        call_id: Option<String>,
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
pub struct CallConnectResponse {
    pub session_id: u64,
    pub hello: Value,
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
    preview_gen: AtomicU64,
    root_cancel: CancellationToken,
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
    generation: u64,
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
    VideoFrameAck,
}

impl CallManager {
    pub async fn stop_preview(&self) {
        self.stop_preview_if(None).await;
    }

    pub async fn stop_preview_if(&self, expected_generation: Option<u64>) {
        let taken = {
            let mut guard = self.preview.lock().await;
            match expected_generation {
                Some(gen) => {
                    let _ = self.preview_gen.compare_exchange(
                        gen,
                        gen.wrapping_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    if guard
                        .as_ref()
                        .is_some_and(|preview| preview.generation == gen)
                    {
                        guard.take()
                    } else {
                        None
                    }
                }
                None => {
                    self.preview_gen.fetch_add(1, Ordering::AcqRel);
                    guard.take()
                }
            }
        };
        if let Some(preview) = taken {
            preview.cancel.cancel();
            let _ = timeout(TEARDOWN_TIMEOUT, preview.task).await;
        }
    }

    async fn stop_preview_older_than(&self, min_generation: u64) {
        let taken = {
            let mut guard = self.preview.lock().await;
            if guard
                .as_ref()
                .is_some_and(|preview| preview.generation < min_generation)
            {
                guard.take()
            } else {
                None
            }
        };
        if let Some(preview) = taken {
            preview.cancel.cancel();
            let _ = timeout(TEARDOWN_TIMEOUT, preview.task).await;
        }
    }

    pub async fn disconnect_account(
        &self,
        connection_id: &str,
        account_id: &str,
        session_id: Option<u64>,
    ) {
        let removed = {
            let mut accounts = self.accounts.lock().await;
            let matches = accounts.get(account_id).is_some_and(|handle| {
                handle.connection_id == connection_id
                    && session_id.is_none_or(|expected| handle.session_id == expected)
            });
            if matches {
                accounts.remove(account_id)
            } else {
                None
            }
        };
        if let Some(handle) = removed {
            handle.cancel.cancel();
            let _ = timeout(Duration::from_secs(4), handle.task).await;
        }
    }

    /// Tear down all active call sessions and camera previews (called on hub switch,
    /// disconnect, local hub reconfiguration, or app shutdown).
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

    /// Lock-free immediate cancellation for `RunEvent::Exit` (macOS Dock Quit / OS shutdown)
    /// so active FFmpeg capture/self-view subprocesses are signalled to exit immediately.
    pub fn cancel_now(&self) {
        self.preview_gen.fetch_add(1, Ordering::AcqRel);
        self.root_cancel.cancel();
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

/// Strictly validate an IPC camera device identifier syntactically per target OS.
/// Never allows `file:`, `test`, flags, arbitrary paths, or AVFoundation audio/screen syntax.
fn validate_camera_id_syntax(camera_id: Option<&str>) -> Result<CameraSource, String> {
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
    #[cfg(target_os = "linux")]
    {
        let Some(suffix) = raw.strip_prefix("/dev/video") else {
            return Err("Invalid camera device".into());
        };
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Invalid camera device".into());
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Invalid camera device".into());
        }
    }
    #[cfg(target_os = "windows")]
    {
        if raw.contains(':') || raw.contains('"') || raw.contains('\\') || raw.contains('/') {
            return Err("Invalid camera device".into());
        }
    }
    Ok(CameraSource::Device(raw.to_owned()))
}

/// Validate a camera identifier against the OS camera enumeration before spawning FFmpeg
/// (critical on macOS where AVFoundation screen-capture sources also have numeric indices).
fn resolve_verified_camera_source(
    camera_id: Option<&str>,
    ffmpeg: Option<&Path>,
) -> Result<CameraSource, String> {
    let source = validate_camera_id_syntax(camera_id)?;
    if let CameraSource::Device(ref id) = source {
        let cameras = video::list_cameras_with_ffmpeg(ffmpeg);
        if !cameras.iter().any(|camera| camera.id == *id) {
            return Err("camera_unavailable".into());
        }
    }
    Ok(source)
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

/// Drop blocking video subprocesses (`LocalVideoCapture` / `SelfViewDecoder`) off Tokio worker threads.
async fn stop_video_handles(video: Option<LocalVideoCapture>, self_view: Option<SelfViewDecoder>) {
    if video.is_none() && self_view.is_none() {
        return;
    }
    let _ = timeout(
        TEARDOWN_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            drop(self_view);
            drop(video);
        }),
    )
    .await;
}

/// Optional secondary FFmpeg decoder that turns outgoing Annex-B H.264/H.265 frames
/// into low-resolution self-view JPEGs without opening the camera twice.
struct SelfViewDecoder {
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    input_tx: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    latest_output: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    wait_for_keyframe: bool,
}

impl SelfViewDecoder {
    fn start(ffmpeg: &Path, codec: CallVideoCodec) -> Option<Self> {
        let (input_fmt, aud_nalu) = match codec {
            CallVideoCodec::H264 => ("h264", H264_AUD_NALU),
            CallVideoCodec::H265 => ("hevc", HEVC_AUD_NALU),
            CallVideoCodec::Jpeg => return None,
        };
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "+genpts",
            "-flags",
            "low_delay",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-threads",
            "1",
            "-f",
            input_fmt,
            "-i",
            "pipe:0",
            "-vf",
            // Outgoing H.264/H.265 frames from LocalVideoCapture are pre-rotated 90° CCW
            // for mobile/conference WeChat wire compatibility; undo that (+90° CW) for self-view
            // and convert to full-range YUV so FFmpeg 7.1/8.0 strict MJPEG accepts every frame.
            "transpose=1,scale=w='min(iw,640)':h='min(ih,480)':force_original_aspect_ratio=decrease:out_range=full,scale=trunc(iw/2)*2:trunc(ih/2)*2:out_range=full",
            "-strict",
            "unofficial",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            "yuv420p",
            "-color_range",
            "pc",
            "-q:v",
            "6",
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
        let kill_child = |mut c: Child| {
            let _ = c.kill();
            let _ = c.wait();
        };
        let Some(mut stdin) = child.stdin.take() else {
            kill_child(child);
            return None;
        };
        let Some(mut stdout) = child.stdout.take() else {
            kill_child(child);
            return None;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (input_tx, input_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
        let latest_output = Arc::new(std::sync::Mutex::new(None));
        let reader_output = latest_output.clone();

        let writer_stop = stop.clone();
        let Ok(writer) = std::thread::Builder::new()
            .name("desktop-selfview-in".into())
            .spawn(move || {
                while !writer_stop.load(Ordering::Acquire) {
                    let Ok(bytes) = input_rx.recv() else {
                        break;
                    };
                    if stdin.write_all(&bytes).is_err()
                        || stdin.write_all(aud_nalu).is_err()
                        || stdin.flush().is_err()
                    {
                        break;
                    }
                }
            })
        else {
            kill_child(child);
            return None;
        };

        let reader_stop = stop.clone();
        let Ok(reader) = std::thread::Builder::new()
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
                            if let Ok(mut guard) = reader_output.lock() {
                                *guard = Some(frame);
                            }
                        }
                    }
                }
            })
        else {
            stop.store(true, Ordering::Release);
            drop(input_tx);
            kill_child(child);
            let _ = writer.join();
            return None;
        };

        Some(Self {
            child: Some(child),
            stop,
            input_tx: Some(input_tx),
            latest_output,
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
        self.latest_output
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct QuantizedLevels {
    mic: u8,
    speaker: u8,
    tracks: BTreeMap<u32, u8>,
}

impl QuantizedLevels {
    fn from_normalized(mic: f32, speaker: f32, tracks: &BTreeMap<u32, f32>) -> Self {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 50.0).round() as u8;
        Self {
            mic: q(mic),
            speaker: q(speaker),
            tracks: tracks.iter().map(|(&id, &v)| (id, q(v))).collect(),
        }
    }
}

fn can_dispatch_video_batch(in_flight_since: Option<Instant>, now: Instant) -> bool {
    in_flight_since.is_none_or(|since| now.saturating_duration_since(since) >= VIDEO_ACK_TIMEOUT)
}

struct ActiveMediaState {
    call_id: String,
    call_type: CallType,
    media: MediaClient,
    audio_format: AudioFormat,
    local_audio: Option<LocalAudio>,
    local_video: Option<LocalVideoCapture>,
    video_gen: u64,
    active_video_format: Option<CallVideoFormat>,
    self_view: Option<SelfViewDecoder>,
    devices: DeviceSelection,
    muted: bool,
    camera_enabled: bool,
    speaker_muted: bool,
    local_media_stopped: bool,
    mic_level: SignalLevel,
    speaker_level: SignalLevel,
    track_levels: BTreeMap<u32, SignalLevel>,
    last_sent_levels: Option<QuantizedLevels>,
    last_local_jpeg: Option<Instant>,
    pending_local_jpeg: Option<Vec<u8>>,
    video_in_flight_since: Option<Instant>,
}

impl ActiveMediaState {
    async fn stop_video(&mut self) {
        self.video_gen = self.video_gen.wrapping_add(1);
        self.active_video_format = None;
        self.pending_local_jpeg = None;
        let video = self.local_video.take();
        let self_view = self.self_view.take();
        stop_video_handles(video, self_view).await;
    }

    fn flush_video_frames(&mut self, channel: &Channel<DesktopCallPacket>) {
        let now = Instant::now();
        if !can_dispatch_video_batch(self.video_in_flight_since, now) {
            return;
        }
        let remote_frames = self.media.receive_video.drain_raw_frames();
        let local_jpeg = if self.pending_local_jpeg.is_some()
            && self
                .last_local_jpeg
                .is_none_or(|prev| now.saturating_duration_since(prev) >= SELF_VIEW_MIN_INTERVAL)
        {
            self.last_local_jpeg = Some(now);
            self.pending_local_jpeg.take()
        } else {
            None
        };
        if remote_frames.is_empty() && local_jpeg.is_none() {
            return;
        }
        self.video_in_flight_since = Some(now);
        for frame in remote_frames {
            let _ = channel.send(DesktopCallPacket::RemoteFrame {
                track_id: frame.track_id,
                participant_id: frame.participant_id,
                rotation_degrees: frame.rotation_degrees,
                jpeg: STANDARD.encode(&frame.jpeg),
            });
        }
        if let Some(jpeg) = local_jpeg {
            let _ = channel.send(DesktopCallPacket::LocalJpeg {
                jpeg: STANDARD.encode(&jpeg),
            });
        }
    }

    async fn stop_local_devices(&mut self) {
        self.local_media_stopped = true;
        self.stop_video().await;
        if let Some(mut audio) = self.local_audio.take() {
            let _ = audio.stop().await;
        }
        self.mic_level.reset();
        self.speaker_level.reset();
    }

    async fn start_camera(&mut self, ffmpeg: Option<PathBuf>) -> Result<()> {
        self.stop_video().await;
        if self.local_media_stopped {
            return Ok(());
        }
        let Some(video_track) = self.media.video_send_track.as_ref() else {
            return Ok(());
        };
        if matches!(video_track, CallTrack::Video { active: false, .. }) {
            return Ok(());
        }
        let Some(video_format) = video_track.video_format() else {
            return Ok(());
        };
        let camera_id = self.devices.camera_device_id.clone();
        let ffmpeg_for_capture = ffmpeg.clone();
        let (capture, self_view) = tokio::task::spawn_blocking(move || -> Result<_> {
            let source =
                resolve_verified_camera_source(camera_id.as_deref(), ffmpeg_for_capture.as_deref())
                    .map_err(|message| anyhow::anyhow!(message))?;
            let capture = LocalVideoCapture::start_with_ffmpeg(
                &source,
                video_format,
                ffmpeg_for_capture.as_deref(),
            )?;
            let self_view = if video_format.codec != CallVideoCodec::Jpeg {
                ffmpeg_for_capture
                    .as_deref()
                    .and_then(|path| SelfViewDecoder::start(path, video_format.codec))
            } else {
                None
            };
            Ok((capture, self_view))
        })
        .await
        .context("camera capture task panicked")??;
        self.local_video = Some(capture);
        self.active_video_format = Some(video_format);
        self.self_view = self_view;
        Ok(())
    }

    fn video_track_is_active(&self) -> bool {
        matches!(
            self.media.video_send_track.as_ref(),
            Some(CallTrack::Video { active: true, .. })
        )
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

struct PendingKeyframeTask {
    call_id: String,
    video_gen: u64,
    handle: tokio::task::JoinHandle<(LocalVideoCapture, Result<()>)>,
}

fn keyframe_matches(task: &Option<PendingKeyframeTask>, state: &ActiveMediaState) -> bool {
    task.as_ref().is_some_and(|pending| {
        pending.call_id == state.call_id && pending.video_gen == state.video_gen
    })
}

async fn drain_keyframe_task(keyframe_task: &mut Option<PendingKeyframeTask>) {
    if let Some(task) = keyframe_task.take() {
        if let Ok(Ok((capture, _))) = timeout(TEARDOWN_TIMEOUT, task.handle).await {
            stop_video_handles(Some(capture), None).await;
        }
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
    mut sounding_rx: watch::Receiver<HashMap<String, String>>,
    cancel: CancellationToken,
) {
    let mut pending: HashMap<String, oneshot::Sender<Result<OwnerResult, String>>> = HashMap::new();
    let mut owner_tokens: HashMap<String, String> = HashMap::new();
    let mut active_media: Option<ActiveMediaState> = None;
    let mut keyframe_task: Option<PendingKeyframeTask> = None;

    let mut last_sounding_call_id = sounding_rx
        .borrow_and_update()
        .get(&endpoint.account)
        .cloned();
    if last_sounding_call_id.is_some() {
        let _ = channel.send(DesktopCallPacket::OsCallAlert {
            call_id: last_sounding_call_id.clone(),
        });
    }

    let mut audio_tick = interval(Duration::from_millis(20));
    audio_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut level_tick = interval(LEVEL_INTERVAL);
    level_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let disconnect_code = loop {
        let has_media = active_media.is_some();
        let has_keyframe_task = keyframe_task.is_some();
        let should_tick_media = active_media
            .as_ref()
            .is_some_and(|state| !state.local_media_stopped);

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
                        drain_keyframe_task(&mut keyframe_task).await;
                        if let Some(old) = active_media.take() {
                            old.close().await;
                        }
                        let Some(owner_token) = owner_tokens.get(&call_id).cloned() else {
                            let _ = channel.send(DesktopCallPacket::MediaError {
                                code: "media_attach_failed".into(),
                                message: Some("missing_owner_token".into()),
                            });
                            let _ = reply.send(Err("missing_owner_token".into()));
                            continue;
                        };
                        let client_id = control.client_id.clone();
                        let control_format = control.format;
                        let attached = tokio::select! {
                            _ = cancel.cancelled() => {
                                let _ = reply.send(Err("closed".into()));
                                break None;
                            }
                            res = MediaClient::attach_passthrough_parts(
                                &endpoint,
                                &client_id,
                                control_format,
                                &call_id,
                                &owner_token,
                            ) => res,
                        };
                        match attached {
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
                                            code: "unsupported".into(),
                                            message: Some(error.to_string()),
                                        });
                                        let _ = reply.send(Err("unsupported".into()));
                                        continue;
                                    }
                                };
                                let audio_opts = AudioDeviceOptions {
                                    input_device: sanitize_device_id(devices.input_device_id.as_deref()),
                                    output_device: sanitize_device_id(devices.output_device_id.as_deref()),
                                };
                                let opened_audio = tokio::select! {
                                    _ = cancel.cancelled() => {
                                        let _ = timeout(Duration::from_secs(2), media.socket.close(None)).await;
                                        let _ = reply.send(Err("closed".into()));
                                        break None;
                                    }
                                    res = LocalAudio::open(&audio_opts) => res,
                                };
                                let local_audio = match opened_audio {
                                    Ok(audio) => Some(audio),
                                    Err(error) => {
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "microphone_unavailable".into(),
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
                                    video_gen: 0,
                                    active_video_format: None,
                                    self_view: None,
                                    devices,
                                    muted: initial_muted,
                                    camera_enabled: initial_camera_enabled && call_type == CallType::Video,
                                    speaker_muted,
                                    local_media_stopped: false,
                                    mic_level: SignalLevel::default(),
                                    speaker_level: SignalLevel::default(),
                                    track_levels: BTreeMap::new(),
                                    last_sent_levels: None,
                                    last_local_jpeg: None,
                                    pending_local_jpeg: None,
                                    video_in_flight_since: None,
                                };
                                if state.camera_enabled && state.video_track_is_active() {
                                    let cam_res = tokio::select! {
                                        _ = cancel.cancelled() => {
                                            state.close().await;
                                            let _ = reply.send(Err("closed".into()));
                                            break None;
                                        }
                                        res = state.start_camera(ffmpeg.clone()) => res,
                                    };
                                    if let Err(error) = cam_res {
                                        let _ = channel.send(DesktopCallPacket::MediaError {
                                            code: "camera_unavailable".into(),
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
                                let _ = reply.send(Err("media_attach_failed".into()));
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
                            drain_keyframe_task(&mut keyframe_task).await;
                            state.stop_video().await;
                            let _ = reply.send(Ok(()));
                        } else if state.video_track_is_active() {
                            let has_capture =
                                state.local_video.is_some() || keyframe_matches(&keyframe_task, state);
                            if has_capture
                                && state.active_video_format
                                    == state
                                        .media
                                        .video_send_track
                                        .as_ref()
                                        .and_then(|t| t.video_format())
                            {
                                let _ = reply.send(Ok(()));
                                continue;
                            }
                            drain_keyframe_task(&mut keyframe_task).await;
                            match state.start_camera(ffmpeg.clone()).await {
                                Ok(()) => {
                                    let _ = reply.send(Ok(()));
                                }
                                Err(error) => {
                                    let _ = channel.send(DesktopCallPacket::MediaError {
                                        code: "camera_unavailable".into(),
                                        message: Some(error.to_string()),
                                    });
                                    let _ = reply.send(Err("camera_unavailable".into()));
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

                        if audio_changed && !state.local_media_stopped {
                            if let Some(mut old) = state.local_audio.take() {
                                let _ = old.stop().await;
                            }
                            let audio_opts = AudioDeviceOptions {
                                input_device: sanitize_device_id(state.devices.input_device_id.as_deref()),
                                output_device: sanitize_device_id(state.devices.output_device_id.as_deref()),
                            };
                            let reopened = tokio::select! {
                                _ = cancel.cancelled() => {
                                    let _ = reply.send(Err("closed".into()));
                                    break None;
                                }
                                res = LocalAudio::open(&audio_opts) => res,
                            };
                            match reopened {
                                Ok(audio) => state.local_audio = Some(audio),
                                Err(error) => {
                                    let _ = channel.send(DesktopCallPacket::MediaError {
                                        code: "microphone_unavailable".into(),
                                        message: Some(error.to_string()),
                                    });
                                    first_err = Some("microphone_unavailable".into());
                                }
                            }
                        }
                        if camera_changed
                            && !state.local_media_stopped
                            && state.camera_enabled
                            && state.video_track_is_active()
                        {
                            drain_keyframe_task(&mut keyframe_task).await;
                            if let Err(error) = state.start_camera(ffmpeg.clone()).await {
                                let _ = channel.send(DesktopCallPacket::MediaError {
                                    code: "camera_unavailable".into(),
                                    message: Some(error.to_string()),
                                });
                                if first_err.is_none() {
                                    first_err = Some("camera_unavailable".into());
                                }
                            }
                        }
                        let _ = reply.send(match first_err {
                            Some(err) => Err(err),
                            None => Ok(()),
                        });
                    }
                    ActorRequest::StopLocalMedia => {
                        drain_keyframe_task(&mut keyframe_task).await;
                        if let Some(state) = active_media.as_mut() {
                            state.stop_local_devices().await;
                        }
                    }
                    ActorRequest::StopMedia => {
                        drain_keyframe_task(&mut keyframe_task).await;
                        if let Some(old) = active_media.take() {
                            old.close().await;
                        }
                    }
                    ActorRequest::VideoFrameAck => {
                        if let Some(state) = active_media.as_mut() {
                            state.video_in_flight_since = None;
                            state.flush_video_frames(&channel);
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
                        // Enforce hardware and media teardown directly in Rust so a throttled or
                        // reloading WebView never leaves mic/camera open on a dead or unowned call.
                        match &event {
                            CallEvent::State { call } => {
                                if matches!(call.state, CallState::Ended | CallState::Uncertain)
                                    || !call.owned_by_this_client
                                {
                                    owner_tokens.remove(&call.call_id);
                                    if active_media
                                        .as_ref()
                                        .is_some_and(|m| m.call_id == call.call_id)
                                    {
                                        drain_keyframe_task(&mut keyframe_task).await;
                                        if let Some(old) = active_media.take() {
                                            old.close().await;
                                        }
                                    }
                                } else if call.state == CallState::Ending {
                                    if let Some(state) = active_media
                                        .as_mut()
                                        .filter(|m| m.call_id == call.call_id)
                                    {
                                        drain_keyframe_task(&mut keyframe_task).await;
                                        state.stop_local_devices().await;
                                    }
                                }
                            }
                            CallEvent::Availability { availability } => {
                                if !availability.busy || !availability.owned_by_this_client {
                                    owner_tokens.clear();
                                    drain_keyframe_task(&mut keyframe_task).await;
                                    if let Some(old) = active_media.take() {
                                        old.close().await;
                                    }
                                }
                            }
                            _ => {}
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
                                if video_send_changed {
                                    if !state.video_track_is_active() || !state.camera_enabled {
                                        drain_keyframe_task(&mut keyframe_task).await;
                                        state.stop_video().await;
                                    } else {
                                        let next_fmt = state
                                            .media
                                            .video_send_track
                                            .as_ref()
                                            .and_then(|t| t.video_format());
                                        let has_capture = state.local_video.is_some()
                                            || keyframe_matches(&keyframe_task, state);
                                        if !has_capture || state.active_video_format != next_fmt {
                                            // Ensure any in-flight keyframe task has released the
                                            // exclusive camera device before spawning a new FFmpeg.
                                            drain_keyframe_task(&mut keyframe_task).await;
                                            if let Err(error) = state.start_camera(ffmpeg.clone()).await {
                                                let _ = channel.send(DesktopCallPacket::MediaError {
                                                    code: "camera_unavailable".into(),
                                                    message: Some(error.to_string()),
                                                });
                                            }
                                        }
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
                                            state.flush_video_frames(&channel);
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
                                    if keyframe_task.is_none() {
                                        if let Some(state) = active_media.as_mut().filter(|s| {
                                            !s.local_media_stopped && s.camera_enabled
                                        }) {
                                            if let Some(mut capture) = state.local_video.take() {
                                                let target_call_id = state.call_id.clone();
                                                let target_video_gen = state.video_gen;
                                                let handle = tokio::task::spawn_blocking(move || {
                                                    let result = capture.request_keyframe();
                                                    (capture, result)
                                                });
                                                keyframe_task = Some(PendingKeyframeTask {
                                                    call_id: target_call_id,
                                                    video_gen: target_video_gen,
                                                    handle,
                                                });
                                            }
                                        }
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
                        drain_keyframe_task(&mut keyframe_task).await;
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
            _ = audio_tick.tick(), if should_tick_media => {
                let mut media_write_failed: Option<String> = None;
                if let Some(state) = active_media.as_mut() {
                    let frame_samples = state.audio_format.samples_per_frame();
                    if let Some(audio) = state.local_audio.as_mut() {
                        match audio.frame(frame_samples, state.muted) {
                            Ok(samples) => {
                                if !state.muted {
                                    state.mic_level.observe_i16(&samples);
                                }
                                if let Err(error) = state.media.send_pcm(&samples, state.audio_format).await {
                                    debug_call("desktop-media", format_args!("pcm send failed: {error:#}"));
                                    media_write_failed = Some(error.to_string());
                                }
                            }
                            Err(error) => {
                                let _ = channel.send(DesktopCallPacket::MediaError {
                                    code: "microphone_unavailable".into(),
                                    message: Some(error.to_string()),
                                });
                                if let Some(mut failed) = state.local_audio.take() {
                                    let _ = failed.stop().await;
                                }
                            }
                        }
                    }

                    if media_write_failed.is_none() {
                        // Advance receive mixer monotonically even when local audio is absent.
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

                        // Pump outgoing video frames (capped per tick so audio never starves).
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
                                media_write_failed = Some(error.to_string());
                                break;
                            }
                            if frame.codec == CallVideoCodec::Jpeg {
                                state.pending_local_jpeg = Some(frame.bytes);
                            } else if let Some(decoder) = state.self_view.as_mut() {
                                decoder.feed(&frame);
                            }
                        }
                        if media_write_failed.is_none() {
                            if let Some(decoder) = state.self_view.as_mut() {
                                if let Some(jpeg) = decoder.drain_latest() {
                                    state.pending_local_jpeg = Some(jpeg);
                                }
                            }
                            state.flush_video_frames(&channel);
                        }
                    }
                }
                if let Some(message) = media_write_failed {
                    drain_keyframe_task(&mut keyframe_task).await;
                    if let Some(old) = active_media.take() {
                        old.close().await;
                    }
                    let _ = channel.send(DesktopCallPacket::MediaError {
                        code: "media_closed".into(),
                        message: Some(message),
                    });
                }
            }
            Ok(()) = sounding_rx.changed() => {
                let next = sounding_rx.borrow_and_update().get(&endpoint.account).cloned();
                if next != last_sounding_call_id {
                    last_sounding_call_id = next.clone();
                    let _ = channel.send(DesktopCallPacket::OsCallAlert { call_id: next });
                }
            }
            keyframe_res = async {
                match keyframe_task.as_mut() {
                    Some(task) => (&mut task.handle).await,
                    None => std::future::pending().await,
                }
            }, if has_keyframe_task => {
                if let Some(task) = keyframe_task.take() {
                    if let Ok((capture, result)) = keyframe_res {
                        let still_valid = active_media.as_ref().is_some_and(|state| {
                            state.call_id == task.call_id
                                && state.video_gen == task.video_gen
                                && !state.local_media_stopped
                                && state.camera_enabled
                                && state.video_track_is_active()
                                && state.local_video.is_none()
                        });
                        if still_valid {
                            if let Err(error) = result {
                                debug_call(
                                    "desktop-media",
                                    format_args!("camera keyframe recovery failed: {error:#}"),
                                );
                            }
                            if let Some(state) = active_media.as_mut() {
                                state.local_video = Some(capture);
                            }
                        } else {
                            stop_video_handles(Some(capture), None).await;
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
                    let mic_level = state.mic_level.normalized();
                    let speaker_level = state.speaker_level.normalized();
                    let quantized =
                        QuantizedLevels::from_normalized(mic_level, speaker_level, &track_levels);
                    if state.last_sent_levels.as_ref() != Some(&quantized) {
                        state.last_sent_levels = Some(quantized);
                        let _ = channel.send(DesktopCallPacket::Levels {
                            mic_level,
                            speaker_level,
                            track_levels,
                        });
                    }
                }
            }
        }
    };

    drain_keyframe_task(&mut keyframe_task).await;
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
) -> Result<CallConnectResponse, String> {
    let endpoint = resolve_active_endpoint(&state, &connection_id, &account_id).await?;
    let ffmpeg = resolve_ffmpeg(&state.binaries);
    let session_id = state.calls.next_session.fetch_add(1, Ordering::AcqRel) + 1;

    // Warm the FFmpeg encoder capability cache before opening the control socket so no
    // socket events sit unread during probing.
    let ffmpeg_for_probe = ffmpeg.clone();
    let probed_video_caps = tokio::task::spawn_blocking(move || {
        video::client_video_capabilities(true, ffmpeg_for_probe.as_deref())
    })
    .await
    .ok()
    .flatten();

    let (mut control, hello_event) = ControlClient::connect(&endpoint)
        .await
        .map_err(|error| map_connect_error(&error))?;

    let daemon_supports_video = control.capabilities.direct_video
        || control.capabilities.conference_video
        || control.video_capabilities.is_some();
    let client_video_caps = if daemon_supports_video {
        probed_video_caps
    } else {
        None
    };
    let control_format = control.format;
    let hello_json = sanitized_event_value(&hello_event).map_err(|error| error.to_string())?;
    let sounding_rx = state.notifications.subscribe_sounding_calls();

    let (tx, previous) = {
        let mut accounts = state.calls.accounts.lock().await;
        if accounts
            .get(&account_id)
            .is_some_and(|existing| existing.session_id > session_id)
        {
            drop(accounts);
            control.close().await;
            return Err("closed".into());
        }
        let cancel = state.calls.root_cancel.child_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let actor_cancel = cancel.clone();
        let task = tauri::async_runtime::spawn(run_account_actor(
            endpoint,
            ffmpeg,
            control,
            rx,
            channel,
            sounding_rx,
            actor_cancel,
        ));
        let replaced = accounts.insert(
            account_id,
            AccountHandle {
                session_id,
                connection_id,
                cancel,
                tx: tx.clone(),
                task,
            },
        );
        (tx, replaced)
    };
    if let Some(old) = previous {
        old.cancel.cancel();
        let _ = timeout(Duration::from_secs(4), old.task).await;
    }

    // Send `call.ready` through the live actor so interleaved offers/state updates are
    // forwarded to the WebView in order rather than swallowed.
    let ready_cmd = CallCommand::Ready {
        request_id: uuid::Uuid::new_v4().to_string(),
        ready: true,
        format: control_format,
        scope: Some(CallReadyScope::All),
        video: client_video_caps,
    };
    if let Err(error) = send_actor_command(&tx, ready_cmd).await {
        debug_call(
            "desktop-control",
            format_args!("call.ready rejected or failed (staying connected): {error}"),
        );
    }

    Ok(CallConnectResponse {
        session_id,
        hello: hello_json,
    })
}

#[tauri::command]
pub async fn call_disconnect(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
    session_id: Option<u64>,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    state
        .calls
        .disconnect_account(&connection_id, &account_id, session_id)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn call_devices(state: State<'_, Desktop>) -> Result<CallDevicesSnapshotDto, String> {
    let ffmpeg = resolve_ffmpeg(&state.binaries);
    tokio::task::spawn_blocking(move || {
        let mut audio_inputs = Vec::new();
        let mut audio_outputs = Vec::new();
        let permission_state = match wechat_audio::AudioSession::devices() {
            Ok(list) => {
                for dev in list {
                    let Some(id) = dev.id.filter(|id| !id.trim().is_empty()) else {
                        continue;
                    };
                    let dto = CallDeviceDto {
                        id,
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
                "granted"
            }
            Err(_) => "unknown",
        };
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
            permission_state,
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
) -> Result<u64, String> {
    let _ = validate_camera_id_syntax(camera_id.as_deref())?;
    let ffmpeg = resolve_ffmpeg(&state.binaries);
    let gen = state.calls.preview_gen.fetch_add(1, Ordering::AcqRel) + 1;
    state.calls.stop_preview_older_than(gen).await;

    let capture = tokio::task::spawn_blocking(move || -> Result<LocalVideoCapture, String> {
        let source = resolve_verified_camera_source(camera_id.as_deref(), ffmpeg.as_deref())?;
        LocalVideoCapture::start_preview(&source, 480, 360, 24, ffmpeg.as_deref())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "Camera preview task failed".to_string())??;

    // If another preview start/stop or call start arrived while FFmpeg was booting,
    // immediately dispose this superseded capture off the Tokio worker thread and
    // return an error so stale callers never tear down the newer preview.
    if state.calls.preview_gen.load(Ordering::Acquire) != gen {
        stop_video_handles(Some(capture), None).await;
        return Err("preview_superseded".into());
    }

    let cancel = state.calls.root_cancel.child_token();
    let loop_cancel = cancel.clone();
    let task = tauri::async_runtime::spawn(async move {
        let mut capture = capture;
        loop {
            tokio::select! {
                _ = loop_cancel.cancelled() => break,
                frame = capture.recv_latest() => {
                    let Some(frame) = frame else { break };
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
        stop_video_handles(Some(capture), None).await;
        let _ = channel.send(DesktopPreviewPacket::Ended);
    });

    let replaced = {
        let mut guard = state.calls.preview.lock().await;
        if state.calls.preview_gen.load(Ordering::Acquire) != gen {
            cancel.cancel();
            drop(guard);
            let _ = timeout(TEARDOWN_TIMEOUT, task).await;
            return Err("preview_superseded".into());
        }
        guard.replace(PreviewSession {
            generation: gen,
            cancel,
            task,
        })
    };
    if let Some(old) = replaced {
        old.cancel.cancel();
        let _ = timeout(TEARDOWN_TIMEOUT, old.task).await;
    }
    Ok(gen)
}

#[tauri::command]
pub async fn call_preview_stop(
    state: State<'_, Desktop>,
    generation: Option<u64>,
) -> Result<(), String> {
    state.calls.stop_preview_if(generation).await;
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
    if !enabled {
        // Stop local camera eagerly before sending `call.camera(false)` so the daemon's
        // interleaved `track.updated(active: false)` never respawns FFmpeg.
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(ActorRequest::SetLocalCamera {
            enabled: false,
            reply: reply_tx,
        })
        .map_err(|_| "not_connected".to_string())?;
        let _ = reply_rx.await;
    }
    send_actor_command(
        &tx,
        CallCommand::Camera {
            request_id,
            call_id,
            enabled,
        },
    )
    .await?;
    if enabled {
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(ActorRequest::SetLocalCamera {
            enabled: true,
            reply: reply_tx,
        })
        .map_err(|_| "not_connected".to_string())?;
        reply_rx.await.map_err(|_| "closed".to_string())??;
    }
    Ok(())
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
        let _ = validate_camera_id_syntax(Some(camera_id))?;
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
        let _ = validate_camera_id_syntax(Some(camera_id))?;
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
    timeout(ATTACH_TIMEOUT, reply_rx)
        .await
        .map_err(|_| "media_attach_failed".to_string())?
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
pub async fn call_video_ack(
    state: State<'_, Desktop>,
    connection_id: String,
    account_id: String,
) -> Result<(), String> {
    validate_account_id(&account_id)?;
    if let Ok(tx) = state.calls.sender(&connection_id, &account_id).await {
        let _ = tx.send(ActorRequest::VideoFrameAck);
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
    fn ipc_camera_validation_enforces_strict_platform_rules() {
        assert_eq!(validate_camera_id_syntax(None).unwrap(), CameraSource::Auto);
        assert_eq!(
            validate_camera_id_syntax(Some("  ")).unwrap(),
            CameraSource::Auto
        );
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                validate_camera_id_syntax(Some("/dev/video0")).unwrap(),
                CameraSource::Device("/dev/video0".into())
            );
            for bad in [
                "/etc/passwd",
                "/dev/sda",
                "/dev/video",
                "/dev/video0/../sda",
                "http://127.0.0.1/x",
                "0",
            ] {
                assert!(
                    validate_camera_id_syntax(Some(bad)).is_err(),
                    "expected {bad:?} to be rejected on Linux"
                );
            }
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                validate_camera_id_syntax(Some("0")).unwrap(),
                CameraSource::Device("0".into())
            );
            for bad in ["0:0", ":0", "Capture screen 0", "/dev/video0"] {
                assert!(
                    validate_camera_id_syntax(Some(bad)).is_err(),
                    "expected {bad:?} to be rejected on macOS"
                );
            }
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                validate_camera_id_syntax(Some("Integrated Camera")).unwrap(),
                CameraSource::Device("Integrated Camera".into())
            );
            for bad in ["video=cam:0", "C:\\Windows\\cmd.exe", "cam\"name"] {
                assert!(
                    validate_camera_id_syntax(Some(bad)).is_err(),
                    "expected {bad:?} to be rejected on Windows"
                );
            }
        }
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
                validate_camera_id_syntax(Some(bad)).is_err(),
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

            let Message::Text(cmd_text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected text command");
            };
            let cmd: Value = serde_json::from_str(&cmd_text).unwrap();
            assert_eq!(cmd["type"], "call.dial");
            let req_id = cmd["requestId"].as_str().unwrap().to_owned();

            // Interleaved call.state arrives BEFORE command.result.
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

            // command.result carries secret ownerToken.
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

        let mut initial_sounding = HashMap::new();
        initial_sounding.insert("acc".into(), "offer-7".into());
        let (sounding_tx, sounding_rx) = watch::channel(initial_sounding);
        let actor_task = tokio::spawn(run_account_actor(
            endpoint,
            None,
            control,
            rx,
            channel,
            sounding_rx,
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

        sounding_tx.send(HashMap::new()).unwrap();
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
        assert!(
            packets
                .iter()
                .any(|p| p["type"] == "osCallAlert" && p["callId"] == "offer-7"),
            "initial sounding OS alert must be forwarded to the WebView: {packets:?}"
        );
        assert!(
            packets
                .iter()
                .any(|p| p["type"] == "osCallAlert" && p["callId"].is_null()),
            "cleared sounding OS alert must be forwarded to the WebView: {packets:?}"
        );
        for packet in packets.iter() {
            let serialized = serde_json::to_string(packet).unwrap();
            assert!(
                !serialized.contains("secret-owner-token-never-expose"),
                "ownerToken leaked to IPC channel: {serialized}"
            );
        }
    }

    #[tokio::test]
    async fn preview_stop_with_stale_generation_keeps_newer_preview_alive() {
        let manager = CallManager::default();
        let gen2 = manager.preview_gen.fetch_add(2, Ordering::AcqRel) + 2;
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tauri::async_runtime::spawn(async move {
            task_cancel.cancelled().await;
        });
        *manager.preview.lock().await = Some(PreviewSession {
            generation: gen2,
            cancel: cancel.clone(),
            task,
        });

        // A stale stop for generation 1 must leave generation 2 running and untouched.
        manager.stop_preview_if(Some(1)).await;
        assert!(!cancel.is_cancelled());
        assert!(manager.preview.lock().await.is_some());
        assert_eq!(manager.preview_gen.load(Ordering::Acquire), gen2);

        // Stopping generation 2 tears it down cleanly.
        manager.stop_preview_if(Some(gen2)).await;
        assert!(cancel.is_cancelled());
        assert!(manager.preview.lock().await.is_none());
    }

    #[test]
    fn quantized_levels_deduplicate_idle_noise_and_detect_track_changes() {
        let mut tracks_a = BTreeMap::new();
        tracks_a.insert(10, 0.002);
        let q1 = QuantizedLevels::from_normalized(0.0, 0.0, &tracks_a);
        let mut tracks_b = BTreeMap::new();
        tracks_b.insert(10, 0.004);
        let q2 = QuantizedLevels::from_normalized(0.003, 0.004, &tracks_b);
        assert_eq!(q1, q2, "sub-step fluctuations should not resend Levels IPC");

        let mut tracks_c = BTreeMap::new();
        tracks_c.insert(10, 0.12);
        let q3 = QuantizedLevels::from_normalized(0.0, 0.0, &tracks_c);
        assert_ne!(q1, q3, "meaningful track speech must trigger Levels IPC");

        let tracks_empty = BTreeMap::new();
        let q4 = QuantizedLevels::from_normalized(0.0, 0.0, &tracks_empty);
        assert_ne!(q1, q4, "removing a track must trigger Levels IPC");
    }

    #[test]
    fn video_batch_pacing_gates_while_in_flight_until_ack_or_timeout() {
        let t0 = Instant::now();
        assert!(can_dispatch_video_batch(None, t0));
        assert!(!can_dispatch_video_batch(
            Some(t0),
            t0 + Duration::from_millis(50)
        ));
        assert!(can_dispatch_video_batch(Some(t0), t0 + VIDEO_ACK_TIMEOUT));
    }
}
