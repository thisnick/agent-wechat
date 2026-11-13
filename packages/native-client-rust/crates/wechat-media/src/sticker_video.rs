//! Bounded MOV/MP4 sticker conversion through the installed FFmpeg tools.
use crate::{prepare_sticker_image, PreparedStickerImage, StickerImageOptions, MAX_STICKER_EDGE};
use anyhow::{ensure, Result};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const MAX_SOURCE: usize = 64 * 1024 * 1024;
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
/// and shape are probed first; shortening and resizing require caller choices.
/// The resulting animation contains no audio and uses a 10 fps conversion policy.
pub fn prepare_sticker(
    input: &[u8],
    options: &StickerImageOptions,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<PreparedStickerImage> {
    cancelled()?;
    if crate::inspect_video_container(input).is_err() {
        let result = prepare_sticker_image(input, options)?;
        cancelled()?;
        return Ok(result);
    }
    ensure!(
        !input.is_empty() && input.len() <= MAX_SOURCE,
        "sticker_source_size_limit"
    );
    let dir = wechat_platform::private_tempdir("wechat-sticker-")?;
    let source = dir.path().join("source.mp4");
    let report = dir.path().join("probe.json");
    let output = dir.path().join("output.gif");
    let mut file = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&source)?;
    file.write_all(input)?;
    drop(file);
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
        .arg(&source);
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
    let resize = width > u64::from(MAX_STICKER_EDGE) || height > u64::from(MAX_STICKER_EDGE);
    ensure!(!resize || options.fit, "sticker_adjustment_required");
    let mut command = Command::new("ffmpeg");
    command.args(["-v","error","-nostdin","-y","-xerror","-max_alloc","67108864","-protocol_whitelist","file",
        "-format_whitelist","mov","-enable_drefs","0","-use_absolute_path","0","-threads","1","-filter_threads","1","-i"])
        .arg(&source).args(["-ss",&start.to_string(),"-t",&seconds.to_string(),"-map","0:v:0","-an","-sn","-dn",
            "-map_metadata","-1","-vf","fps=10,scale=w='min(240,iw)':h='min(240,ih)':force_original_aspect_ratio=decrease:flags=lanczos",
            "-frames:v","100","-fs","1048577","-loop","0"]).arg(&output);
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
        .push("video_to_gif_10fps_without_audio".into());
    if resize {
        result.transformations.push("resize_contain".into());
    }
    if options.trim.is_some() {
        result.transformations.push("trim".into());
    }
    Ok(result)
}
