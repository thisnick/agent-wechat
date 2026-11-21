//! Local camera enumeration, FFmpeg capture/encoding, and TrueColor half-block
//! terminal video rendering for `wx calls`.
//!
//! No camera device is ever opened during enumeration, monitoring, or while the
//! camera is turned off.

use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Instant,
};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;
use wechat_calls::video_codec::{find_ffmpeg, probe_video_capabilities};
use wechat_core::{
    call_types::{
        CallControl, CallEvent, CallTrack, CallTrackDirection, CallVideoCapabilities,
        CallVideoCodec, CallVideoFormat, CallView,
    },
    calls::pcm_envelope::{MediaPayloadKind, WireMediaFrame, FLAG_CODEC_CONFIG, FLAG_KEYFRAME},
};

/// A discovered local camera device (metadata only; never opened during listing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CameraDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Configured camera source for outgoing video calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum CameraSource {
    /// Automatically pick the first available local camera when camera is enabled.
    #[default]
    Auto,
    /// Disable local camera capture.
    Off,
    /// Generate a synthetic animated FFmpeg `testsrc2` pattern (for headless/container testing).
    Test,
    /// Loop a local video or image file via FFmpeg.
    File(PathBuf),
    /// Use an explicit camera device path or platform capture name.
    Device(String),
}

impl CameraSource {
    pub(super) fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        ensure!(
            !trimmed.is_empty() && !trimmed.chars().any(char::is_control),
            "invalid camera source"
        );
        if trimmed.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        if trimmed.eq_ignore_ascii_case("off") || trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self::Off);
        }
        if trimmed.eq_ignore_ascii_case("test") {
            return Ok(Self::Test);
        }
        if let Some(path) = trimmed.strip_prefix("file:") {
            ensure!(!path.trim().is_empty(), "camera file path is required");
            return Ok(Self::File(PathBuf::from(path)));
        }
        Ok(Self::Device(trimmed.to_owned()))
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Off => "off".into(),
            Self::Test => "test".into(),
            Self::File(path) => format!("file:{}", path.display()),
            Self::Device(device) => device.clone(),
        }
    }

    pub(super) fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}

/// Enumerate local camera devices without opening or capturing any device.
pub(super) fn list_cameras() -> Vec<CameraDevice> {
    #[cfg(target_os = "linux")]
    {
        list_linux_v4l2_cameras()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn list_linux_v4l2_cameras() -> Vec<CameraDevice> {
    let Ok(entries) = std::fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut nodes: Vec<(u32, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(num_str) = name.strip_prefix("video") else {
            continue;
        };
        let Ok(num) = num_str.parse::<u32>() else {
            continue;
        };
        let dev_path = PathBuf::from("/dev").join(&name);
        if !dev_path.exists() {
            continue;
        }
        let sys_path = entry.path();
        // Linux V4L2 creates secondary metadata nodes (index >= 1) next to each
        // capture node (index == 0). Skip secondary nodes when index is reported.
        if let Ok(index) = std::fs::read_to_string(sys_path.join("index")) {
            if index.trim().parse::<u32>().is_ok_and(|idx| idx != 0) {
                continue;
            }
        }
        let label = std::fs::read_to_string(sys_path.join("name"))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| format!("Camera {num}"));
        nodes.push((num, label, dev_path));
    }
    nodes.sort_by_key(|(num, _, _)| *num);
    nodes
        .into_iter()
        .enumerate()
        .map(|(idx, (_, name, dev_path))| CameraDevice {
            id: dev_path.display().to_string(),
            name,
            is_default: idx == 0,
        })
        .collect()
}

/// Compute video capabilities this CLI should advertise in `call.ready`.
///
/// The CLI renders received video from standalone JPEG frames (decoded locally
/// via the `image` crate) and encodes outgoing video via FFmpeg into whichever
/// codec the local FFmpeg installation supports.
pub(super) fn client_video_capabilities(
    daemon_supports_video: bool,
) -> Option<CallVideoCapabilities> {
    if !daemon_supports_video {
        return None;
    }
    let host = probe_video_capabilities();
    let mut send_codecs = Vec::new();
    if host.ffmpeg_path.is_some() {
        if host.h265_encode {
            send_codecs.push(CallVideoCodec::H265);
        }
        if host.h264_encode {
            send_codecs.push(CallVideoCodec::H264);
        }
        if host.jpeg_encode {
            send_codecs.push(CallVideoCodec::Jpeg);
        }
    }
    Some(CallVideoCapabilities {
        send_codecs,
        receive_codecs: vec![CallVideoCodec::Jpeg],
        max_width: Some(640),
        max_height: Some(480),
        max_fps: Some(15),
    })
}

/// One encoded outgoing video frame ready to wrap in `WXCM` v3.
pub(super) struct CapturedVideoFrame {
    pub codec: CallVideoCodec,
    pub flags: u16,
    pub timestamp_micros: u64,
    pub bytes: Vec<u8>,
}

struct CaptureWorker {
    child: Child,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    frames: mpsc::Receiver<CapturedVideoFrame>,
}

/// Live FFmpeg camera capture and encoder subprocess.
///
/// Spawned only while the local camera is enabled in a connected call, and
/// terminated immediately when the camera is turned off or the call ends.
pub(super) struct LocalVideoCapture {
    source: CameraSource,
    format: CallVideoFormat,
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    frames: mpsc::Receiver<CapturedVideoFrame>,
    started_at: Instant,
}

impl LocalVideoCapture {
    pub(super) fn start(source: &CameraSource, format: CallVideoFormat) -> Result<Self> {
        ensure!(!source.is_off(), "camera source is set to off");
        let ffmpeg = find_ffmpeg().context(
            "ffmpeg is required for outgoing camera capture; install ffmpeg or pass --camera-off",
        )?;
        let started_at = Instant::now();
        let worker = spawn_capture(&ffmpeg, source, format, started_at)?;
        Ok(Self {
            source: source.clone(),
            format,
            child: Some(worker.child),
            stop: worker.stop,
            reader: Some(worker.reader),
            frames: worker.frames,
            started_at,
        })
    }

    pub(super) fn try_recv(&mut self) -> Option<CapturedVideoFrame> {
        // Always keep the freshest encoded frame when a consumer lags slightly.
        let mut latest = None;
        while let Ok(frame) = self.frames.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    pub(super) fn request_keyframe(&mut self) -> Result<()> {
        // Capture pipelines already emit inline-header IDR keyframes every 1.0 s.
        // Avoid killing and respawning FFmpeg on routine keyframe requests because
        // doing so blocks the 20 ms audio loop and resets camera/testsrc2 state;
        // only restart if the child process has unexpectedly exited.
        let exited = self
            .child
            .as_mut()
            .is_none_or(|child| matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
        if !exited {
            return Ok(());
        }
        self.stop_worker();
        let ffmpeg = find_ffmpeg().context("ffmpeg unavailable for keyframe restart")?;
        let worker = spawn_capture(&ffmpeg, &self.source, self.format, self.started_at)?;
        self.child = Some(worker.child);
        self.stop = worker.stop;
        self.reader = Some(worker.reader);
        self.frames = worker.frames;
        Ok(())
    }

    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for LocalVideoCapture {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

fn resolve_device_input(source: &CameraSource) -> Result<String> {
    match source {
        CameraSource::Device(device) => Ok(device.clone()),
        CameraSource::Auto => {
            #[cfg(target_os = "linux")]
            {
                if let Some(first) = list_cameras().into_iter().next() {
                    return Ok(first.id);
                }
                bail!("no local camera found; connect a camera, pass --camera test, or pass --camera-off");
            }
            #[cfg(target_os = "macos")]
            {
                Ok("0".into())
            }
            #[cfg(target_os = "windows")]
            {
                bail!("pass --camera DEVICE_NAME, --camera test, or --camera-off on Windows")
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            {
                bail!("no default camera capture backend; pass --camera test or --camera-off")
            }
        }
        CameraSource::Off | CameraSource::Test | CameraSource::File(_) => {
            unreachable!("handled before resolve_device_input")
        }
    }
}

fn spawn_capture(
    ffmpeg: &std::path::Path,
    source: &CameraSource,
    format: CallVideoFormat,
    started_at: Instant,
) -> Result<CaptureWorker> {
    let width = format.max_width.clamp(160, 640) & !1;
    let height = format.max_height.clamp(120, 480) & !1;
    let fps = format.max_fps.clamp(1, 15);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error"]);

    match source {
        CameraSource::Test => {
            cmd.args([
                "-re",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={width}x{height}:rate={fps}"),
            ]);
        }
        CameraSource::File(path) => {
            ensure!(path.is_file(), "camera file not found: {}", path.display());
            cmd.args(["-re", "-stream_loop", "-1", "-i"]);
            cmd.arg(path);
        }
        CameraSource::Auto | CameraSource::Device(_) => {
            let device = resolve_device_input(source)?;
            #[cfg(target_os = "linux")]
            {
                cmd.args([
                    "-f",
                    "v4l2",
                    "-framerate",
                    &fps.to_string(),
                    "-video_size",
                    &format!("{width}x{height}"),
                    "-i",
                    &device,
                ]);
            }
            #[cfg(target_os = "macos")]
            {
                cmd.args([
                    "-f",
                    "avfoundation",
                    "-framerate",
                    &fps.to_string(),
                    "-i",
                    &device,
                ]);
            }
            #[cfg(target_os = "windows")]
            {
                cmd.args([
                    "-f",
                    "dshow",
                    "-framerate",
                    &fps.to_string(),
                    "-i",
                    &format!("video={device}"),
                ]);
            }
        }
        CameraSource::Off => bail!("camera source is off"),
    }

    let scale = format!(
        "fps={fps},scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2"
    );
    cmd.args(["-vf", &scale, "-an", "-sn", "-dn"]);

    let gop = u32::from(fps).max(1);
    let kbps = format.bitrate_kbps.unwrap_or(350).clamp(120, 1500);
    let bitrate = format!("{kbps}k");
    let maxrate = format!("{}k", kbps.saturating_mul(3) / 2);
    let bufsize = format!("{kbps}k");

    match format.codec {
        CallVideoCodec::H265 => {
            let x265_params = format!(
                "repeat-headers=1:bframes=0:frame-threads=1:keyint={gop}:min-keyint=1:scenecut=0:rc-lookahead=0:log-level=error"
            );
            cmd.args([
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-b:v",
                &bitrate,
                "-maxrate",
                &maxrate,
                "-bufsize",
                &bufsize,
                "-x265-params",
                &x265_params,
                "-flush_packets",
                "1",
                "-f",
                "hevc",
                "pipe:1",
            ]);
        }
        CallVideoCodec::H264 => {
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-g",
                &gop.to_string(),
                "-keyint_min",
                "1",
                "-sc_threshold",
                "0",
                "-bf",
                "0",
                "-b:v",
                &bitrate,
                "-maxrate",
                &maxrate,
                "-bufsize",
                &bufsize,
                "-x264-params",
                "repeat-headers=1:threads=1",
                "-flush_packets",
                "1",
                "-f",
                "h264",
                "pipe:1",
            ]);
        }
        CallVideoCodec::Jpeg => {
            cmd.args([
                "-c:v",
                "mjpeg",
                "-q:v",
                "7",
                "-flush_packets",
                "1",
                "-f",
                "mjpeg",
                "pipe:1",
            ]);
        }
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start ffmpeg camera capture")?;
    let stdout = child.stdout.take().context("missing ffmpeg video stdout")?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (tx, rx) = mpsc::channel(4);
    let codec = format.codec;
    let reader = thread::Builder::new()
        .name("wx-call-video-cap".into())
        .spawn(move || read_captured_stream(stdout, codec, worker_stop, started_at, tx))
        .context("failed to spawn video capture reader")?;
    Ok(CaptureWorker {
        child,
        stop,
        reader,
        frames: rx,
    })
}

fn read_captured_stream(
    mut stdout: impl Read,
    codec: CallVideoCodec,
    stop: Arc<AtomicBool>,
    started_at: Instant,
    tx: mpsc::Sender<CapturedVideoFrame>,
) {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 8192];
    let mut pending_au = Vec::<u8>::new();
    let mut pending_has_slice = false;
    let mut pending_is_keyframe = false;
    let mut pending_has_config = false;

    while !stop.load(Ordering::Acquire) {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        let ts = started_at.elapsed().as_micros() as u64;
        match codec {
            CallVideoCodec::Jpeg => {
                while let Some(frame) = extract_jpeg_frame(&mut buf) {
                    let _ = tx.try_send(CapturedVideoFrame {
                        codec: CallVideoCodec::Jpeg,
                        flags: FLAG_KEYFRAME,
                        timestamp_micros: ts,
                        bytes: frame,
                    });
                }
            }
            CallVideoCodec::H265 | CallVideoCodec::H264 => {
                drain_annexb_frames(
                    &mut buf,
                    codec,
                    &mut pending_au,
                    &mut pending_has_slice,
                    &mut pending_is_keyframe,
                    &mut pending_has_config,
                    ts,
                    &tx,
                );
            }
        }
    }
}

fn extract_jpeg_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buf.windows(2).position(|w| w == [0xff, 0xd8])?;
    if start > 0 {
        buf.drain(..start);
    }
    let end_rel = buf[2..].windows(2).position(|w| w == [0xff, 0xd9])?;
    let end = 2 + end_rel + 2;
    Some(buf.drain(..end).collect())
}

fn split_start_codes(data: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, 3));
                i += 3;
                continue;
            }
            if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    let mut nalus = Vec::with_capacity(starts.len());
    for idx in 0..starts.len() {
        let (start, sc_len) = starts[idx];
        let end = starts.get(idx + 1).map_or(data.len(), |(next, _)| *next);
        if end > start + sc_len {
            nalus.push((start, end, sc_len));
        }
    }
    nalus
}

#[allow(clippy::too_many_arguments)]
fn drain_annexb_frames(
    buf: &mut Vec<u8>,
    codec: CallVideoCodec,
    pending_au: &mut Vec<u8>,
    pending_has_slice: &mut bool,
    pending_is_keyframe: &mut bool,
    pending_has_config: &mut bool,
    timestamp_micros: u64,
    tx: &mpsc::Sender<CapturedVideoFrame>,
) {
    let nalus = split_start_codes(buf);
    if nalus.len() < 2 {
        return;
    }
    let mut consumed = 0usize;
    for &(start, end, sc_len) in &nalus[..nalus.len() - 1] {
        let nalu = &buf[start..end];
        consumed = end;
        let header = nalu[sc_len];
        let (is_param, is_vcl, is_idr, is_aud) = match codec {
            CallVideoCodec::H265 => {
                let t = (header >> 1) & 0x3f;
                (matches!(t, 32..=34), t < 32, matches!(t, 16..=21), t == 35)
            }
            CallVideoCodec::H264 => {
                let t = header & 0x1f;
                (matches!(t, 7 | 8), matches!(t, 1..=5), t == 5, t == 9)
            }
            CallVideoCodec::Jpeg => (false, true, true, false),
        };
        if *pending_has_slice && (is_param || is_vcl || is_aud) {
            let mut flags = 0u16;
            if *pending_is_keyframe {
                flags |= FLAG_KEYFRAME;
            }
            if *pending_has_config {
                flags |= FLAG_CODEC_CONFIG;
            }
            let _ = tx.try_send(CapturedVideoFrame {
                codec,
                flags,
                timestamp_micros,
                bytes: std::mem::take(pending_au),
            });
            *pending_has_slice = false;
            *pending_is_keyframe = false;
            *pending_has_config = false;
        }
        pending_au.extend_from_slice(nalu);
        *pending_has_config |= is_param;
        *pending_is_keyframe |= is_idr;
        *pending_has_slice |= is_vcl;
    }
    if consumed > 0 {
        buf.drain(..consumed);
    }
}

/// Decoded RGB24 frame held for terminal half-block rendering.
#[derive(Clone)]
struct DecodedFrame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

struct DecodeJob {
    track_id: u32,
    generation: u32,
    rotation_degrees: u16,
    jpeg: Vec<u8>,
}

struct DecodeMailboxState {
    order: VecDeque<u32>,
    pending: BTreeMap<u32, DecodeJob>,
    closed: bool,
}

/// Per-track latest-frame mailbox: incoming JPEGs always overwrite any older
/// unstarted JPEG for the same track without dropping burst arrivals or starving
/// other conference participants.
struct DecodeMailbox {
    state: Mutex<DecodeMailboxState>,
    ready: Condvar,
}

impl DecodeMailbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(DecodeMailboxState {
                order: VecDeque::new(),
                pending: BTreeMap::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn push(&self, job: DecodeJob) -> bool {
        let Ok(mut guard) = self.state.lock() else {
            return false;
        };
        if guard.closed {
            return false;
        }
        let track_id = job.track_id;
        if guard.pending.insert(track_id, job).is_none() {
            guard.order.push_back(track_id);
        }
        self.ready.notify_one();
        true
    }

    fn pop_blocking(&self) -> Option<DecodeJob> {
        let mut guard = self.state.lock().ok()?;
        loop {
            if let Some(track_id) = guard.order.pop_front() {
                if let Some(job) = guard.pending.remove(&track_id) {
                    return Some(job);
                }
            }
            if guard.closed {
                return None;
            }
            guard = self.ready.wait(guard).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.closed = true;
            guard.pending.clear();
            guard.order.clear();
        }
        self.ready.notify_all();
    }
}

struct DecodedJob {
    track_id: u32,
    generation: u32,
    frame: DecodedFrame,
}

struct RemoteVideoTrack {
    track: CallTrack,
    label: String,
    active: bool,
    frame: Option<DecodedFrame>,
    frames_received: u64,
}

/// Tracks active remote video streams and renders them into 24-bit TrueColor
/// Unicode half-block (`▀`) terminal rows.
///
/// JPEG decoding and display rotation run on a dedicated background worker
/// thread so video frames never stall the 20 ms audio loop.
pub(super) struct ReceiveVideo {
    tracks: BTreeMap<u32, RemoteVideoTrack>,
    retired: BTreeMap<u32, u32>,
    peer_label: Option<String>,
    peer_camera_off: bool,
    mailbox: Arc<DecodeMailbox>,
    decoded_rx: std::sync::mpsc::Receiver<DecodedJob>,
    decoder: Option<JoinHandle<()>>,
}

impl ReceiveVideo {
    pub(super) fn new(tracks: Vec<CallTrack>) -> Self {
        let mailbox = Arc::new(DecodeMailbox::new());
        let worker_mailbox = mailbox.clone();
        let (decoded_tx, decoded_rx) = std::sync::mpsc::sync_channel::<DecodedJob>(8);
        let decoder = thread::Builder::new()
            .name("wx-call-video-dec".into())
            .spawn(move || {
                while let Some(job) = worker_mailbox.pop_blocking() {
                    let Ok(img) =
                        image::load_from_memory_with_format(&job.jpeg, image::ImageFormat::Jpeg)
                    else {
                        continue;
                    };
                    let img = match job.rotation_degrees {
                        90 => img.rotate90(),
                        180 => img.rotate180(),
                        270 => img.rotate270(),
                        _ => img,
                    };
                    let rgb = img.to_rgb8();
                    let (width, height) = rgb.dimensions();
                    if width == 0 || height == 0 {
                        continue;
                    }
                    let _ = decoded_tx.try_send(DecodedJob {
                        track_id: job.track_id,
                        generation: job.generation,
                        frame: DecodedFrame {
                            width,
                            height,
                            rgb: rgb.into_raw(),
                        },
                    });
                }
            })
            .ok();
        let mut value = Self {
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            peer_label: None,
            peer_camera_off: false,
            mailbox,
            decoded_rx,
            decoder,
        };
        for track in tracks {
            if track.is_video() && track.direction() == CallTrackDirection::Receive {
                value.upsert_track(track);
            }
        }
        value
    }

    pub(super) fn upsert_track(&mut self, track: CallTrack) {
        if !track.is_video() || track.direction() != CallTrackDirection::Receive {
            return;
        }
        let track_id = track.id();
        let generation = track.generation();
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return;
        }
        let active = match &track {
            CallTrack::Video { active, .. } => *active,
            CallTrack::Audio { .. } => false,
        };
        if let Some(existing) = self.tracks.get_mut(&track_id) {
            if generation < existing.track.generation() {
                return;
            }
            if generation > existing.track.generation() {
                existing.frame = None;
            }
            existing.track = track;
            existing.active = active;
            return;
        }
        let label = track
            .participant_id()
            .map(|id| super::levels::clean_label(id, 32))
            .or_else(|| self.peer_label.clone())
            .unwrap_or_else(|| "Peer".into());
        self.tracks.insert(
            track_id,
            RemoteVideoTrack {
                track,
                label,
                active,
                frame: None,
                frames_received: 0,
            },
        );
    }

    pub(super) fn event(&mut self, event: &CallEvent, call_id: &str) -> bool {
        match event {
            CallEvent::TrackAdded { call_id: id, track }
            | CallEvent::TrackUpdated { call_id: id, track }
                if id == call_id
                    && track.is_video()
                    && track.direction() == CallTrackDirection::Receive =>
            {
                self.upsert_track(track.clone());
                true
            }
            CallEvent::TrackRemoved {
                call_id: id,
                track_id,
                generation,
                ..
            } if id == call_id => {
                if self.retired.len() < 256 || self.retired.contains_key(track_id) {
                    self.retired
                        .entry(*track_id)
                        .and_modify(|old| *old = (*old).max(*generation))
                        .or_insert(*generation);
                }
                if self
                    .tracks
                    .get(track_id)
                    .is_some_and(|entry| entry.track.generation() <= *generation)
                {
                    self.tracks.remove(track_id);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    pub(super) fn roster(&mut self, call: &CallView) {
        if let Some(control) = call.controls.iter().find_map(|entry| match entry.control {
            CallControl::Camera { enabled } => Some((&entry.participant_id, enabled)),
            _ => None,
        }) {
            self.peer_label = Some(super::levels::clean_label(control.0, 32));
            self.peer_camera_off = !control.1;
        } else if let Some(first) = call.controls.first() {
            self.peer_label = Some(super::levels::clean_label(&first.participant_id, 32));
        }

        for entry in self.tracks.values_mut() {
            if let Some(pid) = entry.track.participant_id() {
                if let Some(participant) =
                    call.participants.iter().find(|p| p.participant_id == pid)
                {
                    entry.label = super::levels::clean_label(&participant.contact_id, 32);
                    if let Some(cam) = participant.camera_enabled {
                        entry.active = cam;
                    }
                }
            } else if let Some(peer) = &self.peer_label {
                entry.label = peer.clone();
            }
        }
    }

    pub(super) fn set_peer_label(&mut self, label: &str) {
        let cleaned = super::levels::clean_label(label, 32);
        if cleaned.is_empty() {
            return;
        }
        self.peer_label = Some(cleaned.clone());
        for entry in self.tracks.values_mut() {
            if entry.track.participant_id().is_none() {
                entry.label = cleaned.clone();
            }
        }
    }

    /// Non-blocking handoff of an incoming JPEG frame to the background decode thread.
    pub(super) fn binary_frame(&mut self, wire: &WireMediaFrame<'_>) -> bool {
        if wire.kind != MediaPayloadKind::Jpeg {
            return false;
        }
        let track_id = wire.header.track_id;
        let generation = wire.header.generation;
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return false;
        }
        let Some(entry) = self.tracks.get(&track_id) else {
            return false;
        };
        if generation != entry.track.generation() {
            return false;
        }
        self.mailbox.push(DecodeJob {
            track_id,
            generation,
            rotation_degrees: wire.rotation_degrees(),
            jpeg: wire.payload().to_vec(),
        })
    }

    /// Drain any frames completed by the background JPEG decoder without blocking.
    pub(super) fn poll_decoded(&mut self) -> bool {
        let mut updated = false;
        while let Ok(done) = self.decoded_rx.try_recv() {
            if self
                .retired
                .get(&done.track_id)
                .is_some_and(|old| done.generation <= *old)
            {
                continue;
            }
            let Some(entry) = self.tracks.get_mut(&done.track_id) else {
                continue;
            };
            if done.generation != entry.track.generation() {
                continue;
            }
            entry.frame = Some(done.frame);
            entry.active = true;
            entry.frames_received = entry.frames_received.saturating_add(1);
            if entry.track.participant_id().is_none() {
                self.peer_camera_off = false;
            }
            updated = true;
        }
        updated
    }

    pub(super) fn has_visible_video(&self) -> bool {
        !self.tracks.is_empty()
    }

    pub(super) fn active_track_count(&self) -> usize {
        self.tracks.values().filter(|entry| entry.active).count()
    }

    /// Render active video track(s) as 24-bit TrueColor Unicode half-block rows.
    ///
    /// Returns an empty `Vec` when no remote video tracks exist so voice calls
    /// keep the clean audio-only layout.
    pub(super) fn render_rows(&self, columns: usize, max_rows: usize) -> Vec<String> {
        if self.tracks.is_empty() || columns < 24 || max_rows < 4 {
            return Vec::new();
        }
        let tiles: Vec<&RemoteVideoTrack> = self.tracks.values().take(4).collect();
        match tiles.len() {
            1 => render_single_tile(tiles[0], self.peer_camera_off, columns, max_rows),
            2 => render_two_tiles(tiles[0], tiles[1], columns, max_rows),
            _ => render_quad_tiles(&tiles, columns, max_rows),
        }
    }
}

impl Drop for ReceiveVideo {
    fn drop(&mut self) {
        self.mailbox.close();
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}

fn render_single_tile(
    tile: &RemoteVideoTrack,
    peer_camera_off: bool,
    columns: usize,
    max_rows: usize,
) -> Vec<String> {
    let image_rows_budget = max_rows.saturating_sub(1).max(2);
    let (tile_w, tile_h) = fit_aspect(
        tile.frame.as_ref(),
        columns.saturating_sub(2),
        image_rows_budget,
    );
    let pad_left = columns.saturating_sub(tile_w) / 2;
    let margin = " ".repeat(pad_left);
    let status = if peer_camera_off || !tile.active {
        "camera off"
    } else if tile.frame.is_some() {
        "live"
    } else {
        "waiting for video"
    };
    let caption = format_caption(&tile.label, status, tile_w);
    let mut rows = Vec::with_capacity(tile_h + 1);
    rows.push(format!("{margin}{caption}"));
    let body = render_tile_body(tile, peer_camera_off, tile_w, tile_h);
    for line in body {
        rows.push(format!("{margin}{line}"));
    }
    rows
}

fn render_two_tiles(
    left: &RemoteVideoTrack,
    right: &RemoteVideoTrack,
    columns: usize,
    max_rows: usize,
) -> Vec<String> {
    let gap = 2;
    let each_max_w = columns.saturating_sub(gap + 2) / 2;
    if each_max_w < 14 {
        return render_single_tile(left, false, columns, max_rows);
    }
    let image_rows_budget = max_rows.saturating_sub(1).max(2);
    let (tile_w, tile_h) = fit_aspect(
        left.frame.as_ref().or(right.frame.as_ref()),
        each_max_w,
        image_rows_budget,
    );
    let total_w = tile_w * 2 + gap;
    let pad_left = columns.saturating_sub(total_w) / 2;
    let margin = " ".repeat(pad_left);
    let spacer = " ".repeat(gap);

    let left_cap = format_caption(&left.label, tile_status(left, false), tile_w);
    let right_cap = format_caption(&right.label, tile_status(right, false), tile_w);
    let left_body = render_tile_body(left, false, tile_w, tile_h);
    let right_body = render_tile_body(right, false, tile_w, tile_h);

    let mut rows = Vec::with_capacity(tile_h + 1);
    rows.push(format!("{margin}{left_cap}{spacer}{right_cap}"));
    for (l, r) in left_body.into_iter().zip(right_body) {
        rows.push(format!("{margin}{l}{spacer}{r}"));
    }
    rows
}

fn render_quad_tiles(tiles: &[&RemoteVideoTrack], columns: usize, max_rows: usize) -> Vec<String> {
    let half_rows = max_rows / 2;
    if half_rows < 4 {
        return render_two_tiles(tiles[0], tiles[1], columns, max_rows);
    }
    let mut rows = render_two_tiles(tiles[0], tiles[1], columns, half_rows);
    if tiles.len() == 3 {
        rows.extend(render_single_tile(
            tiles[2],
            false,
            columns,
            max_rows - rows.len(),
        ));
    } else if tiles.len() >= 4 {
        rows.extend(render_two_tiles(
            tiles[2],
            tiles[3],
            columns,
            max_rows - rows.len(),
        ));
    }
    rows
}

fn tile_status(tile: &RemoteVideoTrack, peer_camera_off: bool) -> &'static str {
    if peer_camera_off || !tile.active {
        "camera off"
    } else if tile.frame.is_some() {
        "live"
    } else {
        "waiting"
    }
}

fn fit_aspect(frame: Option<&DecodedFrame>, max_cols: usize, max_rows: usize) -> (usize, usize) {
    let (src_w, src_h) = frame.map_or((4_usize, 3_usize), |f| {
        (f.width as usize, f.height as usize)
    });
    // Each terminal row carries 2 square-ish vertical pixels (`▀`).
    let max_px_w = max_cols.max(8);
    let max_px_h = (max_rows * 2).max(4);
    let by_height_w = (max_px_h * src_w) / src_h.max(1);
    let (px_w, px_h) = if by_height_w <= max_px_w {
        (by_height_w.max(8), max_px_h)
    } else {
        let by_width_h = (max_px_w * src_h) / src_w.max(1);
        (max_px_w, by_width_h.max(4))
    };
    let rows = (px_h / 2).clamp(2, max_rows);
    let cols = px_w.clamp(8, max_cols);
    (cols, rows)
}

fn format_caption(label: &str, status: &str, width: usize) -> String {
    let raw = format!(" {label} ({status}) ");
    let clipped = super::levels::clean_label(&raw, width);
    let fill = width.saturating_sub(clipped.width());
    format!(
        "\x1b[1;38;2;230;236;245;48;2;36;43;56m{clipped}{}\x1b[0m",
        " ".repeat(fill)
    )
}

fn render_tile_body(
    tile: &RemoteVideoTrack,
    peer_camera_off: bool,
    cols: usize,
    rows: usize,
) -> Vec<String> {
    if peer_camera_off || !tile.active {
        return render_placeholder(cols, rows, "Camera off");
    }
    let Some(frame) = &tile.frame else {
        return render_placeholder(cols, rows, "Waiting for video...");
    };
    render_halfblock(frame, cols, rows)
}

fn render_placeholder(cols: usize, rows: usize, message: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(rows);
    let mid = rows / 2;
    let clipped = super::levels::clean_label(message, cols.saturating_sub(2));
    let left_pad = cols.saturating_sub(clipped.width()) / 2;
    let right_pad = cols.saturating_sub(left_pad + clipped.width());
    for row in 0..rows {
        if row == mid {
            out.push(format!(
                "\x1b[38;2;150;160;175;48;2;24;28;36m{}{clipped}{}\x1b[0m",
                " ".repeat(left_pad),
                " ".repeat(right_pad)
            ));
        } else {
            out.push(format!("\x1b[48;2;24;28;36m{}\x1b[0m", " ".repeat(cols)));
        }
    }
    out
}

/// Convert an RGB24 frame into `rows` ANSI TrueColor strings of `cols` visible
/// columns using Unicode upper half-blocks (`▀`), packing 2 vertical pixels per cell.
fn render_halfblock(frame: &DecodedFrame, cols: usize, rows: usize) -> Vec<String> {
    let src_w = frame.width as usize;
    let src_h = frame.height as usize;
    let dst_h = rows * 2;
    let mut out = Vec::with_capacity(rows);

    let sample = |x: usize, y: usize| -> (u8, u8, u8) {
        let sx = (x * src_w / cols.max(1)).min(src_w - 1);
        let sy = (y * src_h / dst_h.max(1)).min(src_h - 1);
        let offset = (sy * src_w + sx) * 3;
        (
            frame.rgb[offset],
            frame.rgb[offset + 1],
            frame.rgb[offset + 2],
        )
    };

    for row in 0..rows {
        let mut line = String::with_capacity(cols * 24);
        let mut prev_top: Option<(u8, u8, u8)> = None;
        let mut prev_bot: Option<(u8, u8, u8)> = None;
        for col in 0..cols {
            let top = sample(col, row * 2);
            let bot = sample(col, row * 2 + 1);
            if prev_top != Some(top) {
                let _ = write!(line, "\x1b[38;2;{};{};{}m", top.0, top.1, top.2);
                prev_top = Some(top);
            }
            if prev_bot != Some(bot) {
                let _ = write!(line, "\x1b[48;2;{};{};{}m", bot.0, bot.1, bot.2);
                prev_bot = Some(bot);
            }
            line.push('▀');
        }
        line.push_str("\x1b[0m");
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wechat_core::call_types::CallVideoMode;
    use wechat_core::calls::pcm_envelope::{encode_video, MediaHeader};

    fn sample_jpeg(r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut img = image::RgbImage::new(32, 24);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([
                r.saturating_add((x as u8).saturating_mul(2)),
                g.saturating_add((y as u8).saturating_mul(2)),
                b,
            ]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn video_track(id: u32, participant_id: Option<&str>) -> CallTrack {
        CallTrack::Video {
            id,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallVideoFormat {
                codec: CallVideoCodec::Jpeg,
                mode: CallVideoMode::Transcoded,
                max_width: 320,
                max_height: 240,
                max_fps: 10,
                bitrate_kbps: None,
            },
            participant_id: participant_id.map(str::to_owned),
            active: true,
        }
    }

    #[test]
    fn camera_source_parses_all_modes() {
        assert_eq!(CameraSource::parse("auto").unwrap(), CameraSource::Auto);
        assert_eq!(CameraSource::parse("off").unwrap(), CameraSource::Off);
        assert_eq!(CameraSource::parse("test").unwrap(), CameraSource::Test);
        assert_eq!(
            CameraSource::parse("file:/tmp/sample.mp4").unwrap(),
            CameraSource::File(PathBuf::from("/tmp/sample.mp4"))
        );
        assert_eq!(
            CameraSource::parse("/dev/video0").unwrap(),
            CameraSource::Device("/dev/video0".into())
        );
        assert!(CameraSource::parse("").is_err());
        assert!(CameraSource::parse("file:").is_err());
        assert!(CameraSource::parse("bad\nname").is_err());
    }

    #[test]
    fn halfblock_renderer_decodes_jpeg_for_single_and_conference_tiles() {
        let mut rx = ReceiveVideo::new(vec![video_track(65_538, None)]);
        rx.set_peer_label("alice");
        let jpg_a = sample_jpeg(200, 40, 20);
        let wire_a = encode_video(
            MediaPayloadKind::Jpeg,
            FLAG_KEYFRAME,
            MediaHeader {
                track_id: 65_538,
                generation: 1,
                sequence: 0,
                timestamp_micros: 0,
            },
            &jpg_a,
        )
        .unwrap();
        let decoded_a = WireMediaFrame::decode(&wire_a).unwrap();
        assert!(rx.binary_frame(&decoded_a));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !rx.poll_decoded() {
            assert!(Instant::now() < deadline, "timed out waiting for JPEG a");
            thread::sleep(Duration::from_millis(5));
        }
        let single = rx.render_rows(80, 14);
        assert!(single.len() >= 4 && single.len() <= 14);
        assert!(single[0].contains("alice (live)"));
        assert!(single[1].contains('▀'));
        assert!(single[1].contains("\x1b[38;2;"));

        // Add second conference peer and verify 2-tile side-by-side grid.
        rx.upsert_track(video_track(65_542, Some("bob")));
        let jpg_b = sample_jpeg(20, 180, 220);
        let wire_b = encode_video(
            MediaPayloadKind::Jpeg,
            FLAG_KEYFRAME,
            MediaHeader {
                track_id: 65_542,
                generation: 1,
                sequence: 0,
                timestamp_micros: 0,
            },
            &jpg_b,
        )
        .unwrap();
        let decoded_b = WireMediaFrame::decode(&wire_b).unwrap();
        assert!(rx.binary_frame(&decoded_b));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !rx.poll_decoded() {
            assert!(Instant::now() < deadline, "timed out waiting for JPEG b");
            thread::sleep(Duration::from_millis(5));
        }
        let grid = rx.render_rows(80, 14);
        assert!(grid[0].contains("alice (live)"));
        assert!(grid[0].contains("bob (live)"));
        assert!(grid[1].contains('▀'));
    }

    #[test]
    fn rotation_flag_rotates_decoded_jpeg_without_restarting_stream() {
        let mut rx = ReceiveVideo::new(vec![video_track(65_538, None)]);
        // 16x8 landscape JPEG rotated 90 degrees CW must become 8x16 portrait.
        let img =
            image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([x as u8 * 10, y as u8 * 20, 80]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        let jpg = out.into_inner();
        let wire = encode_video(
            MediaPayloadKind::Jpeg,
            FLAG_KEYFRAME | wechat_core::calls::pcm_envelope::FLAG_ROTATE_90,
            MediaHeader {
                track_id: 65_538,
                generation: 1,
                sequence: 0,
                timestamp_micros: 0,
            },
            &jpg,
        )
        .unwrap();
        let decoded = WireMediaFrame::decode(&wire).unwrap();
        assert_eq!(decoded.rotation_degrees(), 90);
        assert!(rx.binary_frame(&decoded));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !rx.poll_decoded() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for rotated JPEG"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let frame = rx.tracks[&65_538].frame.as_ref().unwrap();
        assert_eq!((frame.width, frame.height), (8, 16));
    }
}
