//! Representative still previews for the observed WXGF v2 envelope.
//!
//! Partition discovery follows sjzar/chatlog's Apache-2.0 implementation at
//! a16b689f4e7120f3aa3a1bfb42853113101068f1, pkg/util/dat2img/wxgf.go.
//! See ../NOTICE.md and ../LICENSE-CHATLOG. This adaptation validates bounds and
//! HEVC framing, restricts the decoder process, and deliberately does not infer
//! animation, timestamps or alpha from partition-size ratios. The largest HEVC
//! partition supplies one representative frame; every original byte is retained
//! by the caller independently of this lossy JPEG preview.

use crate::{image::inspect_standard_image, DecodedImage, MAX_IMAGE_BYTES};
use anyhow::{bail, ensure, Result};
use std::{
    io::Write,
    ops::Range,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const MAX_PARTITIONS: usize = 512;
const MAX_NAL_UNITS: usize = 2048;
const MAX_DECODED_PIXELS: u64 = 16_777_216;
const DECODE_TIMEOUT: Duration = Duration::from_secs(10);

struct WxgfPartition {
    range: Range<usize>,
    width: u32,
    height: u32,
}

fn start_code_length(bytes: &[u8], position: usize) -> usize {
    if bytes.get(position..position + 4) == Some(b"\0\0\0\x01") {
        4
    } else if bytes.get(position..position + 3) == Some(b"\0\0\x01") {
        3
    } else {
        0
    }
}

fn nal_type(bytes: &[u8]) -> Option<u8> {
    let [first, second, ..] = bytes else {
        return None;
    };
    if first & 0x80 != 0 || second & 7 == 0 {
        return None;
    }
    Some((first >> 1) & 0x3f)
}

fn validate_hevc(bytes: &[u8]) -> Result<()> {
    let mut position = 0;
    let mut count = 0;
    let (mut vps, mut sps, mut pps, mut picture) = (false, false, false, false);
    while position < bytes.len() {
        let code = start_code_length(bytes, position);
        ensure!(code != 0, "wxgf_invalid");
        let payload = position + code;
        let mut end = payload;
        while end < bytes.len() && start_code_length(bytes, end) == 0 {
            end += 1;
        }
        let kind = nal_type(&bytes[payload..end]).ok_or_else(|| anyhow::anyhow!("wxgf_invalid"))?;
        match kind {
            0..=31 => picture = true,
            32 => vps = true,
            33 => sps = true,
            34 => pps = true,
            35..=40 => {}
            _ => bail!("wxgf_invalid"),
        }
        count += 1;
        ensure!(count <= MAX_NAL_UNITS, "wxgf_partition_limit");
        position = end;
    }
    ensure!(vps && sps && pps && picture, "wxgf_invalid");
    Ok(())
}

fn partition(bytes: &[u8]) -> Result<WxgfPartition> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES,
        "image_size_limit"
    );
    ensure!(
        bytes.len() >= 15 && bytes.starts_with(b"wxgf"),
        "wxgf_invalid"
    );
    let header = bytes[4] as usize;
    // Both observed header lengths (18 and 19) carry version 2, then u16-BE
    // canvas dimensions at offsets 7/9. Unknown versions are not guessed.
    ensure!(
        header >= 15 && header < bytes.len() && bytes[5..7] == [0, 2],
        "wxgf_invalid"
    );
    let width = u16::from_be_bytes(bytes[7..9].try_into()?) as u32;
    let height = u16::from_be_bytes(bytes[9..11].try_into()?) as u32;
    ensure!(
        width > 0
            && height > 0
            && width <= 4096
            && height <= 4096
            && u64::from(width) * u64::from(height) <= MAX_DECODED_PIXELS,
        "image_decode_limit"
    );
    let mut position = header + 4;
    let mut largest: Option<Range<usize>> = None;
    let mut count = 0;
    while position < bytes.len() {
        let code = start_code_length(bytes, position);
        if code == 0 {
            position += 1;
            continue;
        }
        let length = u32::from_be_bytes(bytes[position - 4..position].try_into()?) as usize;
        let end = position
            .checked_add(length)
            .filter(|end| *end <= bytes.len());
        let parameter_set = bytes
            .get(position + code..)
            .and_then(nal_type)
            .is_some_and(|kind| matches!(kind, 32..=34));
        if length < code + 2 || end.is_none() || !parameter_set {
            position += 1;
            continue;
        }
        let end = end.unwrap();
        count += 1;
        ensure!(count <= MAX_PARTITIONS, "wxgf_partition_limit");
        if largest.as_ref().is_none_or(|range| length > range.len()) {
            largest = Some(position..end);
        }
        // Accepted partitions never overlap. Rejected candidates move one byte,
        // so discovery has bounded work even for marker-shaped input bytes.
        position = end;
    }
    let range = largest.ok_or_else(|| anyhow::anyhow!("wxgf_partition_missing"))?;
    validate_hevc(&bytes[range.clone()])?;
    Ok(WxgfPartition {
        range,
        width,
        height,
    })
}

/// Decode one representative JPEG without changing the WXGF source. This is a
/// still preview, not a reconstruction of source animation timing or alpha.
pub fn wxgf_preview(bytes: &[u8]) -> Result<DecodedImage> {
    let selected = partition(bytes)?;
    let preview = decode_hevc(&bytes[selected.range], Path::new("ffmpeg"))?;
    let inspection = inspect_standard_image(&preview)?;
    // Some observed odd canvas dimensions are rounded up by one pixel in
    // 4:2:0 HEVC. Keep the decoded dimensions instead of guessing a crop.
    ensure!(
        inspection.content_type == "image/jpeg"
            && inspection.frames == 1
            && (selected.width..=selected.width + (selected.width & 1)).contains(&inspection.width)
            && (selected.height..=selected.height + (selected.height & 1))
                .contains(&inspection.height),
        "wxgf_decode_failed"
    );
    Ok(DecodedImage {
        bytes: preview,
        inspection,
    })
}

fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

#[cfg(unix)]
fn clamp_rlimit(current: libc::rlimit, limit: libc::rlim_t) -> libc::rlimit {
    let cap = |existing: libc::rlim_t| {
        if existing == libc::RLIM_INFINITY {
            limit
        } else {
            existing.min(limit)
        }
    };
    let rlim_max = cap(current.rlim_max);
    let rlim_cur = cap(current.rlim_cur).min(rlim_max);
    libc::rlimit { rlim_cur, rlim_max }
}

fn decoder_limits(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // RLIMIT_AS is intentionally omitted: it caps virtual address space
        // rather than resident memory, so dynamically linked FFmpeg builds with
        // many shared libraries and pipeline threads can fail in pthread_create
        // while RSS remains small. Decoder work is bounded by per-allocation
        // (-max_alloc) and per-frame (-max_pixels) FFmpeg limits, single-frame
        // and input limits, wall-clock timeout, and CPU/file caps; total RSS
        // limits belong at the container or service cgroup level.
        // Only direct getrlimit/setrlimit syscalls run in pre_exec with no
        // allocation or locking, applying strictly to the decoder subprocess.
        unsafe {
            command.pre_exec(|| {
                for (resource, limit) in [
                    (libc::RLIMIT_CPU, 8),
                    (libc::RLIMIT_FSIZE, MAX_IMAGE_BYTES as libc::rlim_t),
                    (libc::RLIMIT_NOFILE, 32),
                ] {
                    let mut current = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::getrlimit(resource, &mut current) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let bounds = clamp_rlimit(current, limit);
                    if libc::setrlimit(resource, &bounds) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}

fn classify_decoder_spawn_error(error: &std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::NotFound {
        return "wxgf_decoder_unavailable";
    }
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOENT | libc::ENOTDIR | libc::EACCES | libc::ENOEXEC)
    ) {
        return "wxgf_decoder_unavailable";
    }
    #[cfg(not(unix))]
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return "wxgf_decoder_unavailable";
    }
    "wxgf_decode_failed"
}

fn wait_for_preview(child: &mut Child, output: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let result = (|| loop {
        if std::fs::metadata(output).is_ok_and(|meta| meta.len() > MAX_IMAGE_BYTES as u64) {
            bail!("wxgf_preview_limit");
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "wxgf_decode_failed");
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "wxgf_decode_timeout");
        std::thread::sleep(Duration::from_millis(10));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn decode_hevc(bytes: &[u8], program: &Path) -> Result<Vec<u8>> {
    decode_hevc_with_max_pixels(bytes, program, MAX_DECODED_PIXELS)
}

fn decode_hevc_with_max_pixels(bytes: &[u8], program: &Path, max_pixels: u64) -> Result<Vec<u8>> {
    let directory = wechat_platform::private_tempdir("wechat-wxgf-")?;
    let input = directory.path().join("frame.hevc");
    let output = directory.path().join("preview.jpg");
    private_file(&input, bytes)?;
    private_file(&output, &[])?;
    let mut command = Command::new(program);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-xerror",
            "-max_alloc",
            "67108864",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            "hevc",
            "-f",
            "hevc",
            "-threads",
            "1",
            "-max_pixels",
        ])
        .arg(max_pixels.to_string())
        .args(["-err_detect", "explode", "-i"])
        .arg(&input)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-c:v",
            "mjpeg",
            "-q:v",
            "4",
            "-pix_fmt",
            "yuvj420p",
            "-f",
            "image2",
            "-update",
            "1",
            "-fs",
            "16777216",
        ])
        .arg(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    decoder_limits(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| anyhow::anyhow!(classify_decoder_spawn_error(&error)))?;
    wait_for_preview(&mut child, &output, DECODE_TIMEOUT)?;
    let length = std::fs::metadata(&output)?.len();
    ensure!(
        length > 0 && length <= MAX_IMAGE_BYTES as u64,
        "wxgf_preview_limit"
    );
    Ok(std::fs::read(output)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/static.wxgf");

    #[test]
    fn decoder_files_are_private_and_never_overwrite_existing_input() {
        let directory = wechat_platform::private_tempdir("wechat-wxgf-test-").unwrap();
        assert!(wechat_platform::private_path(directory.path()).unwrap());
        let input = directory.path().join("frame.hevc");
        private_file(&input, FIXTURE).unwrap();
        assert!(wechat_platform::private_path(&input).unwrap());
        assert!(private_file(&input, b"replacement").is_err());
        assert_eq!(std::fs::read(input).unwrap(), FIXTURE);
    }

    #[test]
    fn synthetic_partition_selects_complete_hevc_and_preserves_all_source_bytes() {
        let selected = partition(FIXTURE).unwrap();
        assert_eq!((selected.width, selected.height), (32, 24));
        assert_eq!(selected.range, 23..FIXTURE.len());
        let mut with_trailer = FIXTURE.to_vec();
        with_trailer.extend_from_slice(b"opaque-wxgf-source-trailer");
        assert_eq!(partition(&with_trailer).unwrap().range, selected.range);
        assert_eq!(&with_trailer[..FIXTURE.len()], FIXTURE);
    }

    #[test]
    fn invalid_headers_lengths_and_nal_headers_are_bounded() {
        for at in [0, 4, 14, 18, 22, FIXTURE.len() - 1] {
            assert!(partition(&FIXTURE[..at]).is_err());
        }
        for header in [0, 4, 14, 255] {
            let mut bytes = FIXTURE.to_vec();
            bytes[4] = header;
            assert!(partition(&bytes).is_err());
        }
        for length in [0u32, 1, u32::MAX] {
            let mut bytes = FIXTURE.to_vec();
            bytes[19..23].copy_from_slice(&length.to_be_bytes());
            assert!(partition(&bytes).is_err());
        }
        for corrupt in [0x80, 0x7e] {
            let mut bytes = FIXTURE.to_vec();
            bytes[27] = corrupt;
            assert!(partition(&bytes).is_err());
        }
        let mut dimensions = FIXTURE.to_vec();
        dimensions[7..9].copy_from_slice(&4097u16.to_be_bytes());
        assert_eq!(
            partition(&dimensions).err().unwrap().to_string(),
            "image_decode_limit"
        );
    }

    #[test]
    fn three_byte_start_codes_and_dominant_color_partition_are_supported() {
        let raw = &FIXTURE[23..];
        let mut three = raw.to_vec();
        three.remove(0);
        let mut bytes = FIXTURE[..19].to_vec();
        bytes.extend_from_slice(&(three.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&three);
        assert!(partition(&bytes).is_ok());
        // A small parameter-only partition must not displace the color stream.
        let mask = &raw[..11];
        bytes = FIXTURE[..19].to_vec();
        bytes.extend_from_slice(&(mask.len() as u32).to_be_bytes());
        bytes.extend_from_slice(mask);
        bytes.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        let expected = bytes.len();
        bytes.extend_from_slice(raw);
        assert_eq!(partition(&bytes).unwrap().range.start, expected);
    }

    #[test]
    fn partition_count_is_bounded_and_false_start_markers_do_not_loop() {
        let mut bytes = FIXTURE[..19].to_vec();
        let nal = b"\0\0\0\x01\x40\x01\x0c";
        for _ in 0..=MAX_PARTITIONS {
            bytes.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            bytes.extend_from_slice(nal);
        }
        assert_eq!(
            partition(&bytes).err().unwrap().to_string(),
            "wxgf_partition_limit"
        );
        bytes.truncate(19);
        bytes.extend_from_slice(&[0; 8192]);
        assert!(partition(&bytes).is_err());
    }

    #[test]
    fn incomplete_parameter_sets_and_excessive_nal_units_are_rejected() {
        assert!(validate_hevc(b"\0\0\0\x01\x40\x01\x0c").is_err());
        let mut stream = Vec::new();
        for _ in 0..=MAX_NAL_UNITS {
            stream.extend_from_slice(b"\0\0\0\x01\x40\x01\x0c");
        }
        assert_eq!(
            validate_hevc(&stream).unwrap_err().to_string(),
            "wxgf_partition_limit"
        );
    }

    #[test]
    fn missing_decoder_has_fixed_error_and_source_size_is_bounded() {
        assert_eq!(
            decode_hevc(b"synthetic", Path::new("/nonexistent-wxgf-decoder"))
                .unwrap_err()
                .to_string(),
            "wxgf_decoder_unavailable"
        );
        let oversized = vec![0; MAX_IMAGE_BYTES + 1];
        assert_eq!(
            partition(&oversized).err().unwrap().to_string(),
            "image_size_limit"
        );
    }

    #[test]
    fn preview_uses_available_decoder_or_returns_explicit_unavailable() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            assert_eq!(
                wxgf_preview(FIXTURE).unwrap_err().to_string(),
                "wxgf_decoder_unavailable"
            );
            return;
        }
        let original = FIXTURE.to_vec();
        let decoded = wxgf_preview(FIXTURE).unwrap();
        assert_eq!(decoded.inspection.content_type, "image/jpeg");
        assert_eq!(
            (
                decoded.inspection.width,
                decoded.inspection.height,
                decoded.inspection.frames
            ),
            (32, 24, 1)
        );
        let inspected = crate::inspect_image(FIXTURE).unwrap();
        assert_eq!(inspected.content_type, "image/x-wxgf");
        assert!(!inspected.animation_preserved);
        assert_eq!(FIXTURE, original);
        let mut odd_canvas = FIXTURE.to_vec();
        odd_canvas[7..9].copy_from_slice(&31u16.to_be_bytes());
        odd_canvas[9..11].copy_from_slice(&23u16.to_be_bytes());
        let padded = wxgf_preview(&odd_canvas).unwrap();
        assert_eq!(
            (padded.inspection.width, padded.inspection.height),
            (32, 24)
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_and_output_limits_kill_and_reap_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let error = wait_for_preview(&mut child, &output, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.to_string(), "wxgf_decode_timeout");
        assert!(child.try_wait().unwrap().is_some());
        std::fs::File::create(&output)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        assert_eq!(
            wait_for_preview(&mut child, &output, Duration::from_secs(1))
                .unwrap_err()
                .to_string(),
            "wxgf_preview_limit"
        );
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn clamp_rlimit_only_tightens_inherited_limits() {
        let inf = libc::RLIM_INFINITY;
        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: inf,
                rlim_max: inf,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (32, 32));

        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 16,
                rlim_max: 64,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (16, 32));

        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 8,
                rlim_max: 16,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (8, 16));

        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 4,
                rlim_max: 4,
            },
            8,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (4, 4));

        // Mixed finite/infinity pairs (such as macOS default RLIMIT_NOFILE).
        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 16,
                rlim_max: inf,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (16, 32));

        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 64,
                rlim_max: inf,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (32, 32));

        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (0, 0));

        // Defensive clamp if soft limit ever exceeds hard limit.
        let clamped = clamp_rlimit(
            libc::rlimit {
                rlim_cur: 64,
                rlim_max: 16,
            },
            32,
        );
        assert_eq!((clamped.rlim_cur, clamped.rlim_max), (16, 16));
    }

    #[test]
    fn decoder_spawn_error_classification_separates_missing_binary_from_runtime_errors() {
        assert_eq!(
            classify_decoder_spawn_error(&std::io::Error::from(std::io::ErrorKind::NotFound)),
            "wxgf_decoder_unavailable"
        );
        #[cfg(unix)]
        {
            for code in [libc::ENOENT, libc::ENOTDIR, libc::EACCES, libc::ENOEXEC] {
                assert_eq!(
                    classify_decoder_spawn_error(&std::io::Error::from_raw_os_error(code)),
                    "wxgf_decoder_unavailable"
                );
            }
            for code in [libc::EPERM, libc::EINVAL, libc::EAGAIN, libc::ENOMEM] {
                assert_eq!(
                    classify_decoder_spawn_error(&std::io::Error::from_raw_os_error(code)),
                    "wxgf_decode_failed"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn decoder_limits_preserve_address_space_and_clamp_stricter_bounds() {
        use std::os::unix::process::CommandExt;

        let mut parent_as = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut parent_as) },
            0
        );

        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", ":"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        decoder_limits(&mut command);
        unsafe {
            command.pre_exec(move || {
                let mut current = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_AS, &mut current) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if current.rlim_cur != parent_as.rlim_cur || current.rlim_max != parent_as.rlim_max
                {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                for (resource, expected_max) in [
                    (libc::RLIMIT_CPU, 8 as libc::rlim_t),
                    (libc::RLIMIT_FSIZE, MAX_IMAGE_BYTES as libc::rlim_t),
                    (libc::RLIMIT_NOFILE, 32 as libc::rlim_t),
                ] {
                    if libc::getrlimit(resource, &mut current) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if current.rlim_max == libc::RLIM_INFINITY
                        || current.rlim_max > expected_max
                        || current.rlim_cur > current.rlim_max
                    {
                        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                    }
                }
                Ok(())
            });
        }
        assert!(command.status().unwrap().success());

        // Tighter inherited hard limits must be clamped rather than failing with EPERM.
        let mut tighter = Command::new("/bin/sh");
        tighter
            .args(["-c", ":"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            tighter.pre_exec(|| {
                for (resource, cap) in [
                    (libc::RLIMIT_CPU, 4 as libc::rlim_t),
                    (libc::RLIMIT_FSIZE, (1024 * 1024) as libc::rlim_t),
                ] {
                    let mut current = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::getrlimit(resource, &mut current) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let bounds = clamp_rlimit(current, cap);
                    if libc::setrlimit(resource, &bounds) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        decoder_limits(&mut tighter);
        unsafe {
            tighter.pre_exec(|| {
                for (resource, cap) in [
                    (libc::RLIMIT_CPU, 4 as libc::rlim_t),
                    (libc::RLIMIT_FSIZE, (1024 * 1024) as libc::rlim_t),
                ] {
                    let mut current = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::getrlimit(resource, &mut current) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if current.rlim_max == libc::RLIM_INFINITY || current.rlim_max > cap {
                        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                    }
                }
                Ok(())
            });
        }
        assert!(tighter.status().unwrap().success());
    }

    #[test]
    fn decode_hevc_enforces_max_pixels_inside_decoder() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let selected = partition(FIXTURE).unwrap();
        let hevc = &FIXTURE[selected.range];
        assert!(decode_hevc(hevc, Path::new("ffmpeg")).is_ok());
        assert_eq!(
            decode_hevc_with_max_pixels(hevc, Path::new("ffmpeg"), 100)
                .unwrap_err()
                .to_string(),
            "wxgf_decode_failed"
        );
    }
}
