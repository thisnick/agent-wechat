//! Enterprise WeChat (WeCom) encrypted HTTP media downloads (`tpurl`, `tphdurl`, `tpthumburl`,
//! `tpvideourl`).
//!
//! WeCom media is hosted over HTTPS on approved Enterprise WeChat file hosts, authenticated via
//! the `authkey` request header (`tpauthkey`), and encrypted with AES-128-ECB + PKCS#7 using the
//! message's 16-byte `aeskey` (or `tpthumbaeskey`). The server caps single responses at 512 KiB
//! and returns `206 Partial Content`, so transfers loop over contiguous byte ranges and decrypt
//! directly into a streaming sink.
use super::{
    AsyncDownloadSink, DownloadReceipt, DownloadedMedia, MediaQuality, MAX_CDN_TRANSFER_BYTES,
    MAX_LARGE_FILE_BYTES, MAX_VIDEO_DOWNLOAD_BYTES,
};
use crate::crypto::{Aes128EcbDecryptor, Md5Hasher, Sha256Hasher};
use crate::transport::{Clock, HttpMethod, HttpRequest, HttpTransport};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
#[cfg(not(target_arch = "wasm32"))]
use std::{io::Read, net::SocketAddr};
use zeroize::{Zeroize, Zeroizing};

/// Approved Enterprise WeChat (WeCom) media download hosts.
pub const TP_MEDIA_HOSTS: &[&str] = &["wwfile.work.weixin.qq.com"];

const TP_CHUNK_BYTES: usize = 512 * 1024;
const TP_CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
const TP_READ_STEP: usize = 64 * 1024;
const TP_USER_AGENT: &str = "MicroMessenger Client";
const TP_MIN_CHUNK_BYTES: usize = 64 * 1024;
const TP_MAX_CHUNKS: usize = 16_384;

/// Descriptor for an Enterprise WeChat (WeCom) encrypted HTTP media download.
#[derive(Clone, Serialize, Deserialize)]
pub struct TpMediaDescriptor {
    /// HTTPS download URL from message metadata (`tpurl`, `tphdurl`, `tpthumburl`, or `tpvideourl`).
    pub url: String,
    /// Transfer authorization key (`tpauthkey`) sent in the `authkey` request header.
    pub auth_key: String,
    /// 16-byte AES-128-ECB decryption key (`aeskey` or `tpthumbaeskey`).
    pub aes_key: Vec<u8>,
    /// Requested media quality used to enforce size bounds.
    pub quality: MediaQuality,
    /// Advertised plaintext byte length, when present.
    pub expected_bytes: Option<u64>,
    /// Advertised plaintext MD5 hex digest, when present.
    pub expected_md5: Option<String>,
    /// Whether a mismatch with `expected_bytes` fails the download.
    pub strict_size: bool,
    /// Optional Unix expiry timestamp from message metadata.
    pub expires_at: Option<u64>,
}

impl Drop for TpMediaDescriptor {
    fn drop(&mut self) {
        self.aes_key.zeroize();
        self.auth_key.zeroize();
        self.url.zeroize();
    }
}

impl fmt::Debug for TpMediaDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TpMediaDescriptor")
            .field("quality", &self.quality)
            .field("metadata", &"[redacted]")
            .finish()
    }
}

/// Validate an Enterprise WeChat media URL: HTTPS on an approved host, default port 443,
/// with no credentials, control characters, or fragment.
pub fn validate_tp_media_url(value: &str) -> Result<url::Url> {
    ensure!(
        !value.is_empty() && value.len() <= 8192 && !value.chars().any(|c| c.is_control()),
        "tp_media_source_not_allowed"
    );
    let url = url::Url::parse(value).map_err(|_| anyhow::anyhow!("tp_media_source_not_allowed"))?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some_and(|h| TP_MEDIA_HOSTS.contains(&h))
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.port().is_none_or(|p| p == 443),
        "tp_media_source_not_allowed"
    );
    Ok(url)
}

/// Validate an Enterprise WeChat transfer auth key (`tpauthkey`) before placing it in an HTTP
/// request header.
pub fn validate_tp_auth_key(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 2048
            && value.bytes().all(|b| (0x21..=0x7e).contains(&b)),
        "media_metadata_invalid"
    );
    Ok(())
}

// Legacy blocking WeCom downloaders kept for 3.x native compatibility (#701).
// Prefer `fetch_tp_media_async` and `fetch_tp_media_to_async` for new code.
/// Download and decrypt an Enterprise WeChat media object into memory.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tp_media(
    descriptor: &TpMediaDescriptor,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<DownloadedMedia> {
    let mut bytes = Vec::new();
    let receipt = fetch_tp_media_to(descriptor, progress, &mut bytes)?;
    Ok(DownloadedMedia::from_receipt(receipt, bytes))
}

/// Download and decrypt an Enterprise WeChat media object directly into `sink`, verifying range
/// framing, PKCS#7 padding, size limits, and any advertised length or MD5 digest. Until this
/// returns `Ok`, bytes written to `sink` are unverified and must be discarded on error.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tp_media_to(
    descriptor: &TpMediaDescriptor,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    sink: &mut dyn std::io::Write,
) -> Result<DownloadReceipt> {
    let url = validate_tp_media_url(&descriptor.url)?;
    validate_tp_auth_key(&descriptor.auth_key)?;
    let address = super::link_thumbnail::public_address(
        &url,
        "tp_media_dns_failed",
        "tp_media_dns_not_public",
    )?;
    fetch_tp_media_at(url, address, descriptor, progress, sink)
}

/// Asynchronously download and decrypt an Enterprise WeChat media object into
/// memory using a pluggable [`HttpTransport`] and [`Clock`].
///
/// The caller's [`HttpTransport`] must not follow HTTP redirects and is
/// responsible for network/SSRF boundaries.
/// On native targets, `transport::NativeHttpTransport` built with
/// `DestinationPolicy::PublicOnly` provides that boundary.
pub async fn fetch_tp_media_async(
    descriptor: &TpMediaDescriptor,
    http: &dyn HttpTransport,
    clock: &dyn Clock,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<DownloadedMedia> {
    let mut bytes = Vec::new();
    let receipt = fetch_tp_media_to_async(descriptor, http, clock, progress, &mut bytes).await?;
    Ok(DownloadedMedia::from_receipt(receipt, bytes))
}

/// Asynchronously download and decrypt an Enterprise WeChat media object
/// directly into `sink`, verifying range framing, PKCS#7 padding, size limits,
/// and any advertised length or MD5 digest. Until this returns `Ok`, bytes
/// written to `sink` are unverified and must be discarded on error.
///
/// The caller's [`HttpTransport`] must not follow HTTP redirects and is
/// responsible for network/SSRF boundaries (on native targets,
/// `transport::NativeHttpTransport` built with
/// `DestinationPolicy::PublicOnly`). Because [`HttpTransport::send`]
/// buffers each response up to `max_response_bytes` before status and headers
/// are inspected, chunk 0 sets `max_response_bytes` to the larger of 512 KiB
/// and the object's ciphertext limit (so a server that ignores `Range` with a
/// whole-body `200 OK` still works), while later chunks cap at 512 KiB. If a
/// server ignores `Range` with a body exceeding that receive budget—for example
/// a strict-size mismatch above `ciphertext_limit` or a `200 OK` on a later
/// chunk—a budget-enforcing transport fails closed with
/// [`crate::transport::TransportError::ResponseTooLarge`], mapped here to
/// `"tp_media_size_limit"` rather than `"Advertised media size mismatch"` or
/// `"media_integrity_failed"`. Similarly, a misbehaving `206` on chunk 0 can be
/// buffered up to chunk 0's receive budget before the range check rejects it.
pub async fn fetch_tp_media_to_async(
    descriptor: &TpMediaDescriptor,
    http: &dyn HttpTransport,
    clock: &dyn Clock,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    sink: &mut (impl AsyncDownloadSink + ?Sized),
) -> Result<DownloadReceipt> {
    let url = validate_tp_media_url(&descriptor.url)?;
    validate_tp_auth_key(&descriptor.auth_key)?;
    ensure!(descriptor.aes_key.len() == 16, "media_metadata_invalid");
    if let Some(expected) = descriptor.expected_md5.as_deref() {
        ensure!(
            expected.len() == 32 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
            "media_metadata_invalid"
        );
    }
    if let Some(expiry) = descriptor.expires_at {
        ensure!(expiry > u64::from(clock.unix_time_secs()?), "media_expired");
    }
    let limit = plaintext_limit(descriptor.quality);
    let max_ciphertext_limit = (limit / 16 + 1) * 16;
    let expected_ciphertext = if let Some(expected) = descriptor.expected_bytes {
        ensure!(expected <= limit as u64, "tp_media_size_limit");
        if descriptor.strict_size {
            ensure!(expected > 0, "media_metadata_invalid");
            let expected_usize =
                usize::try_from(expected).map_err(|_| anyhow::anyhow!("tp_media_size_limit"))?;
            Some((expected_usize / 16 + 1) * 16)
        } else {
            None
        }
    } else {
        None
    };
    let ciphertext_limit = expected_ciphertext.unwrap_or(max_ciphertext_limit);
    let check_total = |size: usize| -> Result<()> {
        ensure!(
            size > 0 && size.is_multiple_of(16),
            "media_integrity_failed"
        );
        ensure!(size <= max_ciphertext_limit, "tp_media_size_limit");
        if let Some(expected) = expected_ciphertext {
            ensure!(size == expected, "Advertised media size mismatch");
        }
        Ok(())
    };

    let mut decryptor = Aes128EcbDecryptor::new(&descriptor.aes_key)?;
    let mut plain = Zeroizing::new(vec![0u8; TP_READ_STEP + 16]);
    let mut plaintext_bytes = 0usize;
    let mut plaintext_md5 = Md5Hasher::new()?;
    let mut plaintext_sha256 = Sha256Hasher::new()?;

    let mut position = 0usize;
    let mut total = 0usize;
    let max_chunks = ciphertext_limit
        .div_ceil(TP_MIN_CHUNK_BYTES)
        .clamp(16, TP_MAX_CHUNKS);

    for _ in 0..max_chunks {
        progress(
            position as u64,
            if total == 0 { None } else { Some(total as u64) },
        )?;
        let requested_end = if total == 0 {
            TP_CHUNK_BYTES - 1
        } else {
            (total - 1).min(position + TP_CHUNK_BYTES - 1)
        };
        let max_response_bytes = if position == 0 {
            ciphertext_limit.max(TP_CHUNK_BYTES)
        } else {
            TP_CHUNK_BYTES
        };
        let response = http
            .send(HttpRequest {
                method: HttpMethod::Get,
                url: url.clone(),
                headers: vec![
                    ("Accept-Encoding".into(), "identity".into()),
                    ("User-Agent".into(), TP_USER_AGENT.into()),
                    ("authkey".into(), descriptor.auth_key.clone()),
                    ("Range".into(), format!("bytes={position}-{requested_end}")),
                ],
                body: Vec::new(),
                timeout: TP_CHUNK_TIMEOUT,
                max_response_bytes,
            })
            .await
            .map_err(|error| {
                super::download_failure(
                    &error,
                    "tp_media_size_limit",
                    "tp_media_dns_not_public",
                    "tp_media_fetch_failed",
                )
            })?;

        let status = response.status;
        if status != 200 && status != 206 {
            bail!("CDN HTTP status {status}");
        }
        for errno in response.header_values("x-errno") {
            ensure!(errno.trim() == "0", "tp_media_fetch_failed");
        }
        for encoding in response.header_values("content-encoding") {
            ensure!(
                encoding.trim().eq_ignore_ascii_case("identity"),
                "tp_media_encoding_refused"
            );
        }

        let mut content_range = None;
        for raw in response.header_values("content-range") {
            ensure!(content_range.is_none(), "media_integrity_failed");
            content_range = Some(parse_content_range(raw)?);
        }
        let content_length = response
            .content_length()
            .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;

        let (expected_chunk_len, whole_body) = if status == 206 {
            let (start, end, size) =
                content_range.ok_or_else(|| anyhow::anyhow!("media_integrity_failed"))?;
            check_total(size)?;
            ensure!(
                start == position && end <= requested_end && (total == 0 || total == size),
                "media_integrity_failed"
            );
            let chunk_len = end - start + 1;
            if let Some(cl) = content_length {
                ensure!(cl == chunk_len as u64, "media_integrity_failed");
            }
            total = size;
            (Some(chunk_len), false)
        } else {
            ensure!(position == 0, "media_integrity_failed");
            if let Some((start, end, size)) = content_range {
                check_total(size)?;
                ensure!(start == 0 && end + 1 == size, "media_integrity_failed");
                if let Some(cl) = content_length {
                    ensure!(cl == size as u64, "media_integrity_failed");
                }
                total = size;
                (Some(size), true)
            } else if let Some(cl) = content_length {
                let cl_usize =
                    usize::try_from(cl).map_err(|_| anyhow::anyhow!("tp_media_size_limit"))?;
                check_total(cl_usize)?;
                total = cl_usize;
                (Some(cl_usize), true)
            } else {
                (None, true)
            }
        };

        let chunk_received = response.body.len();
        ensure!(chunk_received > 0, "media_integrity_failed");
        if let Some(expected) = expected_chunk_len {
            ensure!(chunk_received == expected, "media_integrity_failed");
        }
        let next_pos = position
            .checked_add(chunk_received)
            .ok_or_else(|| anyhow::anyhow!("tp_media_size_limit"))?;
        ensure!(next_pos <= max_ciphertext_limit, "tp_media_size_limit");
        if let Some(expected) = expected_ciphertext {
            ensure!(next_pos <= expected, "Advertised media size mismatch");
        }
        let mut cursor = position;
        for slice in response.body.chunks(TP_READ_STEP) {
            let count = decryptor
                .update(slice, &mut plain)
                .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;
            plaintext_bytes += count;
            ensure!(plaintext_bytes <= limit, "tp_media_size_limit");
            plaintext_md5.update(&plain[..count])?;
            plaintext_sha256.update(&plain[..count])?;
            if count > 0 {
                sink.write_all(&plain[..count]).await?;
            }
            cursor += slice.len();
            progress(
                cursor as u64,
                if total == 0 { None } else { Some(total as u64) },
            )?;
        }

        position = next_pos;
        if whole_body {
            total = position;
            break;
        }
        if position == total {
            break;
        }
        ensure!(chunk_received >= 16, "media_integrity_failed");
    }

    ensure!(
        total > 0 && position == total && total.is_multiple_of(16) && total <= ciphertext_limit,
        "media_integrity_failed"
    );
    let count = decryptor
        .finalize(&mut plain)
        .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;
    plaintext_bytes += count;
    ensure!(plaintext_bytes > 0, "media_integrity_failed");
    ensure!(plaintext_bytes <= limit, "tp_media_size_limit");
    plaintext_md5.update(&plain[..count])?;
    plaintext_sha256.update(&plain[..count])?;
    if count > 0 {
        sink.write_all(&plain[..count]).await?;
    }
    sink.flush().await?;

    let size_matches = descriptor
        .expected_bytes
        .map(|expected| expected == plaintext_bytes as u64);
    ensure!(
        !(descriptor.strict_size && size_matches == Some(false)),
        "Advertised media size mismatch"
    );
    let md5_hex = hex::encode(plaintext_md5.finish()?);
    if let Some(expected) = descriptor.expected_md5.as_deref() {
        ensure!(
            md5_hex.eq_ignore_ascii_case(expected),
            "Advertised media checksum mismatch"
        );
    }
    let sha256_hex = hex::encode(plaintext_sha256.finish()?);
    Ok(DownloadReceipt {
        byte_length: plaintext_bytes as u64,
        requested_quality: descriptor.quality,
        substituted_filetype: None,
        picture_format: None,
        md5: md5_hex,
        sha256: sha256_hex,
        advertised_size_matches: size_matches,
    })
}

fn plaintext_limit(quality: MediaQuality) -> usize {
    match quality {
        MediaQuality::LargeFile => MAX_LARGE_FILE_BYTES,
        MediaQuality::Video => MAX_VIDEO_DOWNLOAD_BYTES,
        MediaQuality::Original
        | MediaQuality::Standard
        | MediaQuality::Thumbnail
        | MediaQuality::File => MAX_CDN_TRANSFER_BYTES,
    }
}

fn parse_content_range(value: &str) -> Result<(usize, usize, usize)> {
    let trimmed = value.trim();
    let rest = trimmed
        .as_bytes()
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case(b"bytes "))
        .map(|_| &trimmed[6..])
        .ok_or_else(|| anyhow::anyhow!("media_integrity_failed"))?;
    let (span, total_str) = rest
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("media_integrity_failed"))?;
    let (start_str, end_str) = span
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("media_integrity_failed"))?;
    let parse_dec = |s: &str| -> Result<usize> {
        ensure!(
            !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()),
            "media_integrity_failed"
        );
        s.parse::<usize>()
            .map_err(|_| anyhow::anyhow!("media_integrity_failed"))
    };
    let start = parse_dec(start_str)?;
    let end = parse_dec(end_str)?;
    let total = parse_dec(total_str)?;
    ensure!(start <= end && end < total, "media_integrity_failed");
    Ok((start, end, total))
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_tp_media_at(
    url: url::Url,
    address: SocketAddr,
    descriptor: &TpMediaDescriptor,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    sink: &mut dyn std::io::Write,
) -> Result<DownloadReceipt> {
    validate_tp_auth_key(&descriptor.auth_key)?;
    ensure!(descriptor.aes_key.len() == 16, "media_metadata_invalid");
    if let Some(expected) = descriptor.expected_md5.as_deref() {
        ensure!(
            expected.len() == 32 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
            "media_metadata_invalid"
        );
    }
    if let Some(expiry) = descriptor.expires_at {
        ensure!(expiry > crate::wire::unix_time()? as u64, "media_expired");
    }
    let limit = plaintext_limit(descriptor.quality);
    let max_ciphertext_limit = (limit / 16 + 1) * 16;
    let expected_ciphertext = if let Some(expected) = descriptor.expected_bytes {
        ensure!(expected <= limit as u64, "tp_media_size_limit");
        if descriptor.strict_size {
            ensure!(expected > 0, "media_metadata_invalid");
            let expected_usize =
                usize::try_from(expected).map_err(|_| anyhow::anyhow!("tp_media_size_limit"))?;
            Some((expected_usize / 16 + 1) * 16)
        } else {
            None
        }
    } else {
        None
    };
    let ciphertext_limit = expected_ciphertext.unwrap_or(max_ciphertext_limit);
    let check_total = |size: usize| -> Result<()> {
        ensure!(
            size > 0 && size.is_multiple_of(16),
            "media_integrity_failed"
        );
        ensure!(size <= max_ciphertext_limit, "tp_media_size_limit");
        if let Some(expected) = expected_ciphertext {
            ensure!(size == expected, "Advertised media size mismatch");
        }
        Ok(())
    };

    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("tp_media_source_not_allowed"))?
        .to_owned();
    let client = reqwest::blocking::Client::builder()
        .timeout(TP_CHUNK_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve(&host, address)
        .build()?;

    let mut decryptor = Aes128EcbDecryptor::new(&descriptor.aes_key)?;
    let mut plain = Zeroizing::new(vec![0u8; TP_READ_STEP + 16]);
    let mut buf = [0u8; TP_READ_STEP];
    let mut plaintext_bytes = 0usize;
    let mut plaintext_md5 = Md5Hasher::new()?;
    let mut plaintext_sha256 = Sha256Hasher::new()?;

    let mut position = 0usize;
    let mut total = 0usize;
    let max_chunks = ciphertext_limit
        .div_ceil(TP_MIN_CHUNK_BYTES)
        .clamp(16, TP_MAX_CHUNKS);

    for _ in 0..max_chunks {
        progress(
            position as u64,
            if total == 0 { None } else { Some(total as u64) },
        )?;
        let requested_end = if total == 0 {
            TP_CHUNK_BYTES - 1
        } else {
            (total - 1).min(position + TP_CHUNK_BYTES - 1)
        };
        let mut response = client
            .get(url.clone())
            .header("Accept-Encoding", "identity")
            .header("User-Agent", TP_USER_AGENT)
            .header("authkey", &descriptor.auth_key)
            .header("Range", format!("bytes={position}-{requested_end}"))
            .send()
            .map_err(|_| anyhow::anyhow!("tp_media_fetch_failed"))?;

        let status = response.status().as_u16();
        if status != 200 && status != 206 {
            bail!("CDN HTTP status {status}");
        }
        for value in response.headers().get_all("x-errno") {
            let errno = value
                .to_str()
                .map_err(|_| anyhow::anyhow!("tp_media_fetch_failed"))?;
            ensure!(errno.trim() == "0", "tp_media_fetch_failed");
        }
        for value in response.headers().get_all("content-encoding") {
            ensure!(
                value.as_bytes().eq_ignore_ascii_case(b"identity"),
                "tp_media_encoding_refused"
            );
        }

        let mut content_range = None;
        for value in response.headers().get_all("content-range") {
            ensure!(content_range.is_none(), "media_integrity_failed");
            let parsed = value
                .to_str()
                .map_err(|_| anyhow::anyhow!("media_integrity_failed"))
                .and_then(parse_content_range)?;
            content_range = Some(parsed);
        }
        let content_length = response.content_length();

        let (expected_chunk_len, whole_body) = if status == 206 {
            let (start, end, size) =
                content_range.ok_or_else(|| anyhow::anyhow!("media_integrity_failed"))?;
            check_total(size)?;
            ensure!(
                start == position && end <= requested_end && (total == 0 || total == size),
                "media_integrity_failed"
            );
            let chunk_len = end - start + 1;
            if let Some(cl) = content_length {
                ensure!(cl == chunk_len as u64, "media_integrity_failed");
            }
            total = size;
            (Some(chunk_len), false)
        } else {
            // 200 OK is only valid for the start of the transfer and delivers the whole body.
            ensure!(position == 0, "media_integrity_failed");
            if let Some((start, end, size)) = content_range {
                check_total(size)?;
                ensure!(start == 0 && end + 1 == size, "media_integrity_failed");
                if let Some(cl) = content_length {
                    ensure!(cl == size as u64, "media_integrity_failed");
                }
                total = size;
                (Some(size), true)
            } else if let Some(cl) = content_length {
                let cl_usize =
                    usize::try_from(cl).map_err(|_| anyhow::anyhow!("tp_media_size_limit"))?;
                check_total(cl_usize)?;
                total = cl_usize;
                (Some(cl_usize), true)
            } else {
                (None, true)
            }
        };

        let mut chunk_received = 0usize;
        loop {
            let n = response
                .read(&mut buf)
                .map_err(|_| anyhow::anyhow!("tp_media_fetch_failed"))?;
            if n == 0 {
                break;
            }
            chunk_received = chunk_received
                .checked_add(n)
                .ok_or_else(|| anyhow::anyhow!("tp_media_size_limit"))?;
            if let Some(expected) = expected_chunk_len {
                ensure!(chunk_received <= expected, "media_integrity_failed");
            }
            let next_pos = position
                .checked_add(chunk_received)
                .ok_or_else(|| anyhow::anyhow!("tp_media_size_limit"))?;
            ensure!(next_pos <= max_ciphertext_limit, "tp_media_size_limit");
            if let Some(expected) = expected_ciphertext {
                ensure!(next_pos <= expected, "Advertised media size mismatch");
            }
            let count = decryptor
                .update(&buf[..n], &mut plain)
                .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;
            plaintext_bytes += count;
            ensure!(plaintext_bytes <= limit, "tp_media_size_limit");
            plaintext_md5.update(&plain[..count])?;
            plaintext_sha256.update(&plain[..count])?;
            sink.write_all(&plain[..count])?;
            progress(
                next_pos as u64,
                if total == 0 { None } else { Some(total as u64) },
            )?;
        }

        ensure!(chunk_received > 0, "media_integrity_failed");
        if let Some(expected) = expected_chunk_len {
            ensure!(chunk_received == expected, "media_integrity_failed");
        }
        position += chunk_received;
        if whole_body {
            total = position;
            break;
        }
        if position == total {
            break;
        }
        // Prevent degenerate servers from trickling tiny chunks indefinitely.
        ensure!(chunk_received >= 16, "media_integrity_failed");
    }

    ensure!(
        total > 0 && position == total && total.is_multiple_of(16) && total <= ciphertext_limit,
        "media_integrity_failed"
    );
    let count = decryptor
        .finalize(&mut plain)
        .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;
    plaintext_bytes += count;
    ensure!(plaintext_bytes > 0, "media_integrity_failed");
    ensure!(plaintext_bytes <= limit, "tp_media_size_limit");
    plaintext_md5.update(&plain[..count])?;
    plaintext_sha256.update(&plain[..count])?;
    sink.write_all(&plain[..count])?;
    sink.flush()?;

    let size_matches = descriptor
        .expected_bytes
        .map(|expected| expected == plaintext_bytes as u64);
    ensure!(
        !(descriptor.strict_size && size_matches == Some(false)),
        "Advertised media size mismatch"
    );
    let md5_hex = hex::encode(plaintext_md5.finish()?);
    if let Some(expected) = descriptor.expected_md5.as_deref() {
        ensure!(
            md5_hex.eq_ignore_ascii_case(expected),
            "Advertised media checksum mismatch"
        );
    }
    let sha256_hex = hex::encode(plaintext_sha256.finish()?);
    Ok(DownloadReceipt {
        byte_length: plaintext_bytes as u64,
        requested_quality: descriptor.quality,
        substituted_filetype: None,
        picture_format: None,
        md5: md5_hex,
        sha256: sha256_hex,
        advertised_size_matches: size_matches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    fn synthetic_descriptor(
        url: &str,
        key: &[u8],
        quality: MediaQuality,
        expected_bytes: Option<u64>,
        expected_md5: Option<String>,
        strict_size: bool,
    ) -> TpMediaDescriptor {
        TpMediaDescriptor {
            url: url.into(),
            auth_key: "0123456789abcdef".into(),
            aes_key: key.to_vec(),
            quality,
            expected_bytes,
            expected_md5,
            strict_size,
            expires_at: None,
        }
    }

    fn encrypt_ecb(plain: &[u8], key: &[u8]) -> Vec<u8> {
        crate::crypto::aes_128_ecb_encrypt(key, plain).unwrap()
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        String::from_utf8(request).unwrap()
    }

    #[test]
    fn url_and_auth_key_validation_are_strict() {
        assert!(validate_tp_media_url(
            "https://wwfile.work.weixin.qq.com/cgi-bin/download?f=synthetic&t=token&p=2"
        )
        .is_ok());
        assert!(validate_tp_media_url(
            "https://wwfile.work.weixin.qq.com:443/cgi-bin/download?f=synthetic"
        )
        .is_ok());
        for bad in [
            "",
            "http://wwfile.work.weixin.qq.com/cgi-bin/download?f=synthetic",
            "https://wwfile.work.weixin.qq.com.evil/cgi-bin/download",
            "https://evil.work.weixin.qq.com/cgi-bin/download",
            "https://127.0.0.1/cgi-bin/download",
            "https://user@wwfile.work.weixin.qq.com/cgi-bin/download",
            "https://wwfile.work.weixin.qq.com:8443/cgi-bin/download",
            "https://wwfile.work.weixin.qq.com/cgi-bin/download#frag",
            "https://wwfile.work.weixin.qq.com/cgi-bin/download\r\nX: y",
        ] {
            assert_eq!(
                validate_tp_media_url(bad).unwrap_err().to_string(),
                "tp_media_source_not_allowed",
                "{bad}"
            );
        }

        assert!(validate_tp_auth_key("3044020100043d").is_ok());
        for bad_key in ["", "a b", "abc\r\ndef", "abc\ndef", &"a".repeat(2049)] {
            assert_eq!(
                validate_tp_auth_key(bad_key).unwrap_err().to_string(),
                "media_metadata_invalid"
            );
        }
    }

    #[test]
    fn debug_redacts_secrets() {
        let descriptor = synthetic_descriptor(
            "https://wwfile.work.weixin.qq.com/cgi-bin/download?f=secret-file&t=secret-token",
            &[0xaa; 16],
            MediaQuality::Standard,
            Some(10),
            None,
            false,
        );
        let debug = format!("{descriptor:?}");
        assert!(!debug.contains("secret-file"));
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("0123456789abcdef"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn single_200_and_multi_chunk_206_stream_and_decrypt_with_authkey_header() {
        let key = [0x42u8; 16];
        // Test 1: small payload returned as 200 OK with CONTENT-RANGE (matching live server).
        let small_plain = b"synthetic-small-wecom-attachment-payload".to_vec();
        let small_cipher = encrypt_ecb(&small_plain, &key);
        let small_md5 = hex::encode(crate::crypto::md5(&small_plain));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = small_cipher.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let req = read_http_request(&mut stream);
            assert!(req
                .to_ascii_lowercase()
                .contains("authkey: 0123456789abcdef\r\n"));
            assert!(req
                .to_ascii_lowercase()
                .contains("range: bytes=0-524287\r\n"));
            let head = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nCONTENT-RANGE: bytes 0-{}/{}\r\n\r\n",
                body.len(),
                body.len() - 1,
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        let url = url::Url::parse(&format!(
            "http://wwfile.example.invalid:{}/cgi-bin/download?f=a&p=5",
            address.port()
        ))
        .unwrap();
        let desc = synthetic_descriptor(
            url.as_str(),
            &key,
            MediaQuality::File,
            Some(small_plain.len() as u64),
            Some(small_md5.clone()),
            true,
        );
        let mut out = Vec::new();
        let receipt = fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut out).unwrap();
        server.join().unwrap();
        assert_eq!(out, small_plain);
        assert_eq!(receipt.byte_length, small_plain.len() as u64);
        assert_eq!(receipt.md5, small_md5);
        assert_eq!(receipt.advertised_size_matches, Some(true));

        // Test 2: payload > 512 KiB spanning 3 ranged 206 chunks (matching live 1.37 MB image).
        let large_plain: Vec<u8> = (0..(TP_CHUNK_BYTES * 2 + 12_345))
            .map(|i| (i % 251) as u8)
            .collect();
        let large_cipher = encrypt_ecb(&large_plain, &key);
        let large_md5 = hex::encode(crate::crypto::md5(&large_plain));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cipher_for_server = large_cipher.clone();
        let server = std::thread::spawn(move || {
            let total = cipher_for_server.len();
            let mut served = 0usize;
            let mut requests = 0usize;
            while served < total {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let req = read_http_request(&mut stream);
                assert!(req
                    .to_ascii_lowercase()
                    .contains("authkey: 0123456789abcdef\r\n"));
                let end = (served + TP_CHUNK_BYTES - 1).min(total - 1);
                let expected_range = format!("range: bytes={served}-{end}\r\n");
                assert!(
                    req.to_ascii_lowercase().contains(&expected_range),
                    "missing {expected_range} in {req}"
                );
                let slice = &cipher_for_server[served..=end];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nContent-Range: bytes {served}-{end}/{total}\r\n\r\n",
                    slice.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(slice).unwrap();
                served = end + 1;
                requests += 1;
            }
            requests
        });
        let url = url::Url::parse(&format!(
            "http://wwfile.example.invalid:{}/cgi-bin/download?f=b&p=2",
            address.port()
        ))
        .unwrap();
        let desc = synthetic_descriptor(
            url.as_str(),
            &key,
            MediaQuality::Standard,
            Some(large_plain.len() as u64),
            Some(large_md5.clone()),
            false,
        );
        let mut out = Vec::new();
        let mut max_seen = 0u64;
        let receipt = fetch_tp_media_at(
            url,
            address,
            &desc,
            |done, _| {
                max_seen = max_seen.max(done);
                Ok(())
            },
            &mut out,
        )
        .unwrap();
        let requests = server.join().unwrap();
        assert_eq!(requests, 3);
        assert_eq!(out, large_plain);
        assert_eq!(receipt.byte_length, large_plain.len() as u64);
        assert_eq!(receipt.md5, large_md5);
        assert_eq!(max_seen, large_cipher.len() as u64);
    }

    #[test]
    fn rejects_redirects_bad_encoding_nonzero_x_errno_wrong_key_and_range_tampering() {
        let key = [0x11u8; 16];
        let plain = b"synthetic-bytes-for-error-tests".to_vec();
        let cipher = encrypt_ecb(&plain, &key);
        let good_md5 = hex::encode(crate::crypto::md5(&plain));

        for (status, headers, body, use_key, expected_md5, expected_err) in [
            (
                "302 Found",
                "Location: http://127.0.0.1/x\r\nContent-Length: 0\r\n".to_string(),
                vec![],
                key,
                Some(good_md5.clone()),
                "CDN HTTP status 302",
            ),
            (
                "403 Forbidden",
                "Content-Length: 0\r\n".to_string(),
                vec![],
                key,
                Some(good_md5.clone()),
                "CDN HTTP status 403",
            ),
            (
                "200 OK",
                format!(
                    "Content-Encoding: gzip\r\nContent-Length: {}\r\n",
                    cipher.len()
                ),
                cipher.clone(),
                key,
                Some(good_md5.clone()),
                "tp_media_encoding_refused",
            ),
            (
                "200 OK",
                format!("X-Errno: -1001\r\nContent-Length: {}\r\n", cipher.len()),
                cipher.clone(),
                key,
                Some(good_md5.clone()),
                "tp_media_fetch_failed",
            ),
            (
                "206 Partial Content",
                format!("Content-Length: {}\r\n", cipher.len()),
                cipher.clone(),
                key,
                Some(good_md5.clone()),
                "media_integrity_failed",
            ),
            (
                "200 OK",
                format!("Content-Length: {}\r\n", cipher.len()),
                cipher.clone(),
                [0x22u8; 16],
                Some(good_md5.clone()),
                "media_integrity_failed",
            ),
            (
                "200 OK",
                format!("Content-Length: {}\r\n", cipher.len()),
                cipher.clone(),
                key,
                Some("00000000000000000000000000000000".into()),
                "Advertised media checksum mismatch",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let _ = read_http_request(&mut stream);
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
                );
                let _ = stream.write_all(&body);
            });
            let url = url::Url::parse(&format!(
                "http://wwfile.example.invalid:{}/cgi-bin/download?f=e",
                address.port()
            ))
            .unwrap();
            let desc = synthetic_descriptor(
                url.as_str(),
                &use_key,
                MediaQuality::File,
                Some(plain.len() as u64),
                expected_md5,
                true,
            );
            let err =
                fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut Vec::new()).unwrap_err();
            server.join().unwrap();
            assert_eq!(err.to_string(), expected_err);
        }

        // Changing total size between 206 chunks must fail.
        let big_plain = vec![0x55u8; TP_CHUNK_BYTES + 100];
        let big_cipher = encrypt_ecb(&big_plain, &key);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let c1 = big_cipher[..TP_CHUNK_BYTES].to_vec();
        let total1 = big_cipher.len();
        let server = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let _ = read_http_request(&mut stream);
                if idx == 0 {
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: {}\r\nContent-Range: bytes 0-{}/{total1}\r\n\r\n",
                        c1.len(),
                        c1.len() - 1
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&c1);
                } else {
                    let altered_total = total1 + 16;
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: 16\r\nContent-Range: bytes {}-{}/{altered_total}\r\n\r\n",
                        TP_CHUNK_BYTES,
                        TP_CHUNK_BYTES + 15
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&[0u8; 16]);
                }
            }
        });
        let url = url::Url::parse(&format!(
            "http://wwfile.example.invalid:{}/cgi-bin/download?f=tamper",
            address.port()
        ))
        .unwrap();
        let desc = synthetic_descriptor(url.as_str(), &key, MediaQuality::File, None, None, false);
        let err =
            fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut Vec::new()).unwrap_err();
        server.join().unwrap();
        assert_eq!(err.to_string(), "media_integrity_failed");
    }

    #[test]
    fn handles_unaligned_206_chunks_and_rejects_header_framing_bounds_and_cancel_edge_cases() {
        let key = [0x33u8; 16];
        let plain: Vec<u8> = (0..123).map(|i| (i * 7 + 3) as u8).collect();
        let cipher = encrypt_ecb(&plain, &key);
        let md5 = hex::encode(crate::crypto::md5(&plain));

        // 1. Non-16-byte-aligned 206 chunks (37 + 19 + rest) decrypt cleanly across block boundaries.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cipher_copy = cipher.clone();
        let server = std::thread::spawn(move || {
            let total = cipher_copy.len();
            let cuts = [37usize, 19, total - 56];
            let mut offset = 0usize;
            for len in cuts {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let _ = read_http_request(&mut stream);
                let end = offset + len - 1;
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: {len}\r\nContent-Range: bytes {offset}-{end}/{total}\r\n\r\n"
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&cipher_copy[offset..=end]).unwrap();
                offset += len;
            }
        });
        let url = url::Url::parse(&format!(
            "http://wwfile.example.invalid:{}/cgi-bin/download?f=unaligned",
            address.port()
        ))
        .unwrap();
        let desc = synthetic_descriptor(
            url.as_str(),
            &key,
            MediaQuality::File,
            Some(plain.len() as u64),
            Some(md5.clone()),
            true,
        );
        let mut out = Vec::new();
        let receipt = fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut out).unwrap();
        server.join().unwrap();
        assert_eq!(out, plain);
        assert_eq!(receipt.md5, md5);

        // 2. Single-response framing, header duplication, non-ASCII X-Errno, strict-size, alignment, and empty plaintext.
        let empty_cipher = encrypt_ecb(&[], &key);
        for (raw_response, expected_bytes, strict, expected_err) in [
            // Truncated 206 body (TCP EOF before Content-Length) is a retryable transport error.
            (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: {}\r\nContent-Range: bytes 0-{}/{}\r\n\r\n",
                    cipher.len(),
                    cipher.len() - 1,
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher[..16].iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "tp_media_fetch_failed",
            ),
            // Cleanly framed short chunked 206 body (EOF before Content-Range length) fails integrity.
            (
                [
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 0-{}/{}\r\n\r\n10\r\n",
                        cipher.len() - 1,
                        cipher.len()
                    )
                    .as_bytes(),
                    &cipher[..16],
                    b"\r\n0\r\n\r\n",
                ]
                .concat(),
                Some(plain.len() as u64),
                true,
                "media_integrity_failed",
            ),
            // 200 OK with partial Content-Range (end + 1 < size).
            (
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 32\r\nContent-Range: bytes 0-31/{}\r\n\r\n",
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher[..32].iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "media_integrity_failed",
            ),
            // Content-Length mismatching Content-Range.
            (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: 16\r\nContent-Range: bytes 0-{}/{}\r\n\r\n",
                    cipher.len() - 1,
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher[..16].iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "media_integrity_failed",
            ),
            // Duplicate Content-Encoding: identity then gzip.
            (
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Encoding: identity\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher.iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "tp_media_encoding_refused",
            ),
            // Duplicate X-Errno: 0 then -1001.
            (
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nX-Errno: 0\r\nX-Errno: -1001\r\nContent-Length: {}\r\n\r\n",
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher.iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "tp_media_fetch_failed",
            ),
            // Non-ASCII X-Errno fails closed.
            (
                [
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\nX-Errno: \xff\r\nContent-Length: "
                        .as_slice(),
                    cipher.len().to_string().as_bytes(),
                    b"\r\n\r\n",
                    cipher.as_slice(),
                ]
                .concat(),
                Some(plain.len() as u64),
                true,
                "tp_media_fetch_failed",
            ),
            // Duplicate Content-Range fails closed.
            (
                format!(
                    "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: {}\r\nContent-Range: bytes 0-{}/{}\r\nContent-Range: bytes 0-{}/{}\r\n\r\n",
                    cipher.len(),
                    cipher.len() - 1,
                    cipher.len(),
                    cipher.len() - 1,
                    cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(cipher.iter().copied())
                .collect::<Vec<_>>(),
                Some(plain.len() as u64),
                true,
                "media_integrity_failed",
            ),
            // Non-16-byte-aligned Content-Length rejected up front.
            (
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 17\r\n\r\n0123456789abcdefg"
                    .to_vec(),
                None,
                false,
                "media_integrity_failed",
            ),
            // Strict-size mismatch rejected up front from headers.
            (
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    cipher.len() + 16
                )
                .into_bytes(),
                Some(plain.len() as u64),
                true,
                "Advertised media size mismatch",
            ),
            // 0-byte plaintext (valid 16-byte PKCS#7 padding block) returns media_integrity_failed.
            (
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    empty_cipher.len()
                )
                .into_bytes()
                .into_iter()
                .chain(empty_cipher.iter().copied())
                .collect::<Vec<_>>(),
                None,
                false,
                "media_integrity_failed",
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let _ = read_http_request(&mut stream);
                let _ = stream.write_all(&raw_response);
            });
            let url = url::Url::parse(&format!(
                "http://wwfile.example.invalid:{}/cgi-bin/download?f=edge",
                address.port()
            ))
            .unwrap();
            let desc = synthetic_descriptor(
                url.as_str(),
                &key,
                MediaQuality::File,
                expected_bytes,
                None,
                strict,
            );
            let err =
                fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut Vec::new()).unwrap_err();
            server.join().unwrap();
            assert_eq!(err.to_string(), expected_err);
        }

        // 3. 200 OK on continuation chunk (position > 0) must fail.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let c_cont = cipher.clone();
        let server = std::thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let _ = read_http_request(&mut stream);
                if idx == 0 {
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: 32\r\nContent-Range: bytes 0-31/{}\r\n\r\n",
                        c_cont.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&c_cont[..32]);
                } else {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        c_cont.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&c_cont);
                }
            }
        });
        let url = url::Url::parse(&format!(
            "http://wwfile.example.invalid:{}/cgi-bin/download?f=cont200",
            address.port()
        ))
        .unwrap();
        let desc = synthetic_descriptor(url.as_str(), &key, MediaQuality::File, None, None, false);
        let err =
            fetch_tp_media_at(url, address, &desc, |_, _| Ok(()), &mut Vec::new()).unwrap_err();
        server.join().unwrap();
        assert_eq!(err.to_string(), "media_integrity_failed");

        // 4. Pre-flight expiry, size limit, and progress cancellation checks (no network needed).
        let dummy_url = url::Url::parse("http://wwfile.example.invalid:443/f").unwrap();
        let dummy_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();

        let mut expired = synthetic_descriptor(
            dummy_url.as_str(),
            &key,
            MediaQuality::File,
            Some(10),
            None,
            true,
        );
        expired.expires_at = Some(1);
        assert_eq!(
            fetch_tp_media_at(
                dummy_url.clone(),
                dummy_addr,
                &expired,
                |_, _| Ok(()),
                &mut Vec::new(),
            )
            .unwrap_err()
            .to_string(),
            "media_expired"
        );

        let oversized = synthetic_descriptor(
            dummy_url.as_str(),
            &key,
            MediaQuality::Thumbnail,
            Some(MAX_CDN_TRANSFER_BYTES as u64 + 1),
            None,
            true,
        );
        assert_eq!(
            fetch_tp_media_at(
                dummy_url.clone(),
                dummy_addr,
                &oversized,
                |_, _| Ok(()),
                &mut Vec::new(),
            )
            .unwrap_err()
            .to_string(),
            "tp_media_size_limit"
        );

        let cancel_desc = synthetic_descriptor(
            dummy_url.as_str(),
            &key,
            MediaQuality::File,
            Some(10),
            None,
            true,
        );
        assert_eq!(
            fetch_tp_media_at(
                dummy_url,
                dummy_addr,
                &cancel_desc,
                |_, _| bail!("media_cancelled"),
                &mut Vec::new(),
            )
            .unwrap_err()
            .to_string(),
            "media_cancelled"
        );
    }

    #[tokio::test]
    async fn async_tp_media_downloads_ranged_chunks_and_enforces_expiry_and_integrity() {
        let key = [0x44u8; 16];
        let plain: Vec<u8> = (0..(TP_CHUNK_BYTES + 1234))
            .map(|i| (i % 251) as u8)
            .collect();
        let ciphertext = encrypt_ecb(&plain, &key);
        let total = ciphertext.len();

        struct FixedClock(u64);
        impl Clock for FixedClock {
            fn monotonic_ms(&self) -> u64 {
                0
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(self.0 * 1000)
            }
        }
        struct RangedHttp {
            body: Vec<u8>,
            ranges_seen: std::sync::Mutex<Vec<String>>,
        }
        impl HttpTransport for RangedHttp {
            fn send<'a>(
                &'a self,
                req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                let range = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("range"))
                    .unwrap()
                    .1
                    .clone();
                let auth = req
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("authkey"))
                    .unwrap()
                    .1
                    .clone();
                assert_eq!(auth, "0123456789abcdef");
                self.ranges_seen.lock().unwrap().push(range.clone());
                let (start, end) = range
                    .strip_prefix("bytes=")
                    .unwrap()
                    .split_once('-')
                    .unwrap();
                let start: usize = start.parse().unwrap();
                let end: usize = end.parse::<usize>().unwrap().min(self.body.len() - 1);
                let slice = self.body[start..=end].to_vec();
                let resp = crate::transport::HttpResponse {
                    status: 206,
                    headers: vec![
                        ("X-ErrNo".into(), "0".into()),
                        (
                            "Content-Range".into(),
                            format!("bytes {start}-{end}/{}", self.body.len()),
                        ),
                        ("Content-Length".into(), slice.len().to_string()),
                    ],
                    body: slice,
                };
                Box::pin(async move { Ok(resp) })
            }
        }

        let mut desc = synthetic_descriptor(
            "https://wwfile.work.weixin.qq.com/cgi-bin/download?f=synthetic",
            &key,
            MediaQuality::File,
            Some(plain.len() as u64),
            Some(hex::encode(crate::crypto::md5(&plain))),
            true,
        );
        desc.expires_at = Some(2_000_000_000);
        let http = RangedHttp {
            body: ciphertext,
            ranges_seen: std::sync::Mutex::new(Vec::new()),
        };
        let downloaded =
            fetch_tp_media_async(&desc, &http, &FixedClock(1_700_000_000), |_, _| Ok(()))
                .await
                .unwrap();
        assert_eq!(downloaded.bytes, plain);
        assert_eq!(downloaded.md5, hex::encode(crate::crypto::md5(&plain)));
        assert_eq!(
            http.ranges_seen.lock().unwrap().len(),
            total.div_ceil(TP_CHUNK_BYTES)
        );

        let expired = fetch_tp_media_async(&desc, &http, &FixedClock(2_000_000_000), |_, _| Ok(()))
            .await
            .err()
            .unwrap();
        assert_eq!(expired.to_string(), "media_expired");

        // Custom AsyncDownloadSink receives non-empty decrypted chunks and flush().
        struct RecordingSink {
            bytes: Vec<u8>,
            writes: usize,
            flushed: bool,
        }
        impl AsyncDownloadSink for RecordingSink {
            fn write_all<'a>(
                &'a mut self,
                buf: &'a [u8],
            ) -> crate::transport::TransportFuture<'a, Result<()>> {
                assert!(!buf.is_empty(), "sink must not receive empty writes");
                self.bytes.extend_from_slice(buf);
                self.writes += 1;
                Box::pin(std::future::ready(Ok(())))
            }
            fn flush<'a>(&'a mut self) -> crate::transport::TransportFuture<'a, Result<()>> {
                self.flushed = true;
                Box::pin(std::future::ready(Ok(())))
            }
        }
        let mut sink = RecordingSink {
            bytes: Vec::new(),
            writes: 0,
            flushed: false,
        };
        let receipt = fetch_tp_media_to_async(
            &desc,
            &http,
            &FixedClock(1_700_000_000),
            |_, _| Ok(()),
            &mut sink,
        )
        .await
        .unwrap();
        assert_eq!(sink.bytes, plain);
        assert!(sink.writes > 0 && sink.flushed);
        assert_eq!(receipt.byte_length, plain.len() as u64);

        // Small strict-size descriptor still gives chunk 0 >= TP_CHUNK_BYTES budget so
        // 4xx error pages and small strict-size mismatches preserve exact error codes.
        let small_plain = b"hello world".to_vec();
        let small_desc = synthetic_descriptor(
            "https://wwfile.work.weixin.qq.com/cgi-bin/download?f=small",
            &key,
            MediaQuality::File,
            Some(small_plain.len() as u64),
            Some(hex::encode(crate::crypto::md5(&small_plain))),
            true,
        );
        struct EnforcingHttp(crate::transport::HttpResponse);
        impl HttpTransport for EnforcingHttp {
            fn send<'a>(
                &'a self,
                req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                assert!(req.max_response_bytes >= TP_CHUNK_BYTES);
                if self.0.body.len() > req.max_response_bytes {
                    return Box::pin(async {
                        Err(crate::transport::TransportError::ResponseTooLarge.into())
                    });
                }
                let resp = self.0.clone();
                Box::pin(async move { Ok(resp) })
            }
        }
        let http_403 = EnforcingHttp(crate::transport::HttpResponse {
            status: 403,
            headers: vec![],
            body: vec![b'x'; 256],
        });
        assert_eq!(
            fetch_tp_media_async(
                &small_desc,
                &http_403,
                &FixedClock(1_700_000_000),
                |_, _| Ok(())
            )
            .await
            .err()
            .unwrap()
            .to_string(),
            "CDN HTTP status 403"
        );
        let mismatch_ct = encrypt_ecb(&[0u8; 64], &key);
        let http_mismatch = EnforcingHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![("Content-Length".into(), mismatch_ct.len().to_string())],
            body: mismatch_ct,
        });
        assert_eq!(
            fetch_tp_media_async(
                &small_desc,
                &http_mismatch,
                &FixedClock(1_700_000_000),
                |_, _| Ok(())
            )
            .await
            .err()
            .unwrap()
            .to_string(),
            "Advertised media size mismatch"
        );

        struct TooLargeHttp;
        impl HttpTransport for TooLargeHttp {
            fn send<'a>(
                &'a self,
                _req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                Box::pin(async { Err(crate::transport::TransportError::ResponseTooLarge.into()) })
            }
        }
        assert_eq!(
            fetch_tp_media_async(
                &small_desc,
                &TooLargeHttp,
                &FixedClock(1_700_000_000),
                |_, _| Ok(())
            )
            .await
            .err()
            .unwrap()
            .to_string(),
            "tp_media_size_limit"
        );
    }
}
