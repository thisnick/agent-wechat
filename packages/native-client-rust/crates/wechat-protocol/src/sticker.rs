//! Original type-47 bytes from the verified CDN host. Signed URLs remain private.
use anyhow::{ensure, Result};
use image::{AnimationDecoder, ImageDecoder};
use serde::{Deserialize, Serialize};
use std::{
    io::{Cursor, Read},
    net::{IpAddr, ToSocketAddrs},
    time::Duration,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerDescriptor {
    pub url: String,
    pub expected_bytes: u64,
    pub expected_md5: String,
    pub expires_at: Option<u64>,
}
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

pub fn inspect_image(bytes: &[u8]) -> Result<ImageInspection> {
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
            ensure!(bytes.last() == Some(&b';'), "gif_trailer_missing");
            let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
            decoder.set_limits(limits())?;
            for frame in decoder.into_frames() {
                count(frame)?;
            }
            "image/gif"
        }
        image::ImageFormat::WebP => {
            ensure!(
                bytes.len() >= 12
                    && u32::from_le_bytes(bytes[4..8].try_into()?) as usize + 8 == bytes.len(),
                "webp_length_invalid"
            );
            let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
            decoder.set_limits(limits())?;
            for frame in decoder.into_frames() {
                count(frame)?;
            }
            "image/webp"
        }
        image::ImageFormat::Jpeg | image::ImageFormat::Png => {
            if format == image::ImageFormat::Jpeg {
                ensure!(jpeg_has_end_marker(bytes), "jpeg_trailer_missing");
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
pub(super) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            super::public_ipv4(v) && !(a == 192 && b == 88 && c == 99)
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] & 0xf000 == 0)
        }
    }
}
pub fn validate_sticker_url(value: &str) -> Result<reqwest::Url> {
    ensure!(
        value.len() <= 8192 && !value.chars().any(|c| c.is_control()),
        "sticker_source_not_allowed"
    );
    let url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("sticker_source_not_allowed"))?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url.host_str() == Some("snsvideo.c2c.wechat.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url
                .port()
                .is_none_or(|p| p == if url.scheme() == "https" { 443 } else { 80 }),
        "sticker_source_not_allowed"
    );
    Ok(url)
}
pub fn fetch_sticker(
    descriptor: &StickerDescriptor,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    let url = validate_sticker_url(&descriptor.url)?;
    ensure!(
        descriptor.expected_bytes > 0
            && descriptor.expected_bytes <= 16 * 1024 * 1024
            && descriptor.expected_md5.len() == 32
            && descriptor
                .expected_md5
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "sticker_metadata_invalid"
    );
    if let Some(expiry) = descriptor.expires_at {
        ensure!(expiry > crate::wire::unix_time()? as u64, "media_expired");
    }
    progress(0, Some(descriptor.expected_bytes))?;
    let port = if url.scheme() == "https" { 443 } else { 80 };
    let addresses = ("snsvideo.c2c.wechat.com", port)
        .to_socket_addrs()
        .map_err(|_| anyhow::anyhow!("sticker_dns_failed"))?
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty()
            && addresses.len() <= 64
            && addresses.iter().all(|a| public_ip(a.ip())),
        "sticker_source_dns_not_public"
    );
    let address = addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.first())
        .unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve("snsvideo.c2c.wechat.com", *address)
        .build()?;
    let mut response = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .send()
        .map_err(|_| anyhow::anyhow!("sticker_fetch_failed"))?;
    ensure!(
        response.status() == 200,
        "sticker_http_status_{}",
        response.status().as_u16()
    );
    ensure!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|v| v.as_bytes().eq_ignore_ascii_case(b"identity")),
        "sticker_content_encoding_refused"
    );
    if let Some(length) = response.content_length() {
        ensure!(length <= 16 * 1024 * 1024, "sticker_response_too_large");
    }
    let mut bytes = vec![];
    let mut chunk = [0u8; 65536];
    loop {
        progress(bytes.len() as u64, Some(descriptor.expected_bytes))?;
        let n = response
            .read(&mut chunk)
            .map_err(|_| anyhow::anyhow!("sticker_fetch_failed"))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "sticker_response_too_large"
        );
    }
    ensure!(
        bytes.len() as u64 == descriptor.expected_bytes
            && hex::encode(crate::crypto::md5(&bytes))
                .eq_ignore_ascii_case(&descriptor.expected_md5),
        "media_integrity_failed"
    );
    let inspection = inspect_image(&bytes)?;
    Ok((bytes, inspection))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_and_network_sources_are_strict() {
        for u in [
            "https://127.0.0.1/x",
            "https://snsvideo.c2c.wechat.com.evil/x",
            "https://user@snsvideo.c2c.wechat.com/x",
            "https://snsvideo.c2c.wechat.com:444/x",
            "https://snsvideo.c2c.wechat.com/x#fragment",
        ] {
            assert!(validate_sticker_url(u).is_err());
        }
        for ip in ["127.0.0.1", "10.0.0.1", "::1", "2001:db8::1"] {
            assert!(!public_ip(ip.parse().unwrap()));
        }
    }
    #[test]
    fn png_is_fully_decoded_and_truncation_fails() {
        let mut bytes = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let good = bytes.into_inner();
        assert_eq!(inspect_image(&good).unwrap().frames, 1);
        assert!(inspect_image(&good[..20]).is_err());
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
        let checksum = crate::crypto::md5(&bytes);
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
