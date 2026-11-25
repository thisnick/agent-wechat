//! Local sticker preparation and visual previews. No account or network access.
use anyhow::{ensure, Result};
use image::{
    codecs::gif::{GifDecoder, GifEncoder, Repeat},
    AnimationDecoder, DynamicImage, Frame, ImageFormat, RgbaImage,
};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

/// Maximum accepted prepared GIF size under this application's sticker policy.
pub const MAX_STICKER_BYTES: usize = 1024 * 1024;
/// Largest image or GIF a sticker can be prepared from; it is decoded in memory.
pub const MAX_STICKER_IMAGE_SOURCE_BYTES: usize = 64 * 1024 * 1024;
/// Largest MOV/MP4 a sticker can be prepared from, the same as a video attachment.
/// Only the converted segment of at most ten seconds is decoded.
pub const MAX_STICKER_VIDEO_SOURCE_BYTES: usize = crate::MAX_FILE_BYTES;
/// Largest accepted edge length. The official client refuses to save larger
/// stickers; inputs within it keep their original encoding unless resized.
pub const MAX_STICKER_EDGE: u32 = 1024;
/// Smallest longest edge a caller may request with `max_edge`.
pub const MIN_STICKER_EDGE: u32 = 16;
/// Longest edge of stickers made from video when the caller sets none.
pub const DEFAULT_VIDEO_STICKER_EDGE: u32 = 240;
/// Frame rate of stickers made from video when the caller sets none.
pub const DEFAULT_VIDEO_STICKER_FPS: u32 = 10;
/// Highest frame rate a caller may request for a sticker made from video.
pub const MAX_VIDEO_STICKER_FPS: u32 = 30;
/// Longest edge of the review images made from a sticker.
const PREVIEW_EDGE: u32 = 240;
const MAX_FRAMES: usize = 200;
/// Bounds decoding work before an optional trim selects the retained frames.
const MAX_SOURCE_FRAMES: usize = 2000;
const MAX_FRAME_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DURATION_MS: u64 = 10_000;

/// Explicit adjustments for image/GIF preparation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerImageOptions {
    /// Shrink proportionally to fit within the sticker edge limit when the
    /// input is larger. Without it, a larger input is refused.
    #[serde(default)]
    pub fit: bool,
    /// Shrink proportionally so the longest edge is at most this many pixels,
    /// from [`MIN_STICKER_EDGE`] to [`MAX_STICKER_EDGE`]. Never enlarges.
    /// For video this replaces [`DEFAULT_VIDEO_STICKER_EDGE`].
    #[serde(default)]
    pub max_edge: Option<u32>,
    /// Frame rate for a sticker made from video, from 1 to
    /// [`MAX_VIDEO_STICKER_FPS`]; omitted means [`DEFAULT_VIDEO_STICKER_FPS`].
    /// Image and GIF inputs keep their own timing and refuse this option.
    #[serde(default)]
    pub fps: Option<u32>,
    /// Optional explicit animation segment; omitted means preserve all frames.
    pub trim: Option<StickerTrim>,
}
impl StickerImageOptions {
    /// The longest edge an input is shrunk to, when resizing is permitted.
    fn resize_edge(&self) -> Result<Option<u32>> {
        if let Some(edge) = self.max_edge {
            ensure!(
                (MIN_STICKER_EDGE..=MAX_STICKER_EDGE).contains(&edge),
                "sticker_option_invalid"
            );
            return Ok(Some(edge));
        }
        Ok(self.fit.then_some(MAX_STICKER_EDGE))
    }
}
/// A segment requested by the caller, in milliseconds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerTrim {
    /// Start of the retained segment.
    pub start_ms: u32,
    /// Length of the retained segment, greater than zero.
    pub duration_ms: u32,
}
/// Validated GIF plus PNG images suitable for visual model input.
pub struct PreparedStickerImage {
    /// Original GIF when already compliant, otherwise the encoded GIF output.
    pub bytes: Vec<u8>,
    /// PNG containing the first displayed frame, at most 240 pixels on its longest edge.
    pub preview: Vec<u8>,
    /// PNG contact sheet sampling up to six displayed frames, each at most 240 pixels.
    pub contact_sheet: Vec<u8>,
    /// Validated output dimensions and animation information.
    pub inspection: crate::ImageInspection,
    /// Sum of retained frame delays, in milliseconds.
    pub duration_ms: u64,
    /// Changes applied to the input; empty means original GIF bytes were retained.
    pub transformations: Vec<String>,
}

fn png(image: RgbaImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image).write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Shrink to fit within an `edge` square, keeping the aspect ratio.
fn contain(image: &RgbaImage, edge: u32) -> RgbaImage {
    if image.width() <= edge && image.height() <= edge {
        return image.clone();
    }
    DynamicImage::ImageRgba8(image.clone())
        .resize(edge, edge, image::imageops::FilterType::Lanczos3)
        .into_rgba8()
}

/// GIF frame delays in milliseconds and the loop setting, read without
/// decoding pixel data.
fn gif_timeline(bytes: &[u8]) -> Result<(Vec<u64>, gif::Repeat)> {
    let mut options = gif::DecodeOptions::new();
    options.skip_frame_decoding(true);
    let mut reader = options.read_info(Cursor::new(bytes))?;
    let mut delays = Vec::new();
    while let Some(frame) = reader.read_next_frame()? {
        ensure!(delays.len() < MAX_SOURCE_FRAMES, "sticker_frame_limit");
        delays.push(u64::from(frame.delay) * 10);
    }
    Ok((delays, reader.repeat()))
}

/// Select the source frames inside `trim`, with each retained frame's delay
/// cut to the part that overlaps the segment.
fn trim_timeline(delays: &[u64], trim: &StickerTrim) -> Result<Vec<(usize, u64)>> {
    ensure!(
        trim.duration_ms > 0 && u64::from(trim.duration_ms) <= MAX_DURATION_MS,
        "sticker_trim_invalid"
    );
    let begin = u64::from(trim.start_ms);
    let end = begin + u64::from(trim.duration_ms);
    let mut time = 0u64;
    let mut retained = Vec::new();
    for (index, delay) in delays.iter().enumerate() {
        let next = time + delay;
        let overlap = next.min(end).saturating_sub(time.max(begin));
        if overlap > 0 {
            retained.push((index, overlap));
        }
        time = next;
    }
    ensure!(end <= time && !retained.is_empty(), "sticker_trim_invalid");
    Ok(retained)
}

/// Convert a bounded still image or GIF into a validated sticker. Inputs within
/// [`MAX_STICKER_EDGE`] keep their size; larger ones need `fit` or `max_edge`.
/// A trim is applied before the frame and duration limits, which fail rather
/// than silently trimming. Compliant GIFs are preserved byte-for-byte. Frames
/// are decoded one at a time, so memory does not grow with the frame count.
pub fn prepare_sticker_image(
    bytes: &[u8],
    options: &StickerImageOptions,
) -> Result<PreparedStickerImage> {
    let source = crate::inspect_image(bytes)?;
    ensure!(
        source.content_type != "image/x-wxgf",
        "sticker_input_unsupported"
    );
    ensure!(options.fps.is_none(), "sticker_option_unsupported");
    ensure!(
        u64::from(source.width) * u64::from(source.height) <= MAX_FRAME_PIXELS,
        "sticker_decoded_size_limit"
    );
    let is_gif = source.content_type == "image/gif";
    // Animated WebP needs its own timing-preserving decoder; never flatten it.
    ensure!(source.frames <= 1 || is_gif, "sticker_input_unsupported");
    ensure!(options.trim.is_none() || is_gif, "sticker_trim_invalid");
    let resize = match options.resize_edge()? {
        Some(edge) => source.width.max(source.height) > edge,
        None => {
            ensure!(
                source.width.max(source.height) <= MAX_STICKER_EDGE,
                "sticker_adjustment_required"
            );
            false
        }
    };
    let edge = options.resize_edge()?.unwrap_or(MAX_STICKER_EDGE);
    let (delays, repeat) = if is_gif {
        gif_timeline(bytes)?
    } else {
        (vec![0], gif::Repeat::Finite(0))
    };
    let retained = match &options.trim {
        Some(trim) => trim_timeline(&delays, trim)?,
        None => delays.iter().copied().enumerate().collect(),
    };
    ensure!(
        !retained.is_empty() && retained.len() <= MAX_FRAMES,
        "sticker_adjustment_required"
    );
    let duration_ms = retained.iter().map(|(_, delay)| delay).sum::<u64>();
    ensure!(
        duration_ms <= MAX_DURATION_MS,
        "sticker_adjustment_required"
    );
    let mut transformations = Vec::new();
    if options.trim.is_some() {
        transformations.push("trim".to_string());
    }
    if resize {
        transformations.push("resize_contain".into());
    }
    if !is_gif {
        transformations.push("encode_gif".into());
    }
    let preserve = is_gif && !resize && options.trim.is_none();
    let samples = retained.len().min(6);
    let sample_at = |ordinal: usize| {
        if samples == 1 {
            ordinal == 0
        } else {
            (0..samples).any(|sample| sample * (retained.len() - 1) / (samples - 1) == ordinal)
        }
    };
    let mut sampled: Vec<RgbaImage> = Vec::with_capacity(samples);
    let mut output = Vec::new();
    {
        let mut encoder = (!preserve).then(|| GifEncoder::new(&mut output));
        if let Some(encoder) = encoder.as_mut() {
            if is_gif && retained.len() > 1 {
                // Retain GIF loop metadata without flattening finite or
                // once-only sources into infinity.
                match repeat {
                    gif::Repeat::Infinite => encoder.set_repeat(Repeat::Infinite)?,
                    gif::Repeat::Finite(0) => {}
                    gif::Repeat::Finite(count) => encoder.set_repeat(Repeat::Finite(count))?,
                }
            }
        }
        let mut handle = |ordinal: usize, image: RgbaImage, delay_ms: u64| -> Result<()> {
            let image = if resize { contain(&image, edge) } else { image };
            if sample_at(ordinal) {
                sampled.push(contain(&image, PREVIEW_EDGE));
            }
            if let Some(encoder) = encoder.as_mut() {
                encoder.encode_frame(Frame::from_parts(
                    image,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(u32::try_from(delay_ms)?, 1),
                ))?;
            }
            Ok(())
        };
        if is_gif {
            let mut next = retained.iter().enumerate().peekable();
            for (index, frame) in GifDecoder::new(Cursor::new(bytes))?
                .into_frames()
                .enumerate()
            {
                let Some(&(ordinal, &(wanted, delay))) = next.peek() else {
                    break;
                };
                let frame = frame?;
                if index == wanted {
                    handle(ordinal, frame.into_buffer(), delay)?;
                    next.next();
                }
            }
            ensure!(next.peek().is_none(), "sticker_frame_limit");
        } else {
            handle(0, image::load_from_memory(bytes)?.into_rgba8(), 0)?;
        }
    }
    let output = if preserve { bytes.to_vec() } else { output };
    ensure!(
        output.len() <= MAX_STICKER_BYTES,
        "sticker_output_size_limit"
    );
    let inspection = crate::inspect_image(&output)?;
    let cell = sampled.iter().map(RgbaImage::width).max().unwrap_or(1);
    let height = sampled.iter().map(RgbaImage::height).max().unwrap_or(1);
    let mut sheet = RgbaImage::new(cell * sampled.len().max(1) as u32, height);
    for (sample, image) in sampled.iter().enumerate() {
        image::imageops::overlay(&mut sheet, image, i64::from(cell) * sample as i64, 0);
    }
    Ok(PreparedStickerImage {
        bytes: output,
        preview: png(sampled.first().cloned().unwrap_or_default())?,
        contact_sheet: png(sheet)?,
        inspection,
        duration_ms,
        transformations,
    })
}

/// Visual images derived from an original without changing its format or timing.
pub struct StickerPreview {
    /// PNG/JPEG of one representative displayed frame.
    pub still: crate::DecodedImage,
    /// PNG/JPEG of at most six sampled displayed frames.
    pub contact_sheet: crate::DecodedImage,
    /// Number of frames actually decoded and shown in the contact sheet.
    pub sampled_frames: u32,
}
/// Produce bounded visual input. WXGF currently supplies one decoded still;
/// this does not claim that its full animation was watched or sampled.
pub fn sticker_preview(bytes: &[u8]) -> Result<StickerPreview> {
    let info = crate::inspect_image(bytes)?;
    #[cfg(not(target_arch = "wasm32"))]
    if info.content_type == "image/x-wxgf" {
        let still = crate::wxgf_preview(bytes)?;
        let contact_sheet = crate::DecodedImage {
            bytes: still.bytes.clone(),
            inspection: still.inspection.clone(),
        };
        return Ok(StickerPreview {
            still,
            contact_sheet,
            sampled_frames: 1,
        });
    }
    ensure!(
        info.frames <= MAX_FRAMES as u32
            && u64::from(info.width) * u64::from(info.height) * u64::from(info.frames.max(1))
                <= 16 * 1024 * 1024,
        "sticker_preview_size_limit"
    );
    ensure!(
        info.frames <= 1 || info.content_type == "image/gif",
        "sticker_preview_unsupported"
    );
    let frames = if info.content_type == "image/gif" {
        GifDecoder::new(Cursor::new(bytes))?
            .into_frames()
            .collect_frames()?
    } else {
        vec![Frame::new(image::load_from_memory(bytes)?.into_rgba8())]
    };
    ensure!(!frames.is_empty(), "image_no_frames");
    let samples = frames.len().min(6);
    let width = info.width.min(PREVIEW_EDGE);
    let height =
        ((u64::from(info.height) * u64::from(width)) / u64::from(info.width)).max(1) as u32;
    ensure!(height <= 4096, "sticker_preview_size_limit");
    let scaled = |index: usize| {
        image::imageops::resize(
            frames[index].buffer(),
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
    };
    let still_bytes = png(scaled(0))?;
    let still = crate::DecodedImage {
        inspection: crate::inspect_image(&still_bytes)?,
        bytes: still_bytes,
    };
    let mut sheet = RgbaImage::new(width * samples as u32, height);
    for i in 0..samples {
        let index = if samples == 1 {
            0
        } else {
            i * (frames.len() - 1) / (samples - 1)
        };
        image::imageops::replace(&mut sheet, &scaled(index), (i as u32 * width).into(), 0);
    }
    let bytes = png(sheet)?;
    let contact_sheet = crate::DecodedImage {
        inspection: crate::inspect_image(&bytes)?,
        bytes,
    };
    Ok(StickerPreview {
        still,
        contact_sheet,
        sampled_frames: samples as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gif(width: u32, height: u32, count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for n in 0..count {
                let image = RgbaImage::from_pixel(
                    width,
                    height,
                    image::Rgba([(n as u8).wrapping_mul(50), 40, 90, 255]),
                );
                encoder
                    .encode_frame(Frame::from_parts(
                        image,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        bytes
    }
    #[test]
    fn resizing_retains_a_finite_loop_instead_of_making_it_infinite() {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Finite(3)).unwrap();
            for color in [20, 80] {
                encoder
                    .encode_frame(Frame::from_parts(
                        RgbaImage::from_pixel(300, 180, image::Rgba([color, 40, 60, 255])),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let output = prepare_sticker_image(
            &bytes,
            &StickerImageOptions {
                max_edge: Some(120),
                ..Default::default()
            },
        )
        .unwrap();
        let mut decoded = gif::DecodeOptions::new()
            .read_info(Cursor::new(&output.bytes))
            .unwrap();
        while decoded.read_next_frame().unwrap().is_some() {}
        assert_eq!(decoded.repeat(), gif::Repeat::Finite(3));
        assert_eq!(output.duration_ms, 200);
    }
    #[test]
    fn compliant_animations_keep_original_bytes_and_produce_real_visual_previews() {
        let bytes = gif(20, 10, 3);
        let result = prepare_sticker_image(&bytes, &Default::default()).unwrap();
        assert_eq!(result.bytes, bytes);
        assert_eq!(result.duration_ms, 300);
        assert!(result.transformations.is_empty());
        assert_eq!(
            image::load_from_memory(&result.contact_sheet)
                .unwrap()
                .width(),
            60
        );
        assert_ne!(result.preview, result.contact_sheet);
    }
    #[test]
    fn inputs_within_the_edge_limit_keep_their_size_and_bytes() {
        let bytes = gif(480, 240, 2);
        let result = prepare_sticker_image(&bytes, &Default::default()).unwrap();
        assert_eq!(result.bytes, bytes);
        assert_eq!(
            (result.inspection.width, result.inspection.height),
            (480, 240)
        );
        assert!(result.transformations.is_empty());
        // Review images stay small even when the sticker is large.
        let preview = image::load_from_memory(&result.preview).unwrap();
        assert_eq!((preview.width(), preview.height()), (240, 120));
        let sheet = image::load_from_memory(&result.contact_sheet).unwrap();
        assert_eq!((sheet.width(), sheet.height()), (480, 120));
    }
    #[test]
    fn larger_inputs_need_fit_or_a_requested_edge_and_keep_aspect_and_frames() {
        let bytes = gif(1100, 550, 2);
        let error = prepare_sticker_image(&bytes, &Default::default())
            .err()
            .expect("refused");
        assert_eq!(error.to_string(), "sticker_adjustment_required");
        let shape = |options: StickerImageOptions| {
            let result = prepare_sticker_image(&bytes, &options).unwrap();
            assert_eq!(result.duration_ms, 200);
            (
                result.inspection.width,
                result.inspection.height,
                result.inspection.frames,
            )
        };
        assert_eq!(
            shape(StickerImageOptions {
                fit: true,
                ..Default::default()
            }),
            (1024, 512, 2)
        );
        assert_eq!(
            shape(StickerImageOptions {
                max_edge: Some(360),
                ..Default::default()
            }),
            (360, 180, 2)
        );
        for edge in [8, 2000] {
            let options = StickerImageOptions {
                max_edge: Some(edge),
                ..Default::default()
            };
            let error = prepare_sticker_image(&bytes, &options)
                .err()
                .expect("refused");
            assert_eq!(error.to_string(), "sticker_option_invalid");
        }
    }
    #[test]
    fn a_requested_edge_never_enlarges() {
        let bytes = gif(100, 50, 2);
        let options = StickerImageOptions {
            max_edge: Some(360),
            ..Default::default()
        };
        assert_eq!(
            prepare_sticker_image(&bytes, &options).unwrap().bytes,
            bytes
        );
    }
    #[test]
    fn a_trim_is_applied_before_the_frame_and_duration_limits() {
        // 250 frames of 100 ms: too many frames and too long until trimmed.
        let bytes = gif(16, 16, 250);
        let error = prepare_sticker_image(&bytes, &Default::default())
            .err()
            .expect("refused");
        assert_eq!(error.to_string(), "sticker_adjustment_required");
        let options = StickerImageOptions {
            trim: Some(StickerTrim {
                start_ms: 1000,
                duration_ms: 5000,
            }),
            ..Default::default()
        };
        let result = prepare_sticker_image(&bytes, &options).unwrap();
        assert_eq!((result.inspection.frames, result.duration_ms), (50, 5000));
        assert_eq!(result.transformations, vec!["trim"]);
    }
    #[test]
    fn image_sources_refuse_a_frame_rate() {
        let options = StickerImageOptions {
            fps: Some(12),
            ..Default::default()
        };
        let error = prepare_sticker_image(&gif(20, 10, 2), &options)
            .err()
            .expect("refused");
        assert_eq!(error.to_string(), "sticker_option_unsupported");
    }
    #[test]
    fn stills_convert_without_claiming_original_gif_identity() {
        let bytes = png(RgbaImage::new(24, 12)).unwrap();
        let result = prepare_sticker_image(&bytes, &Default::default()).unwrap();
        assert!(result.bytes.starts_with(b"GIF"));
        assert_eq!(result.transformations, vec!["encode_gif"]);
    }
}
