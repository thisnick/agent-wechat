//! Download and decode one WeChat Channels video from freshly resolved playback material.
//!
//! The address and token come from the authenticated detail reply and are used once. The request
//! goes only to the Channels video host, pinned to a checked public address, without redirects or
//! proxies. The first 128 KiB are decoded while bytes arrive; the rest is stored as received.
use crate::transport::{HttpMethod, HttpRequest, HttpTransport};
use anyhow::{ensure, Result};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::{io::Read, net::SocketAddr};
use wechat_media::channels::{apply_channels_prefix, channels_prefix_keystream};

/// Upper bound on a Channels video download.
pub const MAX_CHANNELS_VIDEO_BYTES: usize = super::MAX_VIDEO_DOWNLOAD_BYTES;
const VIDEO_HOST: &str = "finder.video.qq.com";
/// Host the detail reply uses for the same objects; the player rewrites it to [`VIDEO_HOST`].
const LEGACY_HOST: &str = "wxapp.tc.qq.com";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36 MicroMessenger";
const REFERER: &str = "https://channels.weixin.qq.com/";

/// What the CDN delivered, kept for checks and failure diagnostics. Holds no address, token or
/// key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelsDownloadFacts {
    /// Video size the detail reply advertised, when present.
    pub advertised_bytes: Option<u64>,
    /// CDN copy size the detail reply advertised, when present.
    pub advertised_cdn_bytes: Option<u64>,
    /// `Content-Length` of the response, when present.
    pub content_length: Option<u64>,
    /// Bytes actually received.
    pub received_bytes: u64,
    /// Media type of the response, when it is a plain `type/subtype`.
    pub content_type: Option<String>,
    /// Box type at offset 4 as received, when printable.
    pub encoded_box: Option<String>,
    /// Box type at offset 4 after decoding the prefix, when printable.
    pub decoded_box: Option<String>,
}

/// A downloaded Channels video: the decoded bytes and what the CDN delivered.
pub struct ChannelsVideo {
    /// Decoded MP4 bytes.
    pub bytes: Vec<u8>,
    /// Sizes, media type and box types observed during the download.
    pub facts: ChannelsDownloadFacts,
}

impl std::fmt::Debug for ChannelsVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelsVideo")
            .field("bytes", &self.bytes.len())
            .field("facts", &self.facts)
            .finish()
    }
}

/// A downloaded video failed a check. Its message is the stable code
/// `channels_integrity_failed`; `check` names which check and `facts` say what arrived.
#[derive(Debug, Clone)]
pub struct ChannelsDownloadFailure {
    /// `empty`, `length` (fewer or more bytes than `Content-Length`) or `container`.
    pub check: &'static str,
    /// What the CDN delivered.
    pub facts: ChannelsDownloadFacts,
}

impl std::fmt::Display for ChannelsDownloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("channels_integrity_failed")
    }
}

impl std::error::Error for ChannelsDownloadFailure {}

/// The four-byte box type at offset 4, when it is printable ASCII.
pub fn channels_box_type(bytes: &[u8]) -> Option<String> {
    let kind = bytes.get(4..8)?;
    kind.iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b' ')
        .then(|| String::from_utf8_lossy(kind).into_owned())
}

/// Build the download address the way the official player does: the returned address with its
/// token appended, over HTTPS, on the video host.
pub fn channels_video_url(media: &crate::ChannelsMedia) -> Result<url::Url> {
    let (url, token) = media.transport();
    let mut address = url::Url::parse(&format!("{url}{token}"))
        .map_err(|_| anyhow::anyhow!("channels_unsupported_media"))?;
    ensure!(
        matches!(address.scheme(), "http" | "https")
            && address.username().is_empty()
            && address.password().is_none()
            && address.port().is_none()
            && address.fragment().is_none(),
        "channels_unsupported_media"
    );
    match address.host_str() {
        Some(LEGACY_HOST) => address
            .set_host(Some(VIDEO_HOST))
            .map_err(|_| anyhow::anyhow!("channels_unsupported_media"))?,
        Some(VIDEO_HOST) => {}
        _ => anyhow::bail!("channels_unsupported_media"),
    }
    address
        .set_scheme("https")
        .map_err(|_| anyhow::anyhow!("channels_unsupported_media"))?;
    Ok(address)
}

// Legacy blocking Channels video downloader kept for 3.x native compatibility (#701).
// Prefer `fetch_channels_video_async` over `HttpTransport` for new code.
/// Download the post's video and return the decoded MP4 bytes.
///
/// `max_bytes` caps the transfer below [`MAX_CHANNELS_VIDEO_BYTES`]. The advertised size, when
/// the reply gives one, must fit the cap and match the received length.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_channels_video(
    media: &crate::ChannelsMedia,
    max_bytes: usize,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<ChannelsVideo> {
    let url = channels_video_url(media)?;
    let address = super::link_thumbnail::public_address(
        &url,
        "channels_download_failed",
        "channels_download_failed",
    )?;
    download_at(url, address, media, max_bytes, progress)
}

fn split_http_list_unquoted(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in value.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == ',' {
            parts.push(&value[start..idx]);
            start = idx + 1;
        }
    }
    parts.push(&value[start..]);
    parts
}

/// Asynchronously download one Channels post's video and return the decoded MP4
/// bytes using a pluggable [`HttpTransport`].
///
/// `max_bytes` caps the transfer below [`MAX_CHANNELS_VIDEO_BYTES`]. The
/// caller's [`HttpTransport`] must not follow HTTP redirects and is responsible
/// for network/SSRF boundaries.
/// On native targets, `transport::NativeHttpTransport` built with
/// `DestinationPolicy::PublicOnly` provides that boundary.
pub async fn fetch_channels_video_async(
    media: &crate::ChannelsMedia,
    max_bytes: usize,
    http: &dyn HttpTransport,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<ChannelsVideo> {
    let url = channels_video_url(media)?;
    let cap = max_bytes.min(MAX_CHANNELS_VIDEO_BYTES) as u64;
    let advertised = [media.file_size, media.cdn_file_size];
    ensure!(
        advertised.iter().flatten().all(|size| *size <= cap),
        "channels_media_too_large"
    );
    progress(0, None)?;
    let response = http
        .send(HttpRequest {
            method: HttpMethod::Get,
            url,
            headers: vec![
                ("Accept-Encoding".into(), "identity".into()),
                ("User-Agent".into(), USER_AGENT.into()),
                ("Referer".into(), REFERER.into()),
            ],
            body: Vec::new(),
            timeout: timeout_for(media.file_size.or(media.cdn_file_size)),
            max_response_bytes: cap as usize,
        })
        .await
        .map_err(|error| {
            super::download_failure(
                &error,
                "channels_media_too_large",
                "channels_download_failed",
                "channels_download_failed",
            )
        })?;
    if response.status != 200 {
        anyhow::bail!("CDN HTTP status {}", response.status);
    }
    ensure!(
        response
            .header_values("x-errno")
            .into_iter()
            .all(|errno| errno.trim() == "0"),
        "channels_download_failed"
    );
    ensure!(
        response
            .header_values("content-encoding")
            .into_iter()
            .all(|v| v.trim().eq_ignore_ascii_case("identity")),
        "channels_download_failed"
    );
    let raw_content_types = response.header_values("content-type");
    let content_type_parts: Vec<&str> = raw_content_types
        .iter()
        .flat_map(|value| split_http_list_unquoted(value))
        .collect();
    ensure!(
        content_type_parts.iter().all(|kind| {
            let trimmed = kind.trim().to_ascii_lowercase();
            trimmed.starts_with("video/") || trimmed.starts_with("application/octet-stream")
        }),
        "channels_download_failed"
    );
    let content_type = content_type_parts
        .first()
        .map(|kind| {
            kind.to_ascii_lowercase()
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
        .filter(|kind| {
            kind.len() <= 64
                && kind.contains('/')
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/.+-".contains(&b))
        });
    let total = response
        .content_length()
        .map_err(|_| anyhow::anyhow!("channels_download_failed"))?;
    if let Some(length) = total {
        ensure!(length <= cap, "channels_media_too_large");
    }
    ensure!(
        response.body.len() as u64 <= cap,
        "channels_media_too_large"
    );
    let mut bytes = response.body;
    let head = bytes[..bytes.len().min(8)].to_vec();
    let stream = channels_prefix_keystream(media.decode_key());
    apply_channels_prefix(&mut bytes, 0, &stream);
    progress(bytes.len() as u64, total)?;
    let facts = ChannelsDownloadFacts {
        advertised_bytes: media.file_size,
        advertised_cdn_bytes: media.cdn_file_size,
        content_length: total,
        received_bytes: bytes.len() as u64,
        content_type,
        encoded_box: channels_box_type(&head),
        decoded_box: channels_box_type(&bytes),
    };
    let check = if bytes.is_empty() {
        Some("empty")
    } else if total.is_some_and(|n| n != facts.received_bytes) {
        Some("length")
    } else {
        None
    };
    if let Some(check) = check {
        return Err(ChannelsDownloadFailure { check, facts }.into());
    }
    Ok(ChannelsVideo { bytes, facts })
}

fn timeout_for(expected: Option<u64>) -> Duration {
    // At least two minutes, plus one second per MiB, at most thirty minutes.
    let seconds = expected.map_or(0, |bytes| bytes / (1024 * 1024));
    Duration::from_secs((120 + seconds).min(30 * 60))
}

#[cfg(not(target_arch = "wasm32"))]
fn download_at(
    url: url::Url,
    address: SocketAddr,
    media: &crate::ChannelsMedia,
    max_bytes: usize,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<ChannelsVideo> {
    let cap = max_bytes.min(MAX_CHANNELS_VIDEO_BYTES) as u64;
    let advertised = [media.file_size, media.cdn_file_size];
    ensure!(
        advertised.iter().flatten().all(|size| *size <= cap),
        "channels_media_too_large"
    );
    let host = url.host_str().unwrap_or_default().to_owned();
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout_for(media.file_size.or(media.cdn_file_size)))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve(&host, address)
        .build()?;
    let mut response = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .header("User-Agent", USER_AGENT)
        .header("Referer", REFERER)
        .send()
        .map_err(|_| anyhow::anyhow!("channels_download_failed"))?;
    if response.status() != 200 {
        anyhow::bail!("CDN HTTP status {}", response.status().as_u16());
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_ascii_lowercase)
    };
    ensure!(
        header("x-errno").is_none_or(|errno| errno.trim() == "0"),
        "channels_download_failed"
    );
    ensure!(
        header("content-encoding").is_none_or(|v| v == "identity"),
        "channels_download_failed"
    );
    ensure!(
        header("content-type").is_none_or(
            |kind| kind.starts_with("video/") || kind.starts_with("application/octet-stream")
        ),
        "channels_download_failed"
    );
    let content_type = header("content-type")
        .map(|kind| kind.split(';').next().unwrap_or_default().trim().to_owned())
        .filter(|kind| {
            kind.len() <= 64
                && kind.contains('/')
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/.+-".contains(&b))
        });
    let total = response.content_length();
    if let Some(length) = total {
        ensure!(length <= cap, "channels_media_too_large");
    }
    let stream = channels_prefix_keystream(media.decode_key());
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(cap) as usize);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut head = Vec::with_capacity(8);
    loop {
        progress(bytes.len() as u64, total)?;
        let n = response
            .read(&mut chunk)
            .map_err(|_| anyhow::anyhow!("channels_download_failed"))?;
        if n == 0 {
            break;
        }
        ensure!((bytes.len() + n) as u64 <= cap, "channels_media_too_large");
        if head.len() < 8 {
            let wanted = (8 - head.len()).min(n);
            head.extend_from_slice(&chunk[..wanted]);
        }
        let offset = bytes.len() as u64;
        apply_channels_prefix(&mut chunk[..n], offset, &stream);
        bytes.extend_from_slice(&chunk[..n]);
    }
    let facts = ChannelsDownloadFacts {
        advertised_bytes: media.file_size,
        advertised_cdn_bytes: media.cdn_file_size,
        content_length: total,
        received_bytes: bytes.len() as u64,
        content_type,
        encoded_box: channels_box_type(&head),
        decoded_box: channels_box_type(&bytes),
    };
    // An advertised size that differs is recorded, not fatal: the container check decides
    // whether the bytes are the expected video.
    let check = if bytes.is_empty() {
        Some("empty")
    } else if total.is_some_and(|n| n != facts.received_bytes) {
        Some("length")
    } else {
        None
    };
    if let Some(check) = check {
        return Err(ChannelsDownloadFailure { check, facts }.into());
    }
    Ok(ChannelsVideo { bytes, facts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};
    use wechat_media::channels::CHANNELS_PREFIX_BYTES;

    fn media(url: &str, token: &str, key: u64, size: Option<u64>) -> crate::ChannelsMedia {
        crate::ChannelsMedia::for_tests(url, token, key, size)
    }

    #[test]
    fn download_address_appends_the_token_and_moves_to_the_video_host() {
        let url = channels_video_url(&media(
            "http://wxapp.tc.qq.com/251/20302/stodownload?encfilekey=a%2Bb&idx=1",
            "&token=t%3D1&web=1",
            1,
            None,
        ))
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://finder.video.qq.com/251/20302/stodownload?encfilekey=a%2Bb&idx=1&token=t%3D1&web=1"
        );
        for bad in [
            "http://example.invalid/251/stodownload?x=1",
            "http://wxapp.tc.qq.com:8080/x",
            "http://user@wxapp.tc.qq.com/x",
            "ftp://wxapp.tc.qq.com/x",
            "not a url",
        ] {
            assert!(
                channels_video_url(&media(bad, "", 1, None)).is_err(),
                "{bad}"
            );
        }
    }

    fn serve(
        head: String,
        body: Vec<u8>,
    ) -> (url::Url, SocketAddr, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = vec![];
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            String::from_utf8(request).unwrap()
        });
        let url =
            url::Url::parse(&format!("http://127.0.0.1:{}/video?x=1", address.port())).unwrap();
        (url, address, server)
    }

    #[test]
    fn decodes_the_prefix_and_checks_length_type_and_redirects() {
        let mut plain: Vec<u8> = (0..CHANNELS_PREFIX_BYTES + 1000)
            .map(|i| (i % 253) as u8)
            .collect();
        plain[..8].copy_from_slice(&[0, 0, 0, 32, b'f', b't', b'y', b'p']);
        let mut encrypted = plain.clone();
        let stream = channels_prefix_keystream(42);
        apply_channels_prefix(&mut encrypted, 0, &stream);
        let ok = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\nX-ErrNo: 0\r\nContent-Length: {}\r\n\r\n",
            encrypted.len()
        );
        let (url, address, server) = serve(ok.clone(), encrypted.clone());
        let size = Some(encrypted.len() as u64);
        let mut seen = 0;
        let video = download_at(
            url,
            address,
            &media("u", "", 42, size),
            usize::MAX,
            |done, _| {
                seen = done;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(video.bytes, plain);
        assert_eq!(video.facts.received_bytes, plain.len() as u64);
        assert_eq!(video.facts.content_length, size);
        assert_eq!(video.facts.content_type.as_deref(), Some("video/mp4"));
        assert_eq!(video.facts.decoded_box.as_deref(), Some("ftyp"));
        assert_ne!(video.facts.encoded_box.as_deref(), Some("ftyp"));
        assert_eq!(seen, plain.len() as u64);
        let request = server.join().unwrap();
        assert!(
            request.contains("referer: https://channels.weixin.qq.com/\r\n")
                || request.contains("Referer: https://channels.weixin.qq.com/\r\n")
        );

        let cases = [
            ("HTTP/1.1 302 Found\r\nConnection: close\r\nLocation: http://127.0.0.1/x\r\nContent-Length: 0\r\n\r\n".to_string(), vec![], size, "CDN HTTP status 302"),
            ("HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_string(), vec![], size, "CDN HTTP status 403"),
            ("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/html\r\nContent-Length: 4\r\n\r\n".to_string(), b"<ht>".to_vec(), None, "channels_download_failed"),
            ("HTTP/1.1 200 OK\r\nConnection: close\r\nX-ErrNo: -5103144\r\nContent-Length: 4\r\n\r\n".to_string(), b"abcd".to_vec(), None, "channels_download_failed"),
            ("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\n\r\n".to_string(), vec![1; 64], Some(32), "channels_media_too_large"),
        ];
        for (head, body, advertised, expected) in cases {
            let cap = if expected == "channels_media_too_large" {
                32
            } else {
                usize::MAX
            };
            let (url, address, server) = serve(head, body);
            let error = download_at(url, address, &media("u", "", 1, advertised), cap, |_, _| {
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
            let _ = server.join();
        }
    }

    #[test]
    fn a_different_advertised_size_is_recorded_and_a_short_body_is_a_length_failure() {
        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\nContent-Length: 16\r\n\r\n";
        let (url, address, server) = serve(head.into(), vec![7; 16]);
        let video = download_at(
            url,
            address,
            &media("u", "", 1, Some(99)),
            usize::MAX,
            |_, _| Ok(()),
        )
        .unwrap();
        let _ = server.join();
        assert_eq!(video.facts.advertised_bytes, Some(99));
        assert_eq!(video.facts.received_bytes, 16);

        let (url, address, server) = serve(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\n\r\n".into(),
            vec![],
        );
        let error = download_at(
            url,
            address,
            &media("u", "", 1, None),
            usize::MAX,
            |_, _| Ok(()),
        )
        .unwrap_err();
        let _ = server.join();
        let failure = error.downcast_ref::<ChannelsDownloadFailure>().unwrap();
        assert_eq!(failure.check, "empty");
        assert_eq!(error.to_string(), "channels_integrity_failed");
    }

    #[test]
    fn an_oversized_advertised_size_fails_before_connecting() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let url = url::Url::parse("http://127.0.0.1:9/v").unwrap();
        let error = download_at(url, address, &media("u", "", 1, Some(100)), 99, |_, _| {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "channels_media_too_large");
    }

    #[tokio::test]
    async fn async_channels_video_decodes_prefix_and_checks_headers_and_bounds() {
        let mut plain: Vec<u8> = (0..CHANNELS_PREFIX_BYTES + 512)
            .map(|i| (i % 253) as u8)
            .collect();
        plain[..8].copy_from_slice(&[0, 0, 0, 32, b'f', b't', b'y', b'p']);
        let mut encrypted = plain.clone();
        let stream = channels_prefix_keystream(42);
        apply_channels_prefix(&mut encrypted, 0, &stream);

        struct StaticHttp(crate::transport::HttpResponse);
        impl HttpTransport for StaticHttp {
            fn send<'a>(
                &'a self,
                req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                assert_eq!(req.url.scheme(), "https");
                assert_eq!(req.url.host_str(), Some(VIDEO_HOST));
                assert!(req
                    .headers
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("referer") && v == REFERER));
                let resp = self.0.clone();
                Box::pin(async move { Ok(resp) })
            }
        }
        let info = media(
            "http://wxapp.tc.qq.com/251/20302/stodownload?encfilekey=a",
            "&token=t",
            42,
            Some(encrypted.len() as u64),
        );
        let ok = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "video/mp4".into()),
                ("X-ErrNo".into(), "0".into()),
                ("Content-Length".into(), encrypted.len().to_string()),
            ],
            body: encrypted.clone(),
        });
        let video = fetch_channels_video_async(&info, usize::MAX, &ok, |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(video.bytes, plain);
        assert_eq!(video.facts.decoded_box.as_deref(), Some("ftyp"));
        assert_eq!(video.facts.content_type.as_deref(), Some("video/mp4"));

        let bad_type = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "video/mp4".into()),
                ("Content-Type".into(), "text/html".into()),
            ],
            body: encrypted.clone(),
        });
        assert_eq!(
            fetch_channels_video_async(&info, usize::MAX, &bad_type, |_, _| Ok(()))
                .await
                .unwrap_err()
                .to_string(),
            "channels_download_failed"
        );

        let folded_bad_type = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![(
                "Content-Type".into(),
                "video/mp4; codecs=\"avc1.4d401f, mp4a.40.2\", text/html".into(),
            )],
            body: encrypted.clone(),
        });
        assert_eq!(
            fetch_channels_video_async(&info, usize::MAX, &folded_bad_type, |_, _| Ok(()))
                .await
                .unwrap_err()
                .to_string(),
            "channels_download_failed"
        );

        let quoted_codecs_ok = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![
                (
                    "Content-Type".into(),
                    "video/mp4; codecs=\"avc1.4d401f, mp4a.40.2\"".into(),
                ),
                ("X-ErrNo".into(), "0".into()),
                ("Content-Length".into(), encrypted.len().to_string()),
            ],
            body: encrypted.clone(),
        });
        let quoted_video =
            fetch_channels_video_async(&info, usize::MAX, &quoted_codecs_ok, |_, _| Ok(()))
                .await
                .unwrap();
        assert_eq!(quoted_video.bytes, plain);
        assert_eq!(
            quoted_video.facts.content_type.as_deref(),
            Some("video/mp4")
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
            fetch_channels_video_async(&info, usize::MAX, &TooLargeHttp, |_, _| Ok(()))
                .await
                .unwrap_err()
                .to_string(),
            "channels_media_too_large"
        );

        let short = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "video/mp4".into()),
                ("Content-Length".into(), (encrypted.len() + 10).to_string()),
            ],
            body: encrypted,
        });
        let err = fetch_channels_video_async(&info, usize::MAX, &short, |_, _| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<ChannelsDownloadFailure>().unwrap().check,
            "length"
        );
    }
}
