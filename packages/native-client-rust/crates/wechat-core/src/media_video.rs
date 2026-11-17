//! Normal video is an owned encoding policy, not a reconstruction of official
//! quality presets. Keep the entire video while fitting the legacy upload cap.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::File,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = wechat_protocol::media::MAX_NATIVE_UPLOAD_BYTES;
const REPORT_LIMIT: usize = 65_536;
const STDERR_LIMIT: usize = 16_384;
const PREPARE_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// [`prepare_normal_file`] for a source held in memory.
#[cfg(test)]
pub(super) fn prepare_normal(
    input: &[u8],
    seconds: u32,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    use std::io::Write;
    cancelled()?;
    validate_input_length(input.len())?;
    wechat_media::inspect_video_container(input)?;
    let dir = wechat_platform::private_tempdir("wechat-video-")?;
    let source = dir.path().join("source.mp4");
    let mut file = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&source)?;
    for chunk in input.chunks(1_048_576) {
        cancelled()?;
        file.write_all(chunk)?;
    }
    drop(file);
    prepare_normal_file(&source, seconds, cancelled)
}

/// Produce one MP4 with H.264 Main/yuv420p, longest edge at most 720, at most 30
/// frames/second, and stereo 48kHz AAC when the source has audio. The bitrate
/// budget reserves 20% of the upload cap for muxing/encoder variation. Inputs
/// that cannot fit even 32kbps video plus 64kbps audio are refused. A file-size
/// stop is only a safety guard: independently decoded frame counts and video
/// durations must still match before any bytes are returned.
///
/// The source is a private file that FFprobe and FFmpeg read directly; it is
/// never loaded into memory.
pub(super) fn prepare_normal_file(
    source: &Path,
    seconds: u32,
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    cancelled()?;
    let length = std::fs::metadata(source)?.len();
    validate_input_length(usize::try_from(length)?)?;
    wechat_media::inspect_video_reader(&mut File::open(source)?, length)?;
    ensure!((1..=3600).contains(&seconds), "video_duration_limit");
    let deadline = Instant::now() + PREPARE_TIMEOUT;
    let dir = wechat_platform::private_tempdir("wechat-video-")?;
    let output = dir.path().join("normal.mp4");
    drop(
        wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&output)?,
    );
    let original = probe(source, false, deadline, &mut cancelled)?;
    ensure!(
        original.container_duration.ceil() as u32 == seconds,
        "video_duration_changed"
    );
    let video_bitrate = bitrate_budget(original.duration, original.audio)?;
    let mut command = Command::new("ffmpeg");
    command.args([
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
        "2",
        "-filter_threads",
        "1",
        "-i",
    ]);
    command.arg(source).args([
        "-map",
        // `V` skips attached pictures (cover art), which FFmpeg lists as video.
        "0:V:0",
        "-map",
        "0:a:0?",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-sn",
        "-dn",
        "-vf",
        // Full-range sources (yuvj420p or color_range=pc) keep their range
        // through the encoder on FFmpeg 7 and later unless it is converted
        // here, and the result would fail the yuv420p check below.
        "scale=w='min(720,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2:out_range=tv,setsar=1,fps=30",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-profile:v",
        "main",
        "-level:v",
        "3.1",
        "-pix_fmt",
        "yuv420p",
        "-threads",
        "2",
        "-b:v",
    ]);
    command
        .arg(video_bitrate.to_string())
        .args(["-maxrate", &video_bitrate.to_string(), "-bufsize"])
        .arg((video_bitrate * 2).to_string())
        .args([
            "-c:a", "aac", "-b:a", "64000", "-ac", "2", "-ar", "48000", "-t",
        ])
        .arg(format!("{:.9}", original.duration))
        .args(["-movflags", "+faststart", "-fs"])
        .arg((OUTPUT_LIMIT + 1).to_string())
        .args(["-f", "mp4"])
        .arg(&output);
    execute(command, Some(&output), deadline, &mut cancelled)?;
    let encoded = probe(&output, true, deadline, &mut cancelled)?;
    let tolerance = (2.0 / original.frame_rate)
        .max(2.0 / encoded.frame_rate)
        .max(0.1);
    ensure!(
        (encoded.duration - original.duration).abs() <= tolerance
            && encoded.audio == original.audio,
        "video_encoder_duration_mismatch"
    );
    cancelled()?;
    let mut bytes = Vec::new();
    File::open(&output)?
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= OUTPUT_LIMIT,
        "media_encoder_output_limit"
    );
    cancelled()?;
    Ok(bytes)
}

fn validate_input_length(length: usize) -> Result<()> {
    ensure!(
        length > 0 && length <= wechat_media::MAX_VIDEO_SOURCE_BYTES,
        "media_source_limit"
    );
    Ok(())
}

fn bitrate_budget(duration: f64, audio: bool) -> Result<u64> {
    ensure!(
        duration.is_finite() && duration > 0.0 && duration <= 3600.0,
        "video_duration_limit"
    );
    let total = (OUTPUT_LIMIT as f64 * 8.0 * 0.8 / duration).floor() as u64;
    let video = total.saturating_sub(if audio { 64_000 } else { 0 });
    ensure!(video >= 32_000, "normal_video_size_limit");
    Ok(video.min(2_000_000))
}

struct VideoProbe {
    duration: f64,
    container_duration: f64,
    frame_rate: f64,
    audio: bool,
}

fn parse_probe(bytes: &[u8], normal: bool) -> Result<VideoProbe> {
    parse_probe_inner(bytes, normal, true)
}

fn parse_probe_inner(bytes: &[u8], normal: bool, require_frames: bool) -> Result<VideoProbe> {
    ensure!(bytes.len() <= REPORT_LIMIT, "media_probe_output_limit");
    let result: serde_json::Value = serde_json::from_slice(bytes).context("media_probe_failed")?;
    let streams = result["streams"].as_array().context("media_probe_failed")?;
    // Phone recordings carry timed-metadata tracks (an iPhone MOV has several
    // `mebx` data streams) that are never decoded or encoded: the playback copy
    // maps one video and one audio stream and drops data and subtitles. Only
    // video and audio count toward the limit; the total stays bounded so the
    // probe report does too.
    ensure!(streams.len() <= 32, "video_stream_limit");
    ensure!(
        streams
            .iter()
            .filter(|s| matches!(s["codec_type"].as_str(), Some("video" | "audio")))
            .count()
            <= 8,
        "video_stream_limit"
    );
    // Cover art is a one-frame video stream marked as an attached picture; it is
    // not encoded, so it does not count as the source's video.
    let videos: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1)
        .collect();
    let audios: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    // A source may have several audio tracks; only the first is encoded, so
    // the playback copy still has at most one.
    ensure!(
        videos.len() == 1 && (!normal || audios.len() <= 1),
        "video_stream_limit"
    );
    let video = videos[0];
    let width = video["width"].as_u64().context("media_probe_failed")?;
    let height = video["height"].as_u64().context("media_probe_failed")?;
    ensure!(
        (2..=8192).contains(&width) && (2..=8192).contains(&height) && width * height <= 16_777_216,
        "video_dimension_limit"
    );
    let number = |value: &serde_json::Value| -> Result<f64> {
        let n = value
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .context("media_probe_failed")?;
        ensure!(n.is_finite() && n > 0.0, "media_probe_failed");
        Ok(n)
    };
    let container_duration = number(&result["format"]["duration"])?;
    let duration = match video["duration"].as_str() {
        Some(_) => number(&video["duration"])?,
        None => container_duration,
    };
    ensure!(
        duration <= 3600.0 && container_duration <= 3600.05,
        "video_duration_limit"
    );
    let (numerator, denominator) = video["avg_frame_rate"]
        .as_str()
        .and_then(|s| s.split_once('/'))
        .context("media_probe_failed")?;
    let frame_rate = numerator.parse::<f64>().context("media_probe_failed")?
        / denominator.parse::<f64>().context("media_probe_failed")?;
    ensure!(
        frame_rate.is_finite() && frame_rate > 0.0 && frame_rate <= 1000.0,
        "video_frame_rate_limit"
    );
    if require_frames {
        let frames = video["nb_read_frames"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .context("media_probe_failed")?;
        ensure!(frames > 0 && frames <= 3_600_000, "video_frame_count_limit");
        // Reading/decoding every frame prevents a muxer's unchanged duration
        // from disguising early termination at the file-size guard.
        ensure!(
            (frames as f64 / frame_rate - duration).abs() <= (2.0 / frame_rate).max(0.1),
            "video_encoder_incomplete"
        );
    }
    if normal {
        ensure!(
            video["codec_name"] == "h264"
                && video["pix_fmt"] == "yuv420p"
                && width <= 720
                && height <= 720
                && width.is_multiple_of(2)
                && height.is_multiple_of(2)
                && frame_rate <= 30.01,
            "normal_video_format"
        );
        if let Some(audio) = audios.first() {
            ensure!(
                audio["codec_name"] == "aac"
                    && audio["sample_rate"] == "48000"
                    && audio["channels"] == 2,
                "normal_video_format"
            );
        }
    }
    Ok(VideoProbe {
        duration,
        container_duration,
        frame_rate,
        audio: !audios.is_empty(),
    })
}

fn probe(
    path: &Path,
    normal: bool,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> Result<()>,
) -> Result<VideoProbe> {
    // Inspect shape before full decoding, so an oversized advertised frame is
    // refused before the frame-count pass starts its decoder work.
    for count_frames in [false, true] {
        let mut command = Command::new("ffprobe");
        command.args([
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
            "-show_entries",
            "stream=codec_name,codec_type,pix_fmt,width,height,duration,avg_frame_rate,nb_read_frames,sample_rate,channels:stream_disposition=attached_pic:format=duration",
            "-of",
            "json",
        ]);
        if count_frames {
            command.arg("-count_frames");
        }
        command.arg(path);
        let bytes = execute(command, None, deadline, cancelled)?;
        if count_frames {
            return parse_probe(&bytes, normal);
        }
        parse_probe_inner(&bytes, normal, false)?;
    }
    unreachable!("full frame-count pass returns above")
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_pipe(mut pipe: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            return Ok(result);
        }
        if count > limit.saturating_sub(result.len()) {
            overflow.store(true, Ordering::Relaxed);
            bail!("media_encoder_report_limit");
        }
        result.extend_from_slice(&buffer[..count]);
    }
}

fn execute(
    mut command: Command,
    output: Option<&Path>,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    cancelled()?;
    ensure!(Instant::now() < deadline, "media_encoder_timeout");
    let mut child = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| anyhow::anyhow!("media_encoder_unavailable"))?,
    );
    let stdout = child.0.stdout.take().context("media_encoder_failed")?;
    let stderr = child.0.stderr.take().context("media_encoder_failed")?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_overflow = overflow.clone();
    let stderr_overflow = overflow.clone();
    let stdout = thread::spawn(move || read_pipe(stdout, REPORT_LIMIT, stdout_overflow));
    let stderr = thread::spawn(move || read_pipe(stderr, STDERR_LIMIT, stderr_overflow));
    let status = (|| -> Result<()> {
        loop {
            cancelled()?;
            ensure!(Instant::now() < deadline, "media_encoder_timeout");
            ensure!(
                !overflow.load(Ordering::Relaxed),
                "media_encoder_report_limit"
            );
            if let Some(path) = output {
                match std::fs::metadata(path) {
                    Ok(metadata) => ensure!(
                        metadata.len() <= OUTPUT_LIMIT as u64,
                        "media_encoder_output_limit"
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(status) = child.0.try_wait()? {
                ensure!(status.success(), "media_encoder_failed");
                return Ok(());
            }
            thread::sleep(POLL_INTERVAL);
        }
    })();
    // Kill and reap before joining pipe readers on every failure path.
    drop(child);
    let stdout = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("media_encoder_failed"))?;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("media_encoder_failed"))?;
    status?;
    let bytes = stdout?;
    ensure!(stderr?.is_empty(), "media_encoder_failed");
    cancelled()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wechat_media::inspect_video_container;

    fn synthetic_video(muxer: &str, codec: &str, brand: Option<&str>) -> Vec<u8> {
        let dir = wechat_platform::private_tempdir("wechat-video-source-test-").unwrap();
        let path = dir.path().join("source.bin");
        let mut command = Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=5:duration=1",
            "-c:v",
            codec,
            "-threads",
            "1",
            "-f",
            muxer,
        ]);
        if let Some(brand) = brand {
            command.args(["-brand", brand]);
        }
        assert!(command.arg(&path).status().unwrap().success());
        std::fs::read(path).unwrap()
    }

    fn legacy_quicktime(mut source: Vec<u8>) -> Vec<u8> {
        // Removing the optional file-type box also shifts the actual sample
        // bytes. Correct the synthetic movie's chunk table to keep it playable.
        fn shift_chunk_offsets(boxes: &mut [u8], removed: u32) {
            let mut offset = 0;
            while offset < boxes.len() {
                let size =
                    u32::from_be_bytes(boxes[offset..offset + 4].try_into().unwrap()) as usize;
                let current = &mut boxes[offset..offset + size];
                match &current[4..8] {
                    b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                        shift_chunk_offsets(&mut current[8..], removed);
                    }
                    b"stco" => {
                        let count = u32::from_be_bytes(current[12..16].try_into().unwrap());
                        assert_eq!(current.len(), 16 + count as usize * 4);
                        for value in current[16..].chunks_exact_mut(4) {
                            let position = u32::from_be_bytes(value.try_into().unwrap());
                            value.copy_from_slice(
                                &position.checked_sub(removed).unwrap().to_be_bytes(),
                            );
                        }
                    }
                    _ => {}
                }
                offset += size;
            }
        }
        assert_eq!(&source[4..8], b"ftyp");
        let removed = u32::from_be_bytes(source[..4].try_into().unwrap());
        source.drain(..removed as usize);
        shift_chunk_offsets(&mut source, removed);
        source
    }

    #[test]
    fn movie_family_sources_prepare_playback_and_preserve_original_upload_bytes() {
        use wechat_protocol::media::{decrypt_cdn, MediaQuality, PreparedUpload};
        for (muxer, codec, brand, expected_type) in [
            ("mp4", "libx264", None, "video/mp4"),
            ("mov", "mjpeg", None, "video/quicktime"),
            ("3gp", "mpeg4", None, "video/3gpp"),
            ("3g2", "mpeg4", None, "video/3gpp2"),
            ("mov", "jpeg2000", Some("mjp2"), "video/mj2"),
        ] {
            let source = synthetic_video(muxer, codec, brand);
            let sources = if muxer == "mov" && brand.is_none() {
                vec![source.clone(), legacy_quicktime(source)]
            } else {
                vec![source]
            };
            for source in sources {
                assert_eq!(inspect_video_container(&source).unwrap(), expected_type);
                let source_hash = openssl::sha::sha256(&source);
                let seconds = super::super::video_duration(&source, || Ok(())).unwrap();
                let normal = prepare_normal(&source, seconds, || Ok(())).unwrap();
                assert_eq!(inspect_video_container(&normal).unwrap(), "video/mp4");
                assert!(normal.len() <= OUTPUT_LIMIT);
                let thumbnail = super::super::video_thumbnail(&normal, || Ok(())).unwrap();
                assert_eq!(
                    wechat_media::inspect_image(&thumbnail)
                        .unwrap()
                        .content_type,
                    "image/jpeg"
                );
                assert_eq!(
                    super::super::validate_media_bytes(super::super::MediaKind::Video, &source)
                        .unwrap()
                        .0,
                    expected_type
                );
                let original = PreparedUpload::with_material(
                    &source,
                    MediaQuality::Video,
                    "source",
                    "filehelper",
                    &[7; 16],
                    "00112233445566778899aabbccddeeff",
                )
                .unwrap();
                assert_eq!(
                    decrypt_cdn(&original.encrypted, &original.aes_key).unwrap(),
                    source
                );
                assert_eq!(openssl::sha::sha256(&source), source_hash);
                assert_ne!(normal, source);
            }
        }
    }

    #[test]
    fn movie_preparation_rejects_audio_only_and_other_containers() {
        let dir = wechat_platform::private_tempdir("wechat-video-reject-test-").unwrap();
        let audio = dir.path().join("audio.m4a");
        assert!(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "aac",
            ])
            .arg(&audio)
            .status()
            .unwrap()
            .success());
        let source = std::fs::read(audio).unwrap();
        assert!(inspect_video_container(&source).is_ok());
        assert!(prepare_normal(&source, 1, || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("video_stream_limit"));
        let avi = synthetic_video("avi", "mpeg4", None);
        assert!(prepare_normal(&avi, 1, || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("invalid_video_source"));
        let source = synthetic_video("mov", "mjpeg", None);
        assert!(prepare_normal(&source[..source.len() / 2], 1, || Ok(())).is_err());
    }

    fn report() -> serde_json::Value {
        json!({"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","width":640,"height":360,"duration":"6.000000","avg_frame_rate":"30/1","nb_read_frames":"180"}],"format":{"duration":"6.000000"}})
    }

    #[test]
    fn normal_video_rejects_truncation_and_changed_format() {
        let mut value = report();
        assert!(parse_probe(&serde_json::to_vec(&value).unwrap(), true).is_ok());
        value["streams"][0]["nb_read_frames"] = json!("30");
        assert!(parse_probe(&serde_json::to_vec(&value).unwrap(), true)
            .err()
            .unwrap()
            .to_string()
            .contains("incomplete"));
        for (key, invalid) in [
            ("codec_name", json!("hevc")),
            ("width", json!(1280)),
            ("avg_frame_rate", json!("0/0")),
        ] {
            let mut value = report();
            value["streams"][0][key] = invalid;
            assert!(parse_probe(&serde_json::to_vec(&value).unwrap(), true).is_err());
        }
    }

    #[test]
    fn normal_video_sizes_and_bitrate_have_explicit_limits() {
        assert!(validate_input_length(wechat_media::MAX_VIDEO_SOURCE_BYTES).is_ok());
        assert!(validate_input_length(wechat_media::MAX_VIDEO_SOURCE_BYTES + 1).is_err());
        assert!(validate_input_length(0).is_err());
        assert_eq!(bitrate_budget(6.0, true).unwrap(), 2_000_000);
        assert!(bitrate_budget(1200.0, true).unwrap() < 100_000);
        assert!(bitrate_budget(3600.0, true).is_err());
        assert!(bitrate_budget(f64::NAN, false).is_err());
        assert!(read_pipe(&[1u8; 8][..], 7, Arc::new(AtomicBool::new(false))).is_err());
    }

    #[test]
    fn normal_video_cancellation_stops_running_encoder() {
        let mut command = Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "color=s=64x64:r=30",
            "-f",
            "null",
            "-",
        ]);
        let mut polls = 0;
        let start = Instant::now();
        let error = execute(command, None, start + Duration::from_secs(10), &mut || {
            polls += 1;
            ensure!(polls < 4, "cancelled_test_video");
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("cancelled_test_video"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn normal_video_process_stops_at_output_or_time_limit() {
        let dir = wechat_platform::private_tempdir("wechat-video-test-").unwrap();
        let output = dir.path().join("too-large.mp4");
        File::create(&output)
            .unwrap()
            .set_len(OUTPUT_LIMIT as u64 + 1)
            .unwrap();
        let mut command = Command::new("ffmpeg");
        command.args([
            "-v",
            "error",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "color=s=64x64:r=30",
            "-f",
            "null",
            "-",
        ]);
        let error = execute(
            command,
            Some(&output),
            Instant::now() + Duration::from_secs(10),
            &mut || Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("media_encoder_output_limit"));
        let error = execute(
            Command::new("unused-after-deadline"),
            None,
            Instant::now(),
            &mut || Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("media_encoder_timeout"));
    }

    #[test]
    fn normal_video_encodes_full_video_and_audio_under_cap() {
        let dir = wechat_platform::private_tempdir("wechat-video-test-").unwrap();
        let source = dir.path().join("source.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1280x720:rate=18:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000:duration=3",
                "-c:v",
                "libx264",
                "-threads",
                "2",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-t",
                "3",
                "-movflags",
                "+faststart",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let input = std::fs::read(&source).unwrap();
        let original_hash = openssl::sha::sha256(&input);
        let wrong_duration = prepare_normal(&input, 4, || Ok(())).unwrap_err();
        assert!(wrong_duration
            .to_string()
            .contains("video_duration_changed"));
        let encoded = prepare_normal(&input, 3, || Ok(())).unwrap();
        assert!(encoded.len() <= OUTPUT_LIMIT);
        assert_eq!(openssl::sha::sha256(&input), original_hash);
        let output = dir.path().join("normal.mp4");
        std::fs::write(&output, encoded).unwrap();
        let probe = probe(&output, true, Instant::now() + PREPARE_TIMEOUT, &mut || {
            Ok(())
        })
        .unwrap();
        assert!((probe.duration - 3.0).abs() < 0.1);
        assert!(probe.audio);
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-xerror", "-i"])
            .arg(output)
            .args(["-f", "null", "-"])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn source_probe_ignores_cover_art_and_extra_audio_tracks() {
        let video = report()["streams"][0].clone();
        let audio =
            json!({"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2});
        let cover = json!({"codec_type":"video","codec_name":"mjpeg","width":320,"height":320,"nb_read_frames":"1","disposition":{"attached_pic":1}});
        let mut value = report();
        value["streams"] = json!([video, audio, audio, cover]);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(parse_probe(&bytes, false).unwrap().audio);
        // The playback copy itself must still have one video and at most one
        // audio stream.
        assert!(parse_probe(&bytes, true)
            .err()
            .unwrap()
            .to_string()
            .contains("video_stream_limit"));
        value["streams"] = json!([cover, audio]);
        assert!(parse_probe(&serde_json::to_vec(&value).unwrap(), false)
            .err()
            .unwrap()
            .to_string()
            .contains("video_stream_limit"));
    }

    #[test]
    fn source_probe_ignores_phone_metadata_tracks_but_keeps_its_limits() {
        // The stream layout of an iPhone recording: HEVC video, AAC plus spatial
        // audio, and seven timed-metadata (`mebx`) data streams.
        let video = report()["streams"][0].clone();
        let aac =
            json!({"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2});
        let spatial = json!({"codec_type":"audio","codec_name":"apple_apac","sample_rate":"48000","channels":4});
        let metadata = json!({"codec_type":"data","avg_frame_rate":"0/0"});
        let probe = |streams: Vec<serde_json::Value>| {
            let mut value = report();
            value["streams"] = serde_json::Value::Array(streams);
            parse_probe(&serde_json::to_vec(&value).unwrap(), false)
        };
        let mut phone = vec![video.clone(), aac.clone(), spatial];
        phone.extend(std::iter::repeat_n(metadata.clone(), 7));
        assert!(probe(phone).unwrap().audio);
        // Decoded streams keep their limit of eight.
        let mut many_audio = vec![video.clone()];
        many_audio.extend(std::iter::repeat_n(aac, 8));
        assert!(probe(many_audio)
            .err()
            .unwrap()
            .to_string()
            .contains("video_stream_limit"));
        // And the report as a whole stays bounded.
        let mut many_data = vec![video];
        many_data.extend(std::iter::repeat_n(metadata, 32));
        assert!(probe(many_data)
            .err()
            .unwrap()
            .to_string()
            .contains("video_stream_limit"));
    }

    #[test]
    fn normal_video_accepts_sources_with_cover_art_and_two_audio_tracks() {
        let dir = wechat_platform::private_tempdir("wechat-video-streams-test-").unwrap();
        let cover = dir.path().join("cover.jpg");
        let plain = dir.path().join("plain.mp4");
        let source = dir.path().join("source.mp4");
        let ffmpeg = |args: &[&str], output: &Path| {
            assert!(Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin"])
                .args(args)
                .arg(output)
                .status()
                .unwrap()
                .success());
        };
        ffmpeg(
            &["-f", "lavfi", "-i", "color=c=red:s=64x64", "-frames:v", "1"],
            &cover,
        );
        ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=10:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:sample_rate=48000:duration=1",
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ],
            &plain,
        );
        let plain = plain.to_str().unwrap();
        let cover = cover.to_str().unwrap();
        ffmpeg(
            &[
                "-i",
                plain,
                "-i",
                cover,
                "-map",
                "0",
                "-map",
                "1",
                "-c",
                "copy",
                "-disposition:v:1",
                "attached_pic",
            ],
            &source,
        );
        let input = std::fs::read(&source).unwrap();
        let encoded = prepare_normal(&input, 1, || Ok(())).unwrap();
        let output = dir.path().join("normal.mp4");
        std::fs::write(&output, encoded).unwrap();
        let probe = probe(&output, true, Instant::now() + PREPARE_TIMEOUT, &mut || {
            Ok(())
        })
        .unwrap();
        assert!(probe.audio);
    }

    #[test]
    fn normal_video_converts_full_range_sources_to_limited_range() {
        let dir = wechat_platform::private_tempdir("wechat-video-range-test-").unwrap();
        let source = dir.path().join("source.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=10:duration=1",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-color_range",
                "pc",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let input = std::fs::read(&source).unwrap();
        let encoded = prepare_normal(&input, 1, || Ok(())).unwrap();
        let output = dir.path().join("normal.mp4");
        std::fs::write(&output, encoded).unwrap();
        let report = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=pix_fmt,color_range",
                "-of",
                "json",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(report.status.success());
        let report: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
        assert_eq!(report["streams"][0]["pix_fmt"], "yuv420p");
        assert_ne!(report["streams"][0]["color_range"], "pc");
    }
}
