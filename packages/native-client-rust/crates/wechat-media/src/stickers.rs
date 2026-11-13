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
/// Maximum prepared edge length under this application's sticker policy.
pub const MAX_STICKER_EDGE: u32 = 240;
const MAX_FRAMES: usize = 200;
const MAX_DURATION_MS: u64 = 10_000;

/// Explicit adjustments for image/GIF preparation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerImageOptions {
    /// Authorize proportional resizing when an input exceeds the output edge limit.
    #[serde(default)]
    pub fit: bool,
    /// Optional explicit animation segment; omitted means preserve all frames.
    pub trim: Option<StickerTrim>,
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
    /// PNG containing the first displayed frame.
    pub preview: Vec<u8>,
    /// PNG contact sheet sampling up to six displayed frames.
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

/// Convert a bounded still image or GIF into a validated sticker. Oversized
/// dimensions require `fit`; duration/frame limits fail rather than silently
/// trimming. Existing compliant GIFs are preserved byte-for-byte.
pub fn prepare_sticker_image(
    bytes: &[u8],
    options: &StickerImageOptions,
) -> Result<PreparedStickerImage> {
    let source = crate::inspect_image(bytes)?;
    ensure!(
        source.content_type != "image/x-wxgf",
        "sticker_input_unsupported"
    );
    ensure!(
        u64::from(source.width) * u64::from(source.height) * u64::from(source.frames.max(1))
            <= 16 * 1024 * 1024,
        "sticker_decoded_size_limit"
    );
    let is_gif = source.content_type == "image/gif";
    // Animated WebP needs its own timing-preserving decoder; never flatten it.
    ensure!(source.frames <= 1 || is_gif, "sticker_input_unsupported");
    ensure!(
        (source.frames as usize) <= MAX_FRAMES,
        "sticker_adjustment_required"
    );
    let resize = source.width > MAX_STICKER_EDGE || source.height > MAX_STICKER_EDGE;
    ensure!(!resize || options.fit, "sticker_adjustment_required");
    let mut frames = if is_gif {
        GifDecoder::new(Cursor::new(bytes))?
            .into_frames()
            .collect_frames()?
    } else {
        vec![Frame::new(image::load_from_memory(bytes)?.into_rgba8())]
    };
    ensure!(
        !frames.is_empty() && frames.len() <= MAX_FRAMES,
        "sticker_frame_limit"
    );
    let mut transformations = Vec::new();
    if let Some(trim) = &options.trim {
        ensure!(
            is_gif && trim.duration_ms > 0 && u64::from(trim.duration_ms) <= MAX_DURATION_MS,
            "sticker_trim_invalid"
        );
        let begin = u64::from(trim.start_ms);
        let end = begin + u64::from(trim.duration_ms);
        let mut time = 0u64;
        let mut retained = Vec::new();
        for frame in frames {
            let (n, d) = frame.delay().numer_denom_ms();
            ensure!(d > 0, "sticker_timing_invalid");
            let next = time + u64::from(n).div_ceil(u64::from(d));
            let overlap = next.min(end).saturating_sub(time.max(begin));
            if overlap > 0 {
                retained.push(Frame::from_parts(
                    frame.into_buffer(),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(u32::try_from(overlap)?, 1),
                ));
            }
            time = next;
        }
        ensure!(end <= time && !retained.is_empty(), "sticker_trim_invalid");
        frames = retained;
        transformations.push("trim".into());
    }
    let mut duration_ms = 0u64;
    for frame in &mut frames {
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        ensure!(denominator > 0, "sticker_timing_invalid");
        duration_ms = duration_ms
            .checked_add(u64::from(numerator).div_ceil(u64::from(denominator)))
            .ok_or_else(|| anyhow::anyhow!("sticker_duration_limit"))?;
        ensure!(
            duration_ms <= MAX_DURATION_MS,
            "sticker_adjustment_required"
        );
        if resize {
            let delay = frame.delay();
            let resized = DynamicImage::ImageRgba8(frame.buffer().clone())
                .resize(
                    MAX_STICKER_EDGE,
                    MAX_STICKER_EDGE,
                    image::imageops::FilterType::Lanczos3,
                )
                .into_rgba8();
            *frame = Frame::from_parts(resized, 0, 0, delay);
        }
    }
    if resize {
        transformations.push("resize_contain".into());
    }
    if !is_gif {
        transformations.push("encode_gif".into());
    }
    let output = if is_gif && !resize && options.trim.is_none() {
        bytes.to_vec()
    } else {
        let mut output = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut output);
            if is_gif && frames.len() > 1 {
                // A single pass with indexed frames retains GIF loop metadata
                // without flattening finite or once-only sources into infinity.
                let mut metadata = gif::DecodeOptions::new().read_info(Cursor::new(bytes))?;
                while metadata.read_next_frame()?.is_some() {}
                match metadata.repeat() {
                    gif::Repeat::Infinite => encoder.set_repeat(Repeat::Infinite)?,
                    gif::Repeat::Finite(0) => {}
                    gif::Repeat::Finite(count) => encoder.set_repeat(Repeat::Finite(count))?,
                }
            }
            encoder.encode_frames(frames.iter().cloned())?;
        }
        output
    };
    ensure!(
        output.len() <= MAX_STICKER_BYTES,
        "sticker_output_size_limit"
    );
    let inspection = crate::inspect_image(&output)?;
    let count = frames.len().min(6);
    let mut sheet = RgbaImage::new(inspection.width * count as u32, inspection.height);
    for sample in 0..count {
        let index = if count == 1 {
            0
        } else {
            sample * (frames.len() - 1) / (count - 1)
        };
        image::imageops::overlay(
            &mut sheet,
            frames[index].buffer(),
            i64::from(inspection.width) * sample as i64,
            0,
        );
    }
    Ok(PreparedStickerImage {
        bytes: output,
        preview: png(frames[0].buffer().clone())?,
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
    let width = info.width.min(MAX_STICKER_EDGE);
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
                let image =
                    RgbaImage::from_pixel(width, height, image::Rgba([n as u8 * 50, 40, 90, 255]));
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
                fit: true,
                trim: None,
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
    fn resize_requires_consent_and_keeps_aspect_ratio_and_all_frames() {
        let bytes = gif(480, 240, 2);
        assert!(prepare_sticker_image(&bytes, &Default::default()).is_err());
        let result = prepare_sticker_image(
            &bytes,
            &StickerImageOptions {
                fit: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (
                result.inspection.width,
                result.inspection.height,
                result.inspection.frames
            ),
            (240, 120, 2)
        );
        assert_eq!(result.duration_ms, 200);
    }
    #[test]
    fn stills_convert_without_claiming_original_gif_identity() {
        let bytes = png(RgbaImage::new(24, 12)).unwrap();
        let result = prepare_sticker_image(&bytes, &Default::default()).unwrap();
        assert!(result.bytes.starts_with(b"GIF"));
        assert_eq!(result.transformations, vec!["encode_gif"]);
    }
}
