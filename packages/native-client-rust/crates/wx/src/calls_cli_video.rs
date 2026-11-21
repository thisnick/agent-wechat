//! TrueColor half-block terminal video rendering for `wx calls`.
//!
//! Camera enumeration, FFmpeg capture/encoding, and remote track tracking live
//! in `wechat_call_client::video` and are shared with the desktop client.

use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;
#[cfg(test)]
pub(super) use wechat_call_client::video::CapturedVideoFrame;
pub(super) use wechat_call_client::video::{
    list_cameras, CameraSource, DecodedFrame, LocalVideoCapture, ReceiveVideo, RemoteVideoTrack,
};
use wechat_core::call_types::CallVideoCapabilities;

/// Compute video capabilities this CLI should advertise in `call.ready`.
pub(super) fn client_video_capabilities(
    daemon_supports_video: bool,
) -> Option<CallVideoCapabilities> {
    wechat_call_client::video::client_video_capabilities(daemon_supports_video, None)
}

/// Render active video track(s) as 24-bit TrueColor Unicode half-block rows.
///
/// Returns an empty `Vec` when no remote video tracks exist so voice calls
/// keep the clean audio-only layout.
pub(super) fn render_rows(video: &ReceiveVideo, columns: usize, max_rows: usize) -> Vec<String> {
    if video.tracks().is_empty() || columns < 24 || max_rows < 4 {
        return Vec::new();
    }
    let tiles: Vec<&RemoteVideoTrack> = video.tracks().values().take(4).collect();
    match tiles.len() {
        1 => render_single_tile(tiles[0], video.peer_camera_off(), columns, max_rows),
        2 => render_two_tiles(tiles[0], tiles[1], columns, max_rows),
        _ => render_quad_tiles(&tiles, columns, max_rows),
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
    use std::{
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };
    use wechat_core::{
        call_types::{
            CallTrack, CallTrackDirection, CallVideoCodec, CallVideoFormat, CallVideoMode,
        },
        calls::pcm_envelope::{
            encode_video, MediaHeader, MediaPayloadKind, WireMediaFrame, FLAG_KEYFRAME,
            FLAG_ROTATE_90,
        },
    };

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
        let single = render_rows(&rx, 80, 14);
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
        let grid = render_rows(&rx, 80, 14);
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
            FLAG_KEYFRAME | FLAG_ROTATE_90,
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
        let frame = rx.tracks()[&65_538].frame.as_ref().unwrap();
        assert_eq!((frame.width, frame.height), (8, 16));
    }
}
