//! Low-latency FFmpeg video codec probing and transcoding bridge for call media.
//!
//! Pass-through tracks whose client codec matches WeChat's active wire codec do
//! not spawn FFmpeg. Transcoders run only when the local client and remote wire
//! codec differ (for example H.264 or JPEG bridged to WeChat H.265/HEVC).

use anyhow::{ensure, Context, Result};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        mpsc::{self, Receiver},
        OnceLock,
    },
    thread::{self, JoinHandle},
};
use wechat_protocol::calls::video_packet::{split_annexb_nalus, VideoRotation, VideoWireCodec};

/// Video codec exchanged on a call media track or WeChat wire stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// Standard Annex-B H.265 / HEVC.
    H265,
    /// Standard Annex-B H.264 / AVC.
    H264,
    /// Standalone JPEG still frames.
    Jpeg,
}

impl VideoCodec {
    /// Convert a standard codec to its WeChat wire codec enum, if directly representable.
    pub const fn to_wire(self) -> Option<VideoWireCodec> {
        match self {
            Self::H265 => Some(VideoWireCodec::H265),
            Self::H264 => Some(VideoWireCodec::H264),
            Self::Jpeg => None,
        }
    }

    /// Convert a WeChat wire codec byte to a standard [`VideoCodec`], if supported.
    pub const fn from_wire(wire: VideoWireCodec) -> Option<Self> {
        match wire {
            VideoWireCodec::H265 => Some(Self::H265),
            VideoWireCodec::H264 => Some(Self::H264),
            _ => None,
        }
    }
}

/// Probed local FFmpeg video codec availability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoCodecCapabilities {
    /// Path to the discovered `ffmpeg` executable, if present.
    pub ffmpeg_path: Option<PathBuf>,
    /// Whether H.265/HEVC software encoding (`libx265`) is available.
    pub h265_encode: bool,
    /// Whether H.265/HEVC decoding (`hevc`) is available.
    pub h265_decode: bool,
    /// Whether H.264/AVC software encoding (`libx264`) is available.
    pub h264_encode: bool,
    /// Whether H.264/AVC decoding (`h264`) is available.
    pub h264_decode: bool,
    /// Whether MJPEG encoding (`mjpeg`) is available.
    pub jpeg_encode: bool,
    /// Whether MJPEG decoding (`mjpeg`) is available.
    pub jpeg_decode: bool,
}

impl VideoCodecCapabilities {
    /// Return whether the local FFmpeg installation can transcode `from -> to`.
    pub fn can_transcode(&self, from: VideoCodec, to: VideoCodec) -> bool {
        if from == to {
            return true;
        }
        let dec = match from {
            VideoCodec::H265 => self.h265_decode,
            VideoCodec::H264 => self.h264_decode,
            VideoCodec::Jpeg => self.jpeg_decode,
        };
        let enc = match to {
            VideoCodec::H265 => self.h265_encode,
            VideoCodec::H264 => self.h264_encode,
            VideoCodec::Jpeg => self.jpeg_encode,
        };
        self.ffmpeg_path.is_some() && dec && enc
    }

    /// Return the codecs this daemon can emit to a receive client given an active wire codec.
    pub fn supported_receive_codecs(&self) -> Vec<VideoCodec> {
        let mut out = vec![VideoCodec::H265, VideoCodec::H264];
        if self.ffmpeg_path.is_some() && self.jpeg_encode {
            out.push(VideoCodec::Jpeg);
        }
        out
    }

    /// Return the codecs this daemon can accept from a sending client.
    pub fn supported_send_codecs(&self) -> Vec<VideoCodec> {
        let mut out = vec![VideoCodec::H265, VideoCodec::H264];
        if self.ffmpeg_path.is_some() && self.jpeg_decode && self.h265_encode {
            out.push(VideoCodec::Jpeg);
        }
        out
    }
}

/// Locate the `ffmpeg` binary via `WX_FFMPEG_BIN` or `PATH`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("WX_FFMPEG_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("ffmpeg");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for fallback in ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Probe and cache local FFmpeg encoder/decoder capabilities.
pub fn probe_video_capabilities() -> VideoCodecCapabilities {
    static CACHED: OnceLock<VideoCodecCapabilities> = OnceLock::new();
    if std::env::var_os("WX_FFMPEG_BIN").is_some() {
        return probe_uncached();
    }
    CACHED.get_or_init(probe_uncached).clone()
}

fn probe_uncached() -> VideoCodecCapabilities {
    let Some(ffmpeg) = find_ffmpeg() else {
        return VideoCodecCapabilities::default();
    };
    let encoders = run_list(&ffmpeg, "-encoders");
    let decoders = run_list(&ffmpeg, "-decoders");
    VideoCodecCapabilities {
        ffmpeg_path: Some(ffmpeg),
        h265_encode: has_codec(&encoders, "libx265"),
        h265_decode: has_codec(&decoders, "hevc"),
        h264_encode: has_codec(&encoders, "libx264"),
        h264_decode: has_codec(&decoders, "h264"),
        jpeg_encode: has_codec(&encoders, "mjpeg"),
        jpeg_decode: has_codec(&decoders, "mjpeg"),
    }
}

fn run_list(ffmpeg: &Path, flag: &str) -> String {
    Command::new(ffmpeg)
        .args(["-hide_banner", flag])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

fn has_codec(listing: &str, name: &str) -> bool {
    listing.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let _flags = parts.next();
        parts.next() == Some(name)
    })
}

/// Configuration for a streaming [`VideoTranscoder`] worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscodeConfig {
    /// Input codec written to the transcoder.
    pub from: VideoCodec,
    /// Output codec read from the transcoder.
    pub to: VideoCodec,
    /// Target output maximum width in pixels (`64..=1920`).
    pub width: u16,
    /// Target output maximum height in pixels (`64..=1080`).
    pub height: u16,
    /// Target frame rate (`1..=60`).
    pub fps: u8,
    /// Target bitrate in kbps (`64..=8000`) for H.264/H.265 outputs.
    pub bitrate_kbps: u32,
    /// Clockwise display rotation applied before scaling.
    pub rotation: VideoRotation,
}

/// One complete encoded frame produced by a [`VideoTranscoder`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedVideoFrame {
    /// Codec of `bytes`.
    pub codec: VideoCodec,
    /// True for keyframes (IDR slices or standalone JPEG frames).
    pub is_keyframe: bool,
    /// True when inline parameter sets (VPS/SPS/PPS) precede the frame.
    pub has_codec_config: bool,
    /// Raw Annex-B access unit or JPEG bytes.
    pub bytes: Vec<u8>,
}

/// Streaming low-latency FFmpeg subprocess transcoder.
///
/// Dropping this handle cleanly closes stdin, terminates the child process and
/// joins the reader thread.
pub struct VideoTranscoder {
    ffmpeg: PathBuf,
    config: TranscodeConfig,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Receiver<EncodedVideoFrame>,
    reader: Option<JoinHandle<()>>,
}

impl VideoTranscoder {
    /// Start a streaming transcoder for `config`.
    pub fn start(config: TranscodeConfig) -> Result<Self> {
        ensure!(
            config.from != config.to,
            "Pass-through codecs must not spawn a transcoder"
        );
        ensure!(
            (64..=1920).contains(&config.width)
                && (64..=1080).contains(&config.height)
                && (1..=60).contains(&config.fps)
                && (64..=8000).contains(&config.bitrate_kbps),
            "Invalid video transcode configuration"
        );
        let caps = probe_video_capabilities();
        ensure!(
            caps.can_transcode(config.from, config.to),
            "Required FFmpeg video codec unavailable"
        );
        let ffmpeg = caps
            .ffmpeg_path
            .context("FFmpeg binary unavailable for video transcoding")?;
        let (child, stdin, rx, reader) = spawn_worker(&ffmpeg, config)?;
        Ok(Self {
            ffmpeg,
            config,
            child: Some(child),
            stdin: Some(stdin),
            rx,
            reader: Some(reader),
        })
    }

    /// Push one complete input frame (Annex-B access unit or JPEG) into the transcoder.
    pub fn push(&mut self, input: &[u8]) -> Result<()> {
        ensure!(!input.is_empty(), "Empty video transcode input");
        let stdin = self
            .stdin
            .as_mut()
            .context("Video transcoder input closed")?;
        stdin.write_all(input)?;
        stdin.flush()?;
        Ok(())
    }

    /// Close stdin so FFmpeg flushes any buffered trailing output frames (used for batch/test teardown).
    pub fn finish_input(&mut self) {
        self.stdin.take();
    }

    /// Non-blocking poll for the next completed output frame.
    pub fn try_recv(&mut self) -> Option<EncodedVideoFrame> {
        self.rx.try_recv().ok()
    }

    /// Blocking receive for the next completed output frame until the stream closes.
    pub fn recv(&mut self) -> Option<EncodedVideoFrame> {
        self.rx.recv().ok()
    }

    /// Restart the underlying encoder so the very next pushed frame emits a fresh keyframe
    /// with inline parameter sets (`VPS`/`SPS`/`PPS` + `IDR`).
    pub fn request_keyframe(&mut self) -> Result<()> {
        self.shutdown_worker();
        let (child, stdin, rx, reader) = spawn_worker(&self.ffmpeg, self.config)?;
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.rx = rx;
        self.reader = Some(reader);
        Ok(())
    }

    fn shutdown_worker(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for VideoTranscoder {
    fn drop(&mut self) {
        self.shutdown_worker();
    }
}

fn spawn_worker(
    ffmpeg: &Path,
    config: TranscodeConfig,
) -> Result<(
    Child,
    ChildStdin,
    Receiver<EncodedVideoFrame>,
    JoinHandle<()>,
)> {
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
    ]);
    match config.from {
        VideoCodec::H265 => {
            cmd.args(["-threads", "1", "-f", "hevc", "-i", "pipe:0"]);
        }
        VideoCodec::H264 => {
            cmd.args(["-threads", "1", "-f", "h264", "-i", "pipe:0"]);
        }
        VideoCodec::Jpeg => {
            cmd.args([
                "-framerate",
                &config.fps.to_string(),
                "-f",
                "mjpeg",
                "-i",
                "pipe:0",
            ]);
        }
    }

    let rotate_prefix = match config.rotation {
        VideoRotation::Deg0 => "",
        VideoRotation::Deg90 => "transpose=1,",
        VideoRotation::Deg180 => "hflip,vflip,",
        VideoRotation::Deg270 => "transpose=2,",
    };
    let scale = format!(
        "{rotate_prefix}scale=w='min(iw,{})':h='min(ih,{})':force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2",
        config.width, config.height
    );
    cmd.args(["-vf", &scale, "-an", "-sn", "-dn"]);

    let gop = u32::from(config.fps).max(1);
    let bitrate = format!("{}k", config.bitrate_kbps);
    let maxrate = format!("{}k", config.bitrate_kbps.saturating_mul(3) / 2);
    let bufsize = format!("{}k", config.bitrate_kbps);
    match config.to {
        VideoCodec::H265 => {
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
        VideoCodec::H264 => {
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
        VideoCodec::Jpeg => {
            cmd.args([
                "-c:v",
                "mjpeg",
                "-q:v",
                "6",
                "-flush_packets",
                "1",
                "-f",
                "mjpeg",
                "pipe:1",
            ]);
        }
    }

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to spawn FFmpeg video transcoder")?;
    let stdin = child
        .stdin
        .take()
        .context("Missing FFmpeg transcoder stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("Missing FFmpeg transcoder stdout")?;
    let (tx, rx) = mpsc::sync_channel(16);
    let out_codec = config.to;
    let reader = thread::Builder::new()
        .name("wechat-video-transcode".into())
        .spawn(move || read_output_stream(stdout, out_codec, tx))
        .context("Failed to spawn FFmpeg stdout reader")?;
    Ok((child, stdin, rx, reader))
}

fn read_output_stream(
    mut stdout: impl Read,
    codec: VideoCodec,
    tx: mpsc::SyncSender<EncodedVideoFrame>,
) {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 8192];
    let mut pending_au = Vec::<u8>::new();
    let mut pending_has_slice = false;
    let mut pending_is_keyframe = false;
    let mut pending_has_config = false;

    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        match codec {
            VideoCodec::Jpeg => {
                while let Some(frame) = extract_jpeg_frame(&mut buf) {
                    let _ = tx.try_send(EncodedVideoFrame {
                        codec: VideoCodec::Jpeg,
                        is_keyframe: true,
                        has_codec_config: false,
                        bytes: frame,
                    });
                }
            }
            VideoCodec::H265 | VideoCodec::H264 => {
                drain_annexb_nalus(
                    &mut buf,
                    codec,
                    false,
                    &mut pending_au,
                    &mut pending_has_slice,
                    &mut pending_is_keyframe,
                    &mut pending_has_config,
                    &tx,
                );
            }
        }
    }

    if matches!(codec, VideoCodec::H265 | VideoCodec::H264) {
        drain_annexb_nalus(
            &mut buf,
            codec,
            true,
            &mut pending_au,
            &mut pending_has_slice,
            &mut pending_is_keyframe,
            &mut pending_has_config,
            &tx,
        );
        if !pending_au.is_empty() && pending_has_slice {
            let _ = tx.send(EncodedVideoFrame {
                codec,
                is_keyframe: pending_is_keyframe,
                has_codec_config: pending_has_config,
                bytes: std::mem::take(&mut pending_au),
            });
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

#[allow(clippy::too_many_arguments)]
fn drain_annexb_nalus(
    buf: &mut Vec<u8>,
    codec: VideoCodec,
    flush_all: bool,
    pending_au: &mut Vec<u8>,
    pending_has_slice: &mut bool,
    pending_is_keyframe: &mut bool,
    pending_has_config: &mut bool,
    tx: &mpsc::SyncSender<EncodedVideoFrame>,
) {
    let nalus = split_annexb_nalus(buf);
    if nalus.is_empty() {
        return;
    }
    let complete_count = if flush_all {
        nalus.len()
    } else {
        nalus.len().saturating_sub(1)
    };
    if complete_count == 0 {
        return;
    }
    let mut consumed = 0usize;
    for &(start, end, sc_len) in &nalus[..complete_count] {
        let nalu = &buf[start..end];
        consumed = end;
        let header_byte = nalu[sc_len];
        let (is_param, is_vcl, is_idr) = classify_output_nalu(codec, header_byte);
        let starts_new_au =
            *pending_has_slice && (is_param || is_vcl || is_aud(codec, header_byte));
        if starts_new_au {
            let _ = tx.try_send(EncodedVideoFrame {
                codec,
                is_keyframe: *pending_is_keyframe,
                has_codec_config: *pending_has_config,
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

fn classify_output_nalu(codec: VideoCodec, header_byte: u8) -> (bool, bool, bool) {
    match codec {
        VideoCodec::H265 => {
            let nal_type = (header_byte >> 1) & 0x3f;
            let is_param = matches!(nal_type, 32..=34);
            let is_vcl = nal_type < 32;
            let is_idr = matches!(nal_type, 16..=21);
            (is_param, is_vcl, is_idr)
        }
        VideoCodec::H264 => {
            let nal_type = header_byte & 0x1f;
            let is_param = matches!(nal_type, 7 | 8);
            let is_vcl = matches!(nal_type, 1..=5);
            let is_idr = nal_type == 5;
            (is_param, is_vcl, is_idr)
        }
        VideoCodec::Jpeg => (false, true, true),
    }
}

fn is_aud(codec: VideoCodec, header_byte: u8) -> bool {
    match codec {
        VideoCodec::H265 => ((header_byte >> 1) & 0x3f) == 35,
        VideoCodec::H264 => (header_byte & 0x1f) == 9,
        VideoCodec::Jpeg => false,
    }
}

/// Choose the local client codec and mode (`passthrough = true` when matching
/// WeChat's active wire codec, otherwise transcoding to the client's first
/// supported codec).
pub fn negotiate_video_codec(
    wire: VideoCodec,
    client_codecs: &[VideoCodec],
    caps: &VideoCodecCapabilities,
    is_send: bool,
) -> Option<(VideoCodec, bool)> {
    if client_codecs.contains(&wire) {
        return Some((wire, true));
    }
    for &candidate in client_codecs {
        let supported = if is_send {
            caps.can_transcode(candidate, wire)
        } else {
            caps.can_transcode(wire, candidate)
        };
        if supported {
            return Some((candidate, false));
        }
    }
    None
}

/// Encode a synthetic solid/gradient JPEG image via `ffmpeg` (only for tests/diagnostics).
#[cfg(test)]
pub fn synthetic_test_jpeg(width: u16, height: u16, shift: u8) -> Result<Vec<u8>> {
    let ffmpeg = find_ffmpeg().context("Missing ffmpeg for synthetic JPEG")?;
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    for y in 0..height {
        for x in 0..width {
            rgb.push((x.wrapping_add(u16::from(shift)) & 0xff) as u8);
            rgb.push((y & 0xff) as u8);
            rgb.push(shift.wrapping_mul(3));
        }
    }
    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &format!("{width}x{height}"),
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-q:v",
            "5",
            "-f",
            "image2pipe",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&rgb)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() || out.stdout.len() < 4 {
        anyhow::bail!("Failed to generate synthetic JPEG");
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_prefers_passthrough_before_transcoding() {
        let caps = VideoCodecCapabilities {
            ffmpeg_path: Some(PathBuf::from("/usr/bin/ffmpeg")),
            h265_encode: true,
            h265_decode: true,
            h264_encode: true,
            h264_decode: true,
            jpeg_encode: true,
            jpeg_decode: true,
        };
        assert_eq!(
            negotiate_video_codec(
                VideoCodec::H265,
                &[VideoCodec::H264, VideoCodec::H265, VideoCodec::Jpeg],
                &caps,
                false
            ),
            Some((VideoCodec::H265, true))
        );
        assert_eq!(
            negotiate_video_codec(
                VideoCodec::H265,
                &[VideoCodec::H264, VideoCodec::Jpeg],
                &caps,
                false
            ),
            Some((VideoCodec::H264, false))
        );
        assert_eq!(
            negotiate_video_codec(VideoCodec::H265, &[VideoCodec::Jpeg], &caps, true),
            Some((VideoCodec::Jpeg, false))
        );
    }

    #[test]
    fn ffmpeg_jpeg_to_hevc_to_jpeg_roundtrip_when_ffmpeg_available() {
        let caps = probe_video_capabilities();
        if !caps.can_transcode(VideoCodec::Jpeg, VideoCodec::H265)
            || !caps.can_transcode(VideoCodec::H265, VideoCodec::Jpeg)
        {
            return;
        }
        let mut enc = VideoTranscoder::start(TranscodeConfig {
            from: VideoCodec::Jpeg,
            to: VideoCodec::H265,
            width: 320,
            height: 240,
            fps: 15,
            bitrate_kbps: 400,
            rotation: VideoRotation::Deg0,
        })
        .unwrap();
        for i in 0..4u8 {
            let jpg = synthetic_test_jpeg(320, 240, i * 25).unwrap();
            enc.push(&jpg).unwrap();
        }
        enc.finish_input();
        let mut hevc_frames = Vec::new();
        while let Some(frame) = enc.recv() {
            hevc_frames.push(frame);
        }
        assert!(!hevc_frames.is_empty(), "expected encoded HEVC frames");
        assert!(hevc_frames[0].is_keyframe);
        assert!(hevc_frames[0].has_codec_config);

        let mut dec = VideoTranscoder::start(TranscodeConfig {
            from: VideoCodec::H265,
            to: VideoCodec::Jpeg,
            width: 320,
            height: 240,
            fps: 15,
            bitrate_kbps: 400,
            rotation: VideoRotation::Deg0,
        })
        .unwrap();
        for frame in &hevc_frames {
            dec.push(&frame.bytes).unwrap();
        }
        dec.finish_input();
        let decoded = dec.recv().expect("expected decoded JPEG frame");
        assert_eq!(&decoded.bytes[..2], &[0xff, 0xd8]);
        assert_eq!(&decoded.bytes[decoded.bytes.len() - 2..], &[0xff, 0xd9]);

        // Verify 90° clockwise rotation flips a landscape 320x240 stream to portrait (180x240 inside 320x240 bounds).
        let mut rotated = VideoTranscoder::start(TranscodeConfig {
            from: VideoCodec::H265,
            to: VideoCodec::Jpeg,
            width: 320,
            height: 240,
            fps: 15,
            bitrate_kbps: 400,
            rotation: VideoRotation::Deg90,
        })
        .unwrap();
        for frame in &hevc_frames {
            rotated.push(&frame.bytes).unwrap();
        }
        rotated.finish_input();
        let rotated_jpg = rotated.recv().expect("expected rotated JPEG frame");
        let (rot_w, rot_h) = jpeg_dimensions(&rotated_jpg.bytes).expect("JPEG SOF dimensions");
        assert!(
            rot_h > rot_w,
            "expected portrait dimensions after 90° CW rotation, got {rot_w}x{rot_h}"
        );
    }

    fn jpeg_dimensions(bytes: &[u8]) -> Option<(u16, u16)> {
        if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
            return None;
        }
        let mut i = 2usize;
        while i + 4 <= bytes.len() {
            if bytes[i] != 0xff {
                return None;
            }
            while i < bytes.len() && bytes[i] == 0xff {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let marker = bytes[i];
            i += 1;
            if matches!(marker, 0xd8 | 0xd9 | 0x01) || (0xd0..=0xd7).contains(&marker) {
                continue;
            }
            if i + 2 > bytes.len() {
                break;
            }
            let len = usize::from(u16::from_be_bytes([bytes[i], bytes[i + 1]]));
            if len < 2 || i + len > bytes.len() {
                break;
            }
            if matches!(marker, 0xc0..=0xc2) && len >= 7 {
                let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]);
                let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
                return Some((w, h));
            }
            i += len;
        }
        None
    }
}
