use anyhow::{ensure, Result};
use image::{AnimationDecoder, ImageDecoder};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ImageInspection {
    pub content_type: String,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub animation_preserved: bool,
}

// JPEG segments can contain marker-shaped bytes, and scan data escapes them.
// Walk the framing so an EOI inside metadata cannot disguise truncation. Bytes
// after the actual EOI are permitted; received photos can include a trailer.
fn jpeg_has_end_marker(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut position = 2;
    let mut in_scan = false;
    let mut saw_scan = false;
    while position < bytes.len() {
        if in_scan {
            while bytes.get(position).is_some_and(|b| *b != 0xff) {
                position += 1;
            }
        }
        if bytes.get(position) != Some(&0xff) {
            return false;
        }
        while bytes.get(position) == Some(&0xff) {
            position += 1;
        }
        let Some(&marker) = bytes.get(position) else {
            return false;
        };
        position += 1;
        match marker {
            0x00 | 0xd0..=0xd7 if in_scan => continue,
            0xd9 => return saw_scan,
            0x00 | 0xd0..=0xd8 => return false,
            0x01 => continue,
            _ => {
                let Some(length) = bytes.get(position..position + 2) else {
                    return false;
                };
                let length = u16::from_be_bytes([length[0], length[1]]) as usize;
                if length < 2 || position + length > bytes.len() {
                    return false;
                }
                position += length;
                if marker == 0xda {
                    saw_scan = true;
                    in_scan = true;
                } else if marker != 0xdc {
                    in_scan = false;
                }
            }
        }
    }
    false
}

fn gif_sub_blocks_end(bytes: &[u8], mut position: usize) -> Option<usize> {
    loop {
        let length = *bytes.get(position)? as usize;
        position = position.checked_add(1 + length)?;
        if position > bytes.len() {
            return None;
        }
        if length == 0 {
            return Some(position);
        }
    }
}

// A ';' inside a palette, extension or compressed frame is not the trailer.
// Follow block lengths; any bytes after the actual trailer stay in the source.
fn gif_has_trailer(bytes: &[u8]) -> bool {
    if bytes.len() < 13 || ![b"GIF87a", b"GIF89a"].iter().any(|h| bytes.starts_with(*h)) {
        return false;
    }
    let palette_size = |packed: u8| {
        if packed & 0x80 != 0 {
            3 * (2usize << (packed & 7))
        } else {
            0
        }
    };
    let mut position = 13 + palette_size(bytes[10]);
    while let Some(&block) = bytes.get(position) {
        position += 1;
        match block {
            0x3b => return true,
            0x21 => {
                if bytes.get(position).is_none() {
                    return false;
                }
                position += 1; // Extension label; its contents use sub-blocks.
            }
            0x2c => {
                let Some(descriptor) = bytes.get(position..position + 9) else {
                    return false;
                };
                position += 9 + palette_size(descriptor[8]);
                if bytes.get(position).is_none() {
                    return false;
                }
                position += 1; // LZW minimum code size, before data sub-blocks.
            }
            _ => return false,
        }
        let Some(end) = gif_sub_blocks_end(bytes, position) else {
            return false;
        };
        position = end;
    }
    false
}

fn png_has_end_chunk(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let mut position: usize = 8;
    while let Some(header) = bytes.get(position..position + 8) {
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let Some(end) = position.checked_add(12).and_then(|n| n.checked_add(length)) else {
            return false;
        };
        if end > bytes.len() {
            return false;
        }
        if &header[4..8] == b"IEND" {
            // The decoder can finish the pixels before consuming IEND. Require
            // its zero-length chunk and fixed CRC without trimming the source.
            return length == 0 && bytes[end - 4..end] == [0xae, 0x42, 0x60, 0x82];
        }
        position = end;
    }
    false
}

pub(crate) fn inspect_standard_image(bytes: &[u8]) -> Result<ImageInspection> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= 16 * 1024 * 1024,
        "image_size_limit"
    );
    let format =
        image::guess_format(bytes).map_err(|_| anyhow::anyhow!("image_format_unsupported"))?;
    let limits = || {
        let mut l = image::Limits::default();
        l.max_image_width = Some(4096);
        l.max_image_height = Some(4096);
        l.max_alloc = Some(64 * 1024 * 1024);
        l
    };
    let mut frames = 0u32;
    let mut total = 0u64;
    let mut width = 0;
    let mut height = 0;
    let mut count = |frame: image::ImageResult<image::Frame>| -> Result<()> {
        let f = frame.map_err(|_| anyhow::anyhow!("image_decode_failed"))?;
        let (w, h) = f.buffer().dimensions();
        ensure!(
            w > 0 && h > 0 && w as u64 * h as u64 <= 4_000_000,
            "image_decode_limit"
        );
        total += w as u64 * h as u64;
        frames += 1;
        ensure!(frames <= 500 && total <= 100_000_000, "image_decode_limit");
        width = width.max(w);
        height = height.max(h);
        Ok(())
    };
    let mime = match format {
        image::ImageFormat::Gif => {
            ensure!(gif_has_trailer(bytes), "gif_trailer_missing");
            let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
            decoder.set_limits(limits())?;
            for frame in decoder.into_frames() {
                count(frame)?;
            }
            "image/gif"
        }
        image::ImageFormat::WebP => {
            let encoded_length = bytes
                .get(4..8)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as u64 + 8)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 12 && *n <= bytes.len())
                .ok_or_else(|| anyhow::anyhow!("webp_length_invalid"))?;
            // The RIFF boundary limits decoding, not the retained source bytes.
            // https://developers.google.com/speed/webp/docs/riff_container
            ensure!(encoded_length % 2 == 0, "webp_length_invalid");
            let mut decoder =
                image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes[..encoded_length]))?;
            decoder.set_limits(limits())?;
            if decoder.has_animation() {
                for frame in decoder.into_frames() {
                    count(frame)?;
                }
            } else {
                let (w, h) = decoder.dimensions();
                ensure!(
                    w > 0 && h > 0 && w as u64 * h as u64 <= 4_000_000,
                    "image_decode_limit"
                );
                image::DynamicImage::from_decoder(decoder)
                    .map_err(|_| anyhow::anyhow!("image_decode_failed"))?;
                width = w;
                height = h;
                frames = 1;
            }
            "image/webp"
        }
        image::ImageFormat::Jpeg | image::ImageFormat::Png => {
            if format == image::ImageFormat::Jpeg {
                ensure!(jpeg_has_end_marker(bytes), "jpeg_trailer_missing");
            } else {
                ensure!(png_has_end_chunk(bytes), "png_trailer_missing");
            }
            let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
            reader.limits(limits());
            let decoded = reader
                .decode()
                .map_err(|_| anyhow::anyhow!("image_decode_failed"))?;
            width = decoded.width();
            height = decoded.height();
            ensure!(
                width > 0 && height > 0 && width as u64 * height as u64 <= 16_777_216,
                "image_decode_limit"
            );
            frames = 1;
            if format == image::ImageFormat::Png {
                "image/png"
            } else {
                "image/jpeg"
            }
        }
        _ => anyhow::bail!("image_format_unsupported"),
    };
    ensure!(frames > 0, "image_no_frames");
    Ok(ImageInspection {
        content_type: mime.into(),
        width,
        height,
        frames,
        animation_preserved: frames > 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect_image;
    #[test]
    fn png_is_fully_decoded_and_truncation_fails() {
        let mut bytes = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let good = bytes.into_inner();
        assert_eq!(inspect_image(&good).unwrap().frames, 1);
        for length in [20, good.len() - 12, good.len() - 1] {
            assert!(
                inspect_image(&good[..length]).is_err(),
                "accepted PNG truncated to {length}"
            );
        }
        let mut missing_end = good[..good.len() - 12].to_vec();
        missing_end.extend_from_slice(&12u32.to_be_bytes());
        missing_end.extend_from_slice(b"tEXt");
        missing_end.extend_from_slice(&good[good.len() - 12..]);
        missing_end.extend_from_slice(&[0; 4]);
        assert_eq!(
            inspect_image(&missing_end).unwrap_err().to_string(),
            "png_trailer_missing"
        );
    }

    fn still_image(format: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn non_jpeg_images_with_appended_data_retain_source_bytes() {
        for (format, mime) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Gif, "image/gif"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut bytes = still_image(format);
            bytes.extend_from_slice(b"synthetic-opaque-trailer!");
            let original = bytes.clone();
            let inspection = inspect_image(&bytes).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!(inspection.content_type, mime);
            assert_eq!((inspection.width, inspection.height), (8, 8));
            assert_eq!(bytes, original);
        }
    }

    #[test]
    fn webp_declared_boundary_cannot_use_bytes_outside_riff() {
        let good = still_image(image::ImageFormat::WebP);
        assert_eq!(inspect_image(&good).unwrap().content_type, "image/webp");
        assert!(inspect_image(&good[..good.len() - 1]).is_err());
        let mut short_boundary = good;
        short_boundary[4..8].copy_from_slice(&4u32.to_le_bytes());
        assert!(inspect_image(&short_boundary).is_err());
        short_boundary[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(inspect_image(&short_boundary).is_err());
    }

    #[test]
    fn animated_webp_keeps_all_frames_with_appended_data() {
        let mut bytes = include_bytes!("../tests/fixtures/two-frame.webp").to_vec();
        let plain = inspect_image(&bytes).unwrap();
        assert_eq!(plain.frames, 2);
        assert!(plain.animation_preserved);
        bytes.extend_from_slice(b"extra-source-data");
        let original = bytes.clone();
        let inspected = inspect_image(&bytes).unwrap();
        assert_eq!(inspected.frames, 2);
        assert_eq!((inspected.width, inspected.height), (12, 8));
        assert!(inspected.animation_preserved);
        assert_eq!(bytes, original);
    }

    #[test]
    fn gif_animation_and_real_trailer_are_checked_separately() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .encode_frames([
                    image::Frame::new(image::RgbaImage::from_pixel(
                        8,
                        8,
                        image::Rgba([0, 0, 0, 255]),
                    )),
                    image::Frame::new(image::RgbaImage::from_pixel(
                        8,
                        8,
                        image::Rgba([255, 0, 0, 255]),
                    )),
                ])
                .unwrap();
        }
        let complete_length = bytes.len();
        assert_eq!(bytes.last(), Some(&b';'));
        bytes.extend_from_slice(b"opaque-appended-data");
        let result = inspect_image(&bytes).unwrap();
        assert_eq!(result.frames, 2);
        assert!(result.animation_preserved);

        // A marker-shaped byte in a comment is not the stream's trailer.
        let mut missing_trailer = bytes[..complete_length - 1].to_vec();
        missing_trailer.extend_from_slice(&[0x21, 0xfe, 1, b';', 0]);
        assert_eq!(
            inspect_image(&missing_trailer).unwrap_err().to_string(),
            "gif_trailer_missing"
        );
        // A truncated comment cannot be skipped just because it contains ';'.
        missing_trailer.truncate(missing_trailer.len() - 1);
        assert!(inspect_image(&missing_trailer).is_err());
    }

    fn jpeg() -> Vec<u8> {
        let mut bytes = Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut bytes, image::ImageFormat::Jpeg)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn jpeg_with_trailing_data_decodes_without_changing_source_bytes() {
        let mut bytes = jpeg();
        let checksum = <md5::Md5 as md5::Digest>::digest(&bytes);
        bytes.extend_from_slice(&0x12345678u64.to_le_bytes());
        bytes.extend_from_slice(&checksum);
        let original = bytes.clone();
        let inspected = inspect_image(&bytes).unwrap();
        assert_eq!(inspected.content_type, "image/jpeg");
        assert_eq!(
            (inspected.width, inspected.height, inspected.frames),
            (8, 8, 1)
        );
        assert_eq!(bytes, original);
    }

    #[test]
    fn progressive_jpeg_multiple_scans_allow_trailing_data() {
        let mut bytes = include_bytes!("../tests/fixtures/progressive.jpg").to_vec();
        bytes.extend_from_slice(b"synthetic-photo-trailer!");
        let inspected = inspect_image(&bytes).unwrap();
        assert_eq!((inspected.width, inspected.height), (16, 12));
        assert_eq!(inspected.content_type, "image/jpeg");
        let end = bytes.len() - 24 - 2;
        assert!(!jpeg_has_end_marker(&bytes[..end]));
    }

    #[test]
    fn jpeg_end_marker_inside_metadata_does_not_hide_truncation() {
        let image = jpeg();
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xfe, 0, 6, 0x11, 0xff, 0xd9, 0x22];
        bytes.extend_from_slice(&image[2..]);
        assert!(inspect_image(&bytes).is_ok());
        bytes.truncate(bytes.len() - 2);
        assert_eq!(
            inspect_image(&bytes).unwrap_err().to_string(),
            "jpeg_trailer_missing"
        );
        bytes[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
        bytes.extend_from_slice(&[0xff, 0xd9]);
        assert!(!jpeg_has_end_marker(&bytes));
    }

    #[test]
    fn jpeg_scan_stuffing_and_restart_markers_are_not_end_markers() {
        let bytes = [
            0xff, 0xd8, 0xff, 0xda, 0, 2, 7, 0xff, 0, 0xd9, 0xff, 0xd0, 9, 0xff,
        ];
        assert!(!jpeg_has_end_marker(&bytes));
        let mut complete = bytes.to_vec();
        complete.push(0xd9);
        assert!(jpeg_has_end_marker(&complete));
    }
}
