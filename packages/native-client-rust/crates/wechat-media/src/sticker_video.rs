//! Bounded MOV/MP4 sticker conversion through the installed FFmpeg tools.
use crate::{
    prepare_sticker_image, PreparedStickerImage, StickerImageOptions, DEFAULT_VIDEO_STICKER_EDGE,
    DEFAULT_VIDEO_STICKER_FPS, MAX_STICKER_EDGE, MAX_STICKER_IMAGE_SOURCE_BYTES,
    MAX_STICKER_VIDEO_SOURCE_BYTES, MAX_VIDEO_STICKER_FPS, MIN_STICKER_EDGE,
};
use anyhow::{ensure, Result};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn execute(
    mut command: Command,
    output: &Path,
    limit: u64,
    cancelled: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut child = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| anyhow::anyhow!("sticker_encoder_unavailable"))?,
    );
    loop {
        cancelled()?;
        ensure!(Instant::now() < deadline, "sticker_encoder_timeout");
        if let Ok(metadata) = std::fs::metadata(output) {
            ensure!(metadata.len() <= limit, "sticker_encoder_output_limit");
        }
        if let Some(status) = child.0.try_wait()? {
            ensure!(status.success(), "sticker_encoder_failed");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn owned_bytes(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "sticker_encoder_output_limit");
    Ok(bytes)
}
/// Prepare a supported still image/GIF or a bounded MOV/MP4 video. Video timing
/// and shape are probed first, and a clip longer than ten seconds needs a trim.
/// Video is always re-encoded: without audio, at `fps` (default 10) and shrunk
/// to `max_edge` (default 240 pixels); it is never enlarged.
pub fn prepare_sticker(
    input: &[u8],
    options: &StickerImageOptions,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<PreparedStickerImage> {
    cancelled()?;
    if crate::inspect_video_container(input).is_err() {
        ensure!(
            !input.is_empty() && input.len() <= MAX_STICKER_IMAGE_SOURCE_BYTES,
            "sticker_source_size_limit"
        );
        let result = prepare_sticker_image(input, options)?;
        cancelled()?;
        return Ok(result);
    }
    ensure!(
        !input.is_empty() && input.len() <= MAX_STICKER_VIDEO_SOURCE_BYTES,
        "sticker_source_size_limit"
    );
    let dir = wechat_platform::private_tempdir("wechat-sticker-")?;
    let source = dir.path().join("source.mp4");
    let mut file = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&source)?;
    file.write_all(input)?;
    drop(file);
    convert_video(dir.path(), &source, options, cancelled)
}

/// The same as [`prepare_sticker`] for a source already in a private file, such
/// as a decrypted upload. A video (at most [`MAX_STICKER_VIDEO_SOURCE_BYTES`]) is
/// converted from the file itself and never read into memory; an image or GIF
/// (at most [`MAX_STICKER_IMAGE_SOURCE_BYTES`]) is read and prepared as usual.
pub fn prepare_sticker_file(
    path: &Path,
    options: &StickerImageOptions,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<PreparedStickerImage> {
    use std::io::{Read, Seek, SeekFrom};
    cancelled()?;
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    ensure!(length > 0, "sticker_source_size_limit");
    if crate::inspect_video_reader(&mut file, length).is_err() {
        ensure!(
            length <= MAX_STICKER_IMAGE_SOURCE_BYTES as u64,
            "sticker_source_size_limit"
        );
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(length as usize);
        file.take(MAX_STICKER_IMAGE_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == length, "sticker_source_changed");
        let result = prepare_sticker_image(&bytes, options)?;
        cancelled()?;
        return Ok(result);
    }
    ensure!(
        length <= MAX_STICKER_VIDEO_SOURCE_BYTES as u64,
        "sticker_source_size_limit"
    );
    let dir = wechat_platform::private_tempdir("wechat-sticker-")?;
    convert_video(dir.path(), path, options, cancelled)
}

/// Probe `source` and convert its selected segment into a GIF sticker, using
/// `dir` (private, owned by the caller) for the probe report and output.
fn convert_video(
    dir: &Path,
    source: &Path,
    options: &StickerImageOptions,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<PreparedStickerImage> {
    let report = dir.join("probe.json");
    let output = dir.join("output.gif");
    let mut probe = Command::new("ffprobe");
    probe
        .args([
            "-v",
            "error",
            "-max_alloc",
            "67108864",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            "mov",
            "-enable_drefs",
            "0",
            "-use_absolute_path",
            "0",
            "-threads",
            "1",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,duration:format=duration",
            "-of",
            "json",
            "-o",
        ])
        .arg(&report)
        .arg(source);
    execute(probe, &report, 65536, &mut cancelled)?;
    let shape: serde_json::Value = serde_json::from_slice(&owned_bytes(&report, 65536)?)?;
    let stream = &shape["streams"][0];
    let width = stream["width"].as_u64().unwrap_or(0);
    let height = stream["height"].as_u64().unwrap_or(0);
    ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192 && width * height <= 16_000_000,
        "sticker_decoded_size_limit"
    );
    let duration = stream["duration"]
        .as_str()
        .or_else(|| shape["format"]["duration"].as_str())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::NAN);
    ensure!(
        duration.is_finite() && duration > 0.0,
        "sticker_timing_invalid"
    );
    let (start, seconds) = options
        .trim
        .as_ref()
        .map(|trim| {
            (
                f64::from(trim.start_ms) / 1000.0,
                f64::from(trim.duration_ms) / 1000.0,
            )
        })
        .unwrap_or((0.0, duration));
    ensure!(
        seconds > 0.0 && seconds <= 10.0 && start + seconds <= duration + 0.001,
        "sticker_adjustment_required"
    );
    let edge = options.max_edge.unwrap_or(DEFAULT_VIDEO_STICKER_EDGE);
    ensure!(
        (MIN_STICKER_EDGE..=MAX_STICKER_EDGE).contains(&edge),
        "sticker_option_invalid"
    );
    let resize = width.max(height) > u64::from(edge);
    let fps = options.fps.unwrap_or(DEFAULT_VIDEO_STICKER_FPS);
    ensure!(
        (1..=MAX_VIDEO_STICKER_FPS).contains(&fps),
        "sticker_option_invalid"
    );
    // The sticker frame limit applies to the converted animation.
    let frames = (f64::from(fps) * seconds).ceil() as u64;
    ensure!(frames <= 200, "sticker_adjustment_required");
    let filter = format!(
        "fps={fps},scale=w='min({edge},iw)':h='min({edge},ih)':force_original_aspect_ratio=decrease:flags=lanczos"
    );
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-xerror",
            "-max_alloc",
            "67108864",
            "-protocol_whitelist",
            "file",
            "-format_whitelist",
            "mov",
            "-enable_drefs",
            "0",
            "-use_absolute_path",
            "0",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-i",
        ])
        .arg(source)
        .args([
            "-ss",
            &start.to_string(),
            "-t",
            &seconds.to_string(),
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-vf",
            &filter,
            "-frames:v",
            "200",
            "-fs",
            "1048577",
            "-loop",
            "0",
        ])
        .arg(&output);
    execute(command, &output, 1024 * 1024 + 1, &mut cancelled)?;
    let mut result =
        prepare_sticker_image(&owned_bytes(&output, 1024 * 1024)?, &Default::default())?;
    // The output cap is a refusal boundary, not permission to publish a truncated clip.
    ensure!(
        (result.duration_ms as f64 / 1000.0 - seconds).abs() <= 0.15,
        "sticker_duration_changed"
    );
    result
        .transformations
        .push(format!("video_to_gif_{fps}fps_without_audio"));
    if resize {
        result.transformations.push("resize_contain".into());
    }
    if options.trim.is_some() {
        result.transformations.push("trim".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic MP4, or `None` where FFmpeg is not installed (the portable
    /// macOS and Windows test runners).
    fn clip(size: &str, seconds: u32) -> Option<Vec<u8>> {
        let dir = wechat_platform::private_tempdir("wechat-sticker-video-test-").unwrap();
        let path = dir.path().join("clip.mp4");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size={size}:rate=25:duration={seconds}"))
            .args(["-c:v", "mpeg4", "-threads", "1", "-f", "mp4"])
            .arg(&path)
            .status()
            .ok()?;
        assert!(status.success());
        Some(std::fs::read(path).unwrap())
    }

    #[test]
    fn video_defaults_to_240_pixels_at_10_fps_and_follows_a_requested_size_and_rate() {
        let Some(source) = clip("640x480", 2) else {
            return;
        };
        let shape = |options: &StickerImageOptions| {
            let result = prepare_sticker(&source, options, || Ok(())).unwrap();
            (
                result.inspection.width,
                result.inspection.height,
                result.inspection.frames,
                result
                    .transformations
                    .iter()
                    .find(|step| step.starts_with("video_to_gif"))
                    .cloned()
                    .unwrap(),
            )
        };
        assert_eq!(
            shape(&Default::default()),
            (240, 180, 20, "video_to_gif_10fps_without_audio".into())
        );
        let requested = StickerImageOptions {
            max_edge: Some(320),
            fps: Some(5),
            ..Default::default()
        };
        assert_eq!(
            shape(&requested),
            (320, 240, 10, "video_to_gif_5fps_without_audio".into())
        );
        // A video already in a file converts from that file to the same result.
        let dir = wechat_platform::private_tempdir("wechat-sticker-video-file-").unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, &source).unwrap();
        let from_file = prepare_sticker_file(&path, &requested, || Ok(())).unwrap();
        assert_eq!(
            (
                from_file.inspection.width,
                from_file.inspection.height,
                from_file.inspection.frames
            ),
            (320, 240, 10)
        );
        for options in [
            StickerImageOptions {
                fps: Some(31),
                ..Default::default()
            },
            StickerImageOptions {
                max_edge: Some(4096),
                ..Default::default()
            },
        ] {
            let error = prepare_sticker(&source, &options, || Ok(()))
                .err()
                .expect("refused");
            assert_eq!(error.to_string(), "sticker_option_invalid");
        }
    }
}
