//! Shared local camera enumeration, FFmpeg capture/encoding, and remote video
//! track reception for both `wx calls` CLI and the desktop Tauri client.
//!
//! No camera device is ever opened during enumeration, monitoring, or while the
//! camera is turned off.

use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, Notify};
use wechat_call_contracts::{
    call_types::{
        CallControl, CallEvent, CallTrack, CallTrackDirection, CallVideoCapabilities,
        CallVideoCodec, CallVideoFormat, CallVideoMode, CallView,
    },
    pcm_envelope::{
        MediaPayloadKind, WireMediaFrame, FLAG_CODEC_CONFIG, FLAG_KEYFRAME, FLAG_ROTATE_90,
        MAX_VIDEO_PAYLOAD_BYTES,
    },
};

const FFMPEG_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CAPTURE_BUFFER_BYTES: usize = MAX_VIDEO_PAYLOAD_BYTES * 2;
const MAX_DECODE_DIMENSION: u32 = 2048;
const MAX_DECODE_ALLOC_BYTES: u64 = 16 * 1024 * 1024;

/// A discovered local camera device (metadata only; never opened during listing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Probed local FFmpeg video codec availability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FfmpegVideoCapabilities {
    pub ffmpeg_path: Option<PathBuf>,
    pub h265_encode: bool,
    pub h264_encode: bool,
    pub jpeg_encode: bool,
}

/// Locate the `ffmpeg` binary via explicit override, `WX_FFMPEG_BIN`, or `PATH`.
pub fn find_ffmpeg(override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(explicit) = override_path {
        if explicit.is_file() {
            return Some(explicit.to_path_buf());
        }
    }
    if let Ok(explicit) = std::env::var("WX_FFMPEG_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let bin_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(not(windows))]
    for fallback in [
        "/usr/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/opt/homebrew/bin/ffmpeg",
    ] {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Probe and cache FFmpeg encoder capabilities for the resolved binary.
pub fn probe_ffmpeg_capabilities(override_path: Option<&Path>) -> FfmpegVideoCapabilities {
    static CACHED: OnceLock<Mutex<BTreeMap<Option<PathBuf>, FfmpegVideoCapabilities>>> =
        OnceLock::new();
    let resolved = find_ffmpeg(override_path);
    let cache = CACHED.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(caps) = guard.get(&resolved) {
            return caps.clone();
        }
    }
    let probed = match &resolved {
        Some(ffmpeg) => {
            let encoders = run_ffmpeg_text(ffmpeg, &["-hide_banner", "-encoders"]);
            FfmpegVideoCapabilities {
                ffmpeg_path: Some(ffmpeg.clone()),
                h265_encode: has_codec(&encoders, "libx265"),
                h264_encode: has_codec(&encoders, "libx264"),
                jpeg_encode: has_codec(&encoders, "mjpeg"),
            }
        }
        None => FfmpegVideoCapabilities::default(),
    };
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= 8 {
            guard.clear();
        }
        guard.insert(resolved, probed.clone());
    }
    probed
}

fn run_ffmpeg_text(ffmpeg: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let Ok(mut child) = cmd.spawn() else {
        return String::new();
    };
    let read_capped = |stream: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(reader) = stream {
                let _ = reader.take(256 * 1024).read_to_end(&mut out);
            }
            out
        })
    };
    let stdout_handle = read_capped(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr_handle = read_capped(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + FFMPEG_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    let mut combined = String::from_utf8_lossy(&stdout).into_owned();
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&stderr));
    }
    combined
}

fn has_codec(listing: &str, name: &str) -> bool {
    listing.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let _flags = parts.next();
        parts.next() == Some(name)
    })
}

/// Configured camera source for outgoing video calls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CameraSource {
    /// Automatically pick the first available local camera when camera is enabled.
    #[default]
    Auto,
    /// Disable local camera capture.
    Off,
    /// Generate a synthetic animated FFmpeg `testsrc2` pattern (for headless/container testing).
    Test,
    /// Loop a local video or image file via FFmpeg.
    File(PathBuf),
    /// Use an explicit camera device path or platform capture name/index.
    Device(String),
}

impl CameraSource {
    pub fn parse(raw: &str) -> Result<Self> {
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
        ensure!(
            !trimmed.starts_with('-'),
            "camera device name cannot start with '-'"
        );
        #[cfg(target_os = "windows")]
        ensure!(
            !trimmed.contains(':'),
            "camera device name cannot contain ':'"
        );
        Ok(Self::Device(trimmed.to_owned()))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Off => "off".into(),
            Self::Test => "test".into(),
            Self::File(path) => format!("file:{}", path.display()),
            Self::Device(device) => device.clone(),
        }
    }

    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off)
    }
}

/// Enumerate local camera devices without opening or capturing any device.
pub fn list_cameras() -> Vec<CameraDevice> {
    list_cameras_with_ffmpeg(None)
}

/// Enumerate local camera devices using an optional explicit `ffmpeg` binary path
/// (used by the desktop app to point at its bundled sidecar).
pub fn list_cameras_with_ffmpeg(override_ffmpeg: Option<&Path>) -> Vec<CameraDevice> {
    #[cfg(target_os = "linux")]
    {
        let _ = override_ffmpeg;
        list_linux_v4l2_cameras()
    }
    #[cfg(target_os = "macos")]
    {
        let Some(ffmpeg) = find_ffmpeg(override_ffmpeg) else {
            return Vec::new();
        };
        let output = run_ffmpeg_text(
            &ffmpeg,
            &[
                "-hide_banner",
                "-f",
                "avfoundation",
                "-list_devices",
                "true",
                "-i",
                "",
            ],
        );
        parse_avfoundation_cameras(&output)
    }
    #[cfg(target_os = "windows")]
    {
        let Some(ffmpeg) = find_ffmpeg(override_ffmpeg) else {
            return Vec::new();
        };
        let output = run_ffmpeg_text(
            &ffmpeg,
            &[
                "-hide_banner",
                "-list_devices",
                "true",
                "-f",
                "dshow",
                "-i",
                "dummy",
            ],
        );
        parse_dshow_cameras(&output)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = override_ffmpeg;
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

/// Parse FFmpeg's `avfoundation -list_devices true` output into video camera entries,
/// skipping audio devices and virtual `Capture screen` sources.
pub fn parse_avfoundation_cameras(output: &str) -> Vec<CameraDevice> {
    let mut in_video = false;
    let mut devices = Vec::new();
    for line in output.lines() {
        if line.contains("AVFoundation video devices:") {
            in_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices:") {
            in_video = false;
            continue;
        }
        if !in_video {
            continue;
        }
        // Format: `[AVFoundation indev @ 0x...] [0] FaceTime HD Camera`
        let Some(after_prefix) = line.split("] [").nth(1) else {
            continue;
        };
        let Some((idx_str, rest)) = after_prefix.split_once(']') else {
            continue;
        };
        if idx_str.trim().parse::<u32>().is_err() {
            continue;
        }
        let name = rest.trim();
        if name.is_empty() || name.to_ascii_lowercase().starts_with("capture screen") {
            continue;
        }
        devices.push(CameraDevice {
            id: idx_str.trim().to_owned(),
            name: name.to_owned(),
            is_default: devices.is_empty(),
        });
    }
    devices
}

/// Parse FFmpeg's Windows `dshow -list_devices true` output into video camera entries.
/// Supports both FFmpeg 5+ inline `"Name" (video)` lines and legacy section headers.
pub fn parse_dshow_cameras(output: &str) -> Vec<CameraDevice> {
    let mut in_legacy_video = false;
    let mut devices = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("DirectShow video devices") {
            in_legacy_video = true;
            continue;
        }
        if trimmed.contains("DirectShow audio devices") {
            in_legacy_video = false;
            continue;
        }
        if trimmed.contains("Alternative name") {
            continue;
        }
        let is_video = if trimmed.ends_with("(video)") {
            true
        } else if trimmed.ends_with("(audio)") || trimmed.ends_with("(none)") {
            false
        } else {
            in_legacy_video
        };
        if !is_video {
            continue;
        }
        let Some(first_quote) = trimmed.find('"') else {
            continue;
        };
        let after_first = &trimmed[first_quote + 1..];
        let Some(second_quote) = after_first.find('"') else {
            continue;
        };
        let name = after_first[..second_quote].trim();
        if name.is_empty() || name.contains(':') {
            continue;
        }
        devices.push(CameraDevice {
            id: name.to_owned(),
            name: name.to_owned(),
            is_default: devices.is_empty(),
        });
    }
    devices
}

/// Compute video capabilities this client should advertise in `call.ready`.
///
/// The client renders received video from standalone JPEG frames and encodes
/// outgoing video via FFmpeg into whichever codec the local FFmpeg installation
/// supports.
pub fn client_video_capabilities(
    daemon_supports_video: bool,
    override_ffmpeg: Option<&Path>,
) -> Option<CallVideoCapabilities> {
    if !daemon_supports_video {
        return None;
    }
    let host = probe_ffmpeg_capabilities(override_ffmpeg);
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

/// One encoded outgoing video frame ready to wrap in `WXCM` v3 (or preview in UI).
#[derive(Clone, Debug)]
pub struct CapturedVideoFrame {
    pub codec: CallVideoCodec,
    pub flags: u16,
    pub timestamp_micros: u64,
    pub bytes: Vec<u8>,
}

/// Latest-frame drop-oldest slot for independent MJPEG frames so slow UI consumers
/// always receive the freshest camera frame without polling jitter or queue lag.
struct LatestJpegSlot {
    state: Mutex<(Option<CapturedVideoFrame>, bool)>,
    notify: Notify,
}

impl LatestJpegSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new((None, false)),
            notify: Notify::new(),
        }
    }

    fn publish(&self, frame: CapturedVideoFrame) {
        if let Ok(mut guard) = self.state.lock() {
            guard.0 = Some(frame);
        }
        self.notify.notify_one();
    }

    fn close(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.1 = true;
        }
        self.notify.notify_waiters();
    }

    fn try_take(&self) -> Option<CapturedVideoFrame> {
        self.state.lock().ok().and_then(|mut guard| guard.0.take())
    }

    async fn recv_latest(&self) -> Option<CapturedVideoFrame> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut guard = self.state.lock().ok()?;
                if let Some(frame) = guard.0.take() {
                    return Some(frame);
                }
                if guard.1 {
                    return None;
                }
            }
            notified.await;
        }
    }
}

struct CaptureWorker {
    child: Child,
    stop: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    frames: mpsc::Receiver<CapturedVideoFrame>,
    latest_jpeg: Option<Arc<LatestJpegSlot>>,
}

/// Live FFmpeg camera capture and encoder subprocess.
///
/// Spawned only while the local camera is enabled in a connected call or an
/// explicit pre-call preview, and terminated immediately when stopped.
pub struct LocalVideoCapture {
    source: CameraSource,
    format: CallVideoFormat,
    ffmpeg_path: PathBuf,
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    frames: mpsc::Receiver<CapturedVideoFrame>,
    latest_jpeg: Option<Arc<LatestJpegSlot>>,
    buffered: VecDeque<CapturedVideoFrame>,
    wait_for_keyframe: bool,
    started_at: Instant,
}

impl LocalVideoCapture {
    pub fn start(source: &CameraSource, format: CallVideoFormat) -> Result<Self> {
        Self::start_with_ffmpeg(source, format, None)
    }

    pub fn start_with_ffmpeg(
        source: &CameraSource,
        format: CallVideoFormat,
        override_ffmpeg: Option<&Path>,
    ) -> Result<Self> {
        ensure!(!source.is_off(), "camera source is set to off");
        let ffmpeg = find_ffmpeg(override_ffmpeg).context(
            "ffmpeg is required for outgoing camera capture; install ffmpeg or pass --camera-off",
        )?;
        let started_at = Instant::now();
        let worker = spawn_capture(&ffmpeg, source, format, started_at)?;
        Ok(Self {
            source: source.clone(),
            format,
            ffmpeg_path: ffmpeg,
            child: Some(worker.child),
            stop: worker.stop,
            reader: Some(worker.reader),
            frames: worker.frames,
            latest_jpeg: worker.latest_jpeg,
            buffered: VecDeque::with_capacity(16),
            wait_for_keyframe: false,
            started_at,
        })
    }

    /// Start a standalone MJPEG camera capture for local pre-call preview.
    pub fn start_preview(
        source: &CameraSource,
        max_width: u16,
        max_height: u16,
        max_fps: u8,
        override_ffmpeg: Option<&Path>,
    ) -> Result<Self> {
        let format = CallVideoFormat {
            codec: CallVideoCodec::Jpeg,
            mode: CallVideoMode::Transcoded,
            max_width,
            max_height,
            max_fps,
            bitrate_kbps: None,
        };
        Self::start_with_ffmpeg(source, format, override_ffmpeg)
    }

    /// Await the freshest available captured frame without fixed-interval polling.
    /// Returns `None` once the capture worker exits.
    pub async fn recv_latest(&mut self) -> Option<CapturedVideoFrame> {
        if let Some(slot) = self.latest_jpeg.clone() {
            return slot.recv_latest().await;
        }
        if let Some(frame) = self.try_recv() {
            return Some(frame);
        }
        let first = self.frames.recv().await?;
        let mut latest = first;
        if self.format.codec == CallVideoCodec::Jpeg {
            while let Ok(frame) = self.frames.try_recv() {
                latest = frame;
            }
            return Some(latest);
        }
        self.buffered.push_back(latest);
        self.try_recv()
    }

    pub fn try_recv(&mut self) -> Option<CapturedVideoFrame> {
        if let Some(slot) = &self.latest_jpeg {
            return slot.try_take();
        }
        if self.format.codec == CallVideoCodec::Jpeg {
            // Standalone JPEG frames are independent; always keep the freshest frame.
            let mut latest = None;
            while let Ok(frame) = self.frames.try_recv() {
                latest = Some(frame);
            }
            return latest;
        }
        // Inter-frame H.265/H.264 must deliver FIFO so IDR keyframes and reference
        // P-frames are never dropped; only skip ahead when a newer self-contained
        // keyframe (IDR + inline parameter sets) is already ready. If overflow occurs,
        // wait for the next keyframe rather than forwarding broken P-frame references.
        while let Ok(frame) = self.frames.try_recv() {
            let is_idr_with_config = frame.flags & (FLAG_KEYFRAME | FLAG_CODEC_CONFIG)
                == (FLAG_KEYFRAME | FLAG_CODEC_CONFIG);
            if is_idr_with_config {
                self.buffered.clear();
                self.wait_for_keyframe = false;
                self.buffered.push_back(frame);
            } else if self.wait_for_keyframe {
                continue;
            } else if self.buffered.len() >= 32 {
                self.wait_for_keyframe = true;
            } else {
                self.buffered.push_back(frame);
            }
        }
        self.buffered.pop_front()
    }

    pub fn request_keyframe(&mut self) -> Result<()> {
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
        let worker = spawn_capture(
            &self.ffmpeg_path,
            &self.source,
            self.format,
            self.started_at,
        )?;
        self.child = Some(worker.child);
        self.stop = worker.stop;
        self.reader = Some(worker.reader);
        self.frames = worker.frames;
        self.latest_jpeg = worker.latest_jpeg;
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

fn resolve_device_input(source: &CameraSource, ffmpeg: &Path) -> Result<String> {
    match source {
        CameraSource::Device(device) => Ok(device.clone()),
        CameraSource::Auto => {
            let cameras = list_cameras_with_ffmpeg(Some(ffmpeg));
            if let Some(first) = cameras.into_iter().next() {
                return Ok(first.id);
            }
            #[cfg(target_os = "macos")]
            {
                bail!("no local camera found; connect a camera, pass --camera test, or pass --camera-off")
            }
            #[cfg(target_os = "linux")]
            {
                bail!("no local camera found; connect a camera, pass --camera test, or pass --camera-off");
            }
            #[cfg(target_os = "windows")]
            {
                bail!("no local camera found; connect a camera, pass --camera DEVICE_NAME, --camera test, or --camera-off")
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
    ffmpeg: &Path,
    source: &CameraSource,
    format: CallVideoFormat,
    started_at: Instant,
) -> Result<CaptureWorker> {
    let width = format.max_width.clamp(160, 640) & !1;
    let height = format.max_height.clamp(120, 480) & !1;
    let max_fps = if format.codec == CallVideoCodec::Jpeg {
        30
    } else {
        15
    };
    let fps = format.max_fps.clamp(1, max_fps);
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
            let canonical = std::fs::canonicalize(path)
                .with_context(|| format!("failed to resolve camera file: {}", path.display()))?;
            let mut input = std::ffi::OsString::from("file:");
            input.push(canonical.as_os_str());
            cmd.args(["-re", "-stream_loop", "-1", "-i"]);
            cmd.arg(input);
        }
        CameraSource::Auto | CameraSource::Device(_) => {
            let device = resolve_device_input(source, ffmpeg)?;
            cmd.args(["-fflags", "nobuffer", "-flags", "low_delay"]);
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
                // Request 30 fps from AVFoundation (omitting `-framerate` falls back to FFmpeg's
                // 29.97 ntsc default, which some Apple Silicon FaceTime/Continuity cameras reject);
                // the output `-vf fps={fps}` filter below enforces the negotiated call rate.
                cmd.args(["-f", "avfoundation", "-framerate", "30", "-i", &device]);
            }
            #[cfg(target_os = "windows")]
            {
                // Let DirectShow negotiate the webcam's native rate; `-vf fps={fps}` throttles output.
                cmd.args(["-f", "dshow", "-i", &format!("video={device}")]);
            }
        }
        CameraSource::Off => bail!("camera source is off"),
    }

    // Pre-rotate outgoing wire H.265/H.264 90° CCW to match mobile/conference WeChat receivers
    // (which apply +90° CW display rotation). Keep JPEG unrotated for local pre-call previews
    // and daemon-side transcoded TX.
    let pre_rotate_ccw = matches!(format.codec, CallVideoCodec::H265 | CallVideoCodec::H264);
    let scale = if pre_rotate_ccw {
        format!(
            "fps={fps},scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2,transpose=2"
        )
    } else {
        format!(
            "fps={fps},scale=w='min(iw,{width})':h='min(ih,{height})':force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2"
        )
    };
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

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
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
    let codec = format.codec;
    let latest_jpeg = (codec == CallVideoCodec::Jpeg).then(|| Arc::new(LatestJpegSlot::new()));
    let worker_jpeg = latest_jpeg.clone();
    let channel_cap = if codec == CallVideoCodec::Jpeg { 1 } else { 32 };
    let (tx, rx) = mpsc::channel(channel_cap);
    let reader = match thread::Builder::new()
        .name("wx-call-video-cap".into())
        .spawn(move || {
            let rotation_flags = if pre_rotate_ccw { FLAG_ROTATE_90 } else { 0 };
            read_captured_stream(
                stdout,
                codec,
                rotation_flags,
                worker_stop,
                started_at,
                tx,
                worker_jpeg,
            )
        }) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("failed to spawn video capture reader");
        }
    };
    Ok(CaptureWorker {
        child,
        stop,
        reader,
        frames: rx,
        latest_jpeg,
    })
}

fn read_captured_stream(
    mut stdout: impl Read,
    codec: CallVideoCodec,
    rotation_flags: u16,
    stop: Arc<AtomicBool>,
    started_at: Instant,
    tx: mpsc::Sender<CapturedVideoFrame>,
    latest_jpeg: Option<Arc<LatestJpegSlot>>,
) {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 8192];
    let mut pending_au = Vec::<u8>::new();
    let mut pending_has_slice = false;
    let mut pending_is_keyframe = false;
    let mut pending_has_config = false;
    let mut wait_for_keyframe = false;

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
                    if frame.len() <= MAX_VIDEO_PAYLOAD_BYTES {
                        let captured = CapturedVideoFrame {
                            codec: CallVideoCodec::Jpeg,
                            flags: FLAG_KEYFRAME,
                            timestamp_micros: ts,
                            bytes: frame,
                        };
                        if let Some(slot) = &latest_jpeg {
                            slot.publish(captured);
                        } else {
                            let _ = tx.try_send(captured);
                        }
                    }
                }
            }
            CallVideoCodec::H265 | CallVideoCodec::H264 => {
                drain_annexb_frames(
                    &mut buf,
                    codec,
                    rotation_flags,
                    &mut pending_au,
                    &mut pending_has_slice,
                    &mut pending_is_keyframe,
                    &mut pending_has_config,
                    &mut wait_for_keyframe,
                    ts,
                    &tx,
                );
            }
        }
    }
    if let Some(slot) = &latest_jpeg {
        slot.close();
    }
}

/// Extract one complete JPEG image (`SOI .. EOI`), skipping length-prefixed header
/// segments (`APPn`, `DQT`, `DHT`, `COM`, etc.) before `SOS` so embedded thumbnails
/// containing `0xff 0xd9` never split a frame prematurely.
pub fn extract_jpeg_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    loop {
        let Some(start) = buf.windows(2).position(|w| w == [0xff, 0xd8]) else {
            // Retain only a trailing 0xff byte in case SOI straddles a read boundary.
            if buf.len() > 1 {
                let keep = usize::from(*buf.last().unwrap_or(&0) == 0xff);
                let drop_len = buf.len() - keep;
                buf.drain(..drop_len);
            }
            return None;
        };
        if start > 0 {
            buf.drain(..start);
        }
        // Walk marker segments starting after SOI (index 2).
        let mut pos = 2usize;
        let mut in_scan = false;
        let mut frame_end = None;
        let mut invalid = false;

        while pos < buf.len() {
            if !in_scan {
                if buf[pos] != 0xff {
                    invalid = true;
                    break;
                }
                while pos < buf.len() && buf[pos] == 0xff {
                    pos += 1;
                }
                if pos >= buf.len() {
                    break;
                }
                let marker = buf[pos];
                pos += 1;
                if marker == 0xd9 {
                    frame_end = Some(pos);
                    break;
                }
                if marker == 0x00 || marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
                    continue;
                }
                if pos + 2 > buf.len() {
                    break;
                }
                let seg_len = usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]));
                if seg_len < 2 {
                    invalid = true;
                    break;
                }
                if pos + seg_len > buf.len() {
                    break;
                }
                pos += seg_len;
                if marker == 0xda {
                    in_scan = true;
                }
            } else {
                // Inside entropy-coded scan: 0xff 0x00 is byte-stuffed, 0xff 0xd0..0xd7 are RSTn.
                let Some(rel) = buf[pos..].iter().position(|&b| b == 0xff) else {
                    break;
                };
                pos += rel;
                while pos < buf.len() && buf[pos] == 0xff {
                    pos += 1;
                }
                if pos >= buf.len() {
                    break;
                }
                let marker = buf[pos];
                pos += 1;
                if marker == 0x00 || (0xd0..=0xd7).contains(&marker) {
                    continue;
                }
                if marker == 0xd9 {
                    frame_end = Some(pos);
                    break;
                }
                // Another marker ended the scan (e.g. multi-scan progressive JPEG); resume marker parsing.
                in_scan = false;
                if marker != 0x01 && marker != 0xd8 {
                    if pos + 2 > buf.len() {
                        break;
                    }
                    let seg_len = usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]));
                    if seg_len < 2 {
                        invalid = true;
                        break;
                    }
                    if pos + seg_len > buf.len() {
                        break;
                    }
                    pos += seg_len;
                    if marker == 0xda {
                        in_scan = true;
                    }
                }
            }
        }

        if let Some(end) = frame_end {
            return Some(buf.drain(..end).collect());
        }
        if invalid {
            buf.drain(..2);
            continue;
        }
        if buf.len() > MAX_CAPTURE_BUFFER_BYTES {
            buf.clear();
        }
        return None;
    }
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
    rotation_flags: u16,
    pending_au: &mut Vec<u8>,
    pending_has_slice: &mut bool,
    pending_is_keyframe: &mut bool,
    pending_has_config: &mut bool,
    wait_for_keyframe: &mut bool,
    timestamp_micros: u64,
    tx: &mpsc::Sender<CapturedVideoFrame>,
) {
    let nalus = split_start_codes(buf);
    if nalus.len() < 2 {
        if buf.len() > MAX_CAPTURE_BUFFER_BYTES {
            buf.clear();
            pending_au.clear();
            *pending_has_slice = false;
            *pending_is_keyframe = false;
            *pending_has_config = false;
            *wait_for_keyframe = true;
        }
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
            let mut flags = rotation_flags;
            if *pending_is_keyframe {
                flags |= FLAG_KEYFRAME;
            }
            if *pending_has_config {
                flags |= FLAG_CODEC_CONFIG;
            }
            let bytes = std::mem::take(pending_au);
            *pending_has_slice = false;
            *pending_is_keyframe = false;
            *pending_has_config = false;

            let is_idr_with_config =
                flags & (FLAG_KEYFRAME | FLAG_CODEC_CONFIG) == (FLAG_KEYFRAME | FLAG_CODEC_CONFIG);
            if bytes.len() > MAX_VIDEO_PAYLOAD_BYTES {
                *wait_for_keyframe = true;
            } else if !*wait_for_keyframe || is_idr_with_config {
                match tx.try_send(CapturedVideoFrame {
                    codec,
                    flags,
                    timestamp_micros,
                    bytes,
                }) {
                    Ok(()) => {
                        *wait_for_keyframe = false;
                    }
                    Err(_) => {
                        *wait_for_keyframe = true;
                    }
                }
            }
        }
        if pending_au.len().saturating_add(nalu.len()) > MAX_VIDEO_PAYLOAD_BYTES {
            pending_au.clear();
            *pending_has_slice = false;
            *pending_is_keyframe = false;
            *pending_has_config = false;
            *wait_for_keyframe = true;
            continue;
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

/// Decoded RGB24 frame for clients that render raw pixels (such as the CLI).
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Raw JPEG frame with display rotation metadata for zero-re-encode WebView rendering.
#[derive(Clone, Debug)]
pub struct RawVideoFrame {
    pub track_id: u32,
    pub generation: u32,
    pub participant_id: Option<String>,
    pub rotation_degrees: u16,
    pub jpeg: Vec<u8>,
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
pub struct DecodeMailbox {
    state: Mutex<DecodeMailboxState>,
    ready: Condvar,
}

impl Default for DecodeMailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl DecodeMailbox {
    pub fn new() -> Self {
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

    pub fn close(&self) {
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

/// Active remote video stream metadata and latest decoded frame (when RGB decoding is enabled).
pub struct RemoteVideoTrack {
    pub track: CallTrack,
    pub label: String,
    pub active: bool,
    pub frame: Option<DecodedFrame>,
    pub frames_received: u64,
}

fn clean_peer_label(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

/// Tracks active remote video streams with generation and retirement fencing.
///
/// Construct with [`ReceiveVideo::new`] to decode incoming JPEGs into rotated
/// RGB24 frames on a background thread (for terminal rendering), or
/// [`ReceiveVideo::new_passthrough`] to forward validated raw JPEG frames and
/// rotation degrees straight to a WebView without CPU decoding.
pub struct ReceiveVideo {
    tracks: BTreeMap<u32, RemoteVideoTrack>,
    retired: BTreeMap<u32, u32>,
    peer_label: Option<String>,
    peer_camera_off: bool,
    decode_rgb: bool,
    mailbox: Arc<DecodeMailbox>,
    decoded_rx: Option<std::sync::mpsc::Receiver<DecodedJob>>,
    decoder: Option<JoinHandle<()>>,
    raw_pending: BTreeMap<u32, RawVideoFrame>,
    raw_order: VecDeque<u32>,
}

impl ReceiveVideo {
    pub fn new(tracks: Vec<CallTrack>) -> Self {
        let mailbox = Arc::new(DecodeMailbox::new());
        let worker_mailbox = mailbox.clone();
        let (decoded_tx, decoded_rx) = std::sync::mpsc::sync_channel::<DecodedJob>(8);
        let decoder = thread::Builder::new()
            .name("wx-call-video-dec".into())
            .spawn(move || {
                while let Some(job) = worker_mailbox.pop_blocking() {
                    let mut reader = image::ImageReader::with_format(
                        std::io::Cursor::new(&job.jpeg),
                        image::ImageFormat::Jpeg,
                    );
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
                    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
                    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
                    reader.limits(limits);
                    let Ok(img) = reader.decode() else {
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
            decode_rgb: true,
            mailbox,
            decoded_rx: Some(decoded_rx),
            decoder,
            raw_pending: BTreeMap::new(),
            raw_order: VecDeque::new(),
        };
        for track in tracks {
            if track.is_video() && track.direction() == CallTrackDirection::Receive {
                value.upsert_track(track);
            }
        }
        value
    }

    pub fn new_passthrough(tracks: Vec<CallTrack>) -> Self {
        let mut value = Self {
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            peer_label: None,
            peer_camera_off: false,
            decode_rgb: false,
            mailbox: Arc::new(DecodeMailbox::new()),
            decoded_rx: None,
            decoder: None,
            raw_pending: BTreeMap::new(),
            raw_order: VecDeque::new(),
        };
        for track in tracks {
            if track.is_video() && track.direction() == CallTrackDirection::Receive {
                value.upsert_track(track);
            }
        }
        value
    }

    pub fn upsert_track(&mut self, track: CallTrack) {
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
                self.raw_pending.remove(&track_id);
                self.raw_order.retain(|id| *id != track_id);
            }
            existing.track = track;
            existing.active = active;
            return;
        }
        let label = track
            .participant_id()
            .map(|id| clean_peer_label(id, 32))
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

    pub fn event(&mut self, event: &CallEvent, call_id: &str) -> bool {
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
                    self.raw_pending.remove(track_id);
                    self.raw_order.retain(|id| id != track_id);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    pub fn roster(&mut self, call: &CallView) {
        if let Some(control) = call.controls.iter().find_map(|entry| match entry.control {
            CallControl::Camera { enabled } => Some((&entry.participant_id, enabled)),
            _ => None,
        }) {
            self.peer_label = Some(clean_peer_label(control.0, 32));
            self.peer_camera_off = !control.1;
        } else if let Some(first) = call.controls.first() {
            self.peer_label = Some(clean_peer_label(&first.participant_id, 32));
        }

        for entry in self.tracks.values_mut() {
            if let Some(pid) = entry.track.participant_id() {
                if let Some(participant) =
                    call.participants.iter().find(|p| p.participant_id == pid)
                {
                    entry.label = clean_peer_label(&participant.contact_id, 32);
                    if let Some(cam) = participant.camera_enabled {
                        entry.active = cam;
                    }
                }
            } else if let Some(peer) = &self.peer_label {
                entry.label = peer.clone();
            }
        }
    }

    pub fn set_peer_label(&mut self, label: &str) {
        let cleaned = clean_peer_label(label, 32);
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

    /// Non-blocking handoff of an incoming JPEG frame.
    pub fn binary_frame(&mut self, wire: &WireMediaFrame<'_>) -> bool {
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
        let Some(entry) = self.tracks.get_mut(&track_id) else {
            return false;
        };
        if generation != entry.track.generation() {
            return false;
        }
        if self.decode_rgb {
            return self.mailbox.push(DecodeJob {
                track_id,
                generation,
                rotation_degrees: wire.rotation_degrees(),
                jpeg: wire.payload().to_vec(),
            });
        }
        entry.active = true;
        entry.frames_received = entry.frames_received.saturating_add(1);
        let participant_id = entry.track.participant_id().map(str::to_owned);
        if participant_id.is_none() {
            self.peer_camera_off = false;
        }
        let frame = RawVideoFrame {
            track_id,
            generation,
            participant_id,
            rotation_degrees: wire.rotation_degrees(),
            jpeg: wire.payload().to_vec(),
        };
        if self.raw_pending.insert(track_id, frame).is_none() {
            self.raw_order.push_back(track_id);
        }
        true
    }

    /// Drain frames completed by the background JPEG decoder (RGB mode).
    pub fn poll_decoded(&mut self) -> bool {
        let Some(rx) = &self.decoded_rx else {
            return false;
        };
        let mut updated = false;
        while let Ok(done) = rx.try_recv() {
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

    /// Drain pending latest-per-track raw JPEG frames (passthrough mode).
    pub fn drain_raw_frames(&mut self) -> Vec<RawVideoFrame> {
        let mut frames = Vec::with_capacity(self.raw_pending.len());
        while let Some(track_id) = self.raw_order.pop_front() {
            if let Some(frame) = self.raw_pending.remove(&track_id) {
                frames.push(frame);
            }
        }
        frames
    }

    pub fn tracks(&self) -> &BTreeMap<u32, RemoteVideoTrack> {
        &self.tracks
    }

    pub fn peer_camera_off(&self) -> bool {
        self.peer_camera_off
    }

    pub fn has_visible_video(&self) -> bool {
        !self.tracks.is_empty()
    }

    pub fn active_track_count(&self) -> usize {
        self.tracks.values().filter(|entry| entry.active).count()
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

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_call_contracts::pcm_envelope::{encode_video, MediaHeader, FLAG_ROTATE_90};

    #[test]
    fn parses_macos_avfoundation_cameras_and_skips_screens_and_audio() {
        let sample = "\
[AVFoundation indev @ 0x1234] AVFoundation video devices:
[AVFoundation indev @ 0x1234] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1234] [1] Desk View Camera
[AVFoundation indev @ 0x1234] [2] Capture screen 0
[AVFoundation indev @ 0x1234] AVFoundation audio devices:
[AVFoundation indev @ 0x1234] [0] MacBook Pro Microphone
";
        let cameras = parse_avfoundation_cameras(sample);
        assert_eq!(
            cameras,
            vec![
                CameraDevice {
                    id: "0".into(),
                    name: "FaceTime HD Camera".into(),
                    is_default: true,
                },
                CameraDevice {
                    id: "1".into(),
                    name: "Desk View Camera".into(),
                    is_default: false,
                },
            ]
        );
    }

    #[test]
    fn parses_windows_dshow_modern_and_legacy_video_devices() {
        let modern = "\
[dshow @ 0x1234] \"Integrated Webcam\" (video)
[dshow @ 0x1234]   Alternative name \"@device_pnp_\\\\?\\usb#vid_046d\"
[dshow @ 0x1234] \"USB Capture Cam\" (video)
[dshow @ 0x1234] \"Microphone Array\" (audio)
";
        assert_eq!(
            parse_dshow_cameras(modern),
            vec![
                CameraDevice {
                    id: "Integrated Webcam".into(),
                    name: "Integrated Webcam".into(),
                    is_default: true,
                },
                CameraDevice {
                    id: "USB Capture Cam".into(),
                    name: "USB Capture Cam".into(),
                    is_default: false,
                },
            ]
        );

        let legacy = "\
[dshow @ 0x1234] DirectShow video devices (some may be both video and audio devices)
[dshow @ 0x1234]  \"HD Pro Webcam C920\"
[dshow @ 0x1234]     Alternative name \"@device_pnp_xyz\"
[dshow @ 0x1234] DirectShow audio devices
[dshow @ 0x1234]  \"Headset Microphone\"
";
        assert_eq!(
            parse_dshow_cameras(legacy),
            vec![CameraDevice {
                id: "HD Pro Webcam C920".into(),
                name: "HD Pro Webcam C920".into(),
                is_default: true,
            }]
        );
    }

    #[test]
    fn passthrough_receive_video_keeps_latest_jpeg_per_track_with_rotation() {
        let track = CallTrack::Video {
            id: 65_538,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallVideoFormat {
                codec: CallVideoCodec::Jpeg,
                mode: CallVideoMode::Transcoded,
                max_width: 640,
                max_height: 480,
                max_fps: 15,
                bitrate_kbps: None,
            },
            participant_id: Some("peer-1".into()),
            active: true,
        };
        let mut rx = ReceiveVideo::new_passthrough(vec![track]);
        let f1 = encode_video(
            MediaPayloadKind::Jpeg,
            FLAG_KEYFRAME,
            MediaHeader {
                track_id: 65_538,
                generation: 1,
                sequence: 0,
                timestamp_micros: 0,
            },
            &[0xff, 0xd8, 0x01, 0xff, 0xd9],
        )
        .unwrap();
        let f2 = encode_video(
            MediaPayloadKind::Jpeg,
            FLAG_KEYFRAME | FLAG_ROTATE_90,
            MediaHeader {
                track_id: 65_538,
                generation: 1,
                sequence: 1,
                timestamp_micros: 66_000,
            },
            &[0xff, 0xd8, 0x02, 0xff, 0xd9],
        )
        .unwrap();
        assert!(rx.binary_frame(&WireMediaFrame::decode(&f1).unwrap()));
        assert!(rx.binary_frame(&WireMediaFrame::decode(&f2).unwrap()));
        let drained = rx.drain_raw_frames();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].rotation_degrees, 90);
        assert_eq!(drained[0].jpeg, vec![0xff, 0xd8, 0x02, 0xff, 0xd9]);
        assert_eq!(drained[0].participant_id.as_deref(), Some("peer-1"));
    }

    #[test]
    fn extract_jpeg_frame_ignores_embedded_eoi_in_app1_and_bounds_garbage() {
        let mut garbage = vec![0x11u8; 4096];
        garbage.push(0xff);
        assert!(extract_jpeg_frame(&mut garbage).is_none());
        assert_eq!(garbage, vec![0xff]);

        // JPEG with APP1 segment containing an embedded thumbnail EOI (0xff 0xd9),
        // followed by SOS (0xff 0xda) with byte-stuffed 0xff 0x00 and the real EOI.
        let mut frame = vec![
            0xff, 0xd8, // SOI
            0xff, 0xe1, 0x00, 0x08, // APP1, length 8 (2 len + 6 payload)
            0xff, 0xd8, 0x12, 0x34, 0xff, 0xd9, // embedded thumbnail with EOI
            0xff, 0xda, 0x00, 0x04, 0x01, 0x02, // SOS header (length 4)
            0xaa, 0xff, 0x00, 0xbb, // scan data with stuffed 0xff 0x00
            0xff, 0xd9, // real EOI
        ];
        let expected = frame.clone();
        frame.extend_from_slice(&[0xff, 0xd8, 0x01]); // trailing start of next frame
        let extracted = extract_jpeg_frame(&mut frame).unwrap();
        assert_eq!(extracted, expected);
        assert_eq!(frame, vec![0xff, 0xd8, 0x01]);
    }

    #[test]
    fn drain_annexb_waits_for_idr_with_config_after_channel_backpressure() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut buf = Vec::new();
        let mut pending_au = Vec::new();
        let mut has_slice = false;
        let mut is_kf = false;
        let mut has_cfg = false;
        let mut wait_for_kf = false;

        // Frame 1: SPS (7) + IDR (5)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88]);
        // Frame 2: P-frame (1) -> fills channel (cap 1) after Frame 1
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x01]);
        // Frame 3: P-frame (1) -> triggers backpressure drop & sets wait_for_keyframe
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x02]);
        // Frame 4: P-frame (1) -> must be dropped while waiting for keyframe
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x03]);
        // Trailing start code to flush Frame 3
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x04]);

        drain_annexb_frames(
            &mut buf,
            CallVideoCodec::H264,
            FLAG_ROTATE_90,
            &mut pending_au,
            &mut has_slice,
            &mut is_kf,
            &mut has_cfg,
            &mut wait_for_kf,
            100,
            &tx,
        );
        assert!(wait_for_kf);
        let first = rx.try_recv().unwrap();
        assert_eq!(
            first.flags,
            FLAG_KEYFRAME | FLAG_CODEC_CONFIG | FLAG_ROTATE_90
        );
        assert!(rx.try_recv().is_err());

        // Next P-frame must still be dropped even now that channel has room.
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x05]);
        drain_annexb_frames(
            &mut buf,
            CallVideoCodec::H264,
            FLAG_ROTATE_90,
            &mut pending_au,
            &mut has_slice,
            &mut is_kf,
            &mut has_cfg,
            &mut wait_for_kf,
            200,
            &tx,
        );
        assert!(wait_for_kf);
        assert!(rx.try_recv().is_err());

        // New SPS + IDR followed by next slice + trailing boundary flushes SPS+IDR and resumes streaming.
        buf.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x99]);
        buf.extend_from_slice(&[0, 0, 0, 1, 0x41, 0x06, 0, 0, 0, 1, 0x41, 0x07]);
        drain_annexb_frames(
            &mut buf,
            CallVideoCodec::H264,
            FLAG_ROTATE_90,
            &mut pending_au,
            &mut has_slice,
            &mut is_kf,
            &mut has_cfg,
            &mut wait_for_kf,
            300,
            &tx,
        );
        assert!(!wait_for_kf);
        let resumed = rx.try_recv().unwrap();
        assert_eq!(
            resumed.flags,
            FLAG_KEYFRAME | FLAG_CODEC_CONFIG | FLAG_ROTATE_90
        );
        assert!(resumed.bytes.ends_with(&[0x65, 0x99]));
    }
}
