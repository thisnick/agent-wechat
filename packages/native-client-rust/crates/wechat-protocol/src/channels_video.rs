//! Download and decode one WeChat Channels video from freshly resolved playback material.
//!
//! The address and token come from the authenticated detail reply and are used once. The request
//! goes only to the Channels video host, pinned to a checked public address, without redirects or
//! proxies. The first 128 KiB are decoded while bytes arrive; the rest is stored as received.
use anyhow::{ensure, Result};
use std::{io::Read, net::SocketAddr, time::Duration};
use wechat_media::channels::{apply_channels_prefix, channels_prefix_keystream};

/// Upper bound on a Channels video download.
pub const MAX_CHANNELS_VIDEO_BYTES: usize = super::MAX_VIDEO_DOWNLOAD_BYTES;
const VIDEO_HOST: &str = "finder.video.qq.com";
/// Host the detail reply uses for the same objects; the player rewrites it to [`VIDEO_HOST`].
const LEGACY_HOST: &str = "wxapp.tc.qq.com";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36 MicroMessenger";
const REFERER: &str = "https://channels.weixin.qq.com/";

/// Build the download address the way the official player does: the returned address with its
/// token appended, over HTTPS, on the video host.
pub fn channels_video_url(media: &crate::ChannelsMedia) -> Result<reqwest::Url> {
    let (url, token) = media.transport();
    let mut address = reqwest::Url::parse(&format!("{url}{token}"))
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

/// Download the post's video and return the decoded MP4 bytes.
///
/// `max_bytes` caps the transfer below [`MAX_CHANNELS_VIDEO_BYTES`]. The advertised size, when
/// the reply gives one, must fit the cap and match the received length.
pub fn fetch_channels_video(
    media: &crate::ChannelsMedia,
    max_bytes: usize,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<Vec<u8>> {
    let url = channels_video_url(media)?;
    let address = super::link_thumbnail::public_address(
        &url,
        "channels_download_failed",
        "channels_download_failed",
    )?;
    download_at(url, address, media, max_bytes, progress)
}

fn timeout_for(expected: Option<u64>) -> Duration {
    // At least two minutes, plus one second per MiB, at most thirty minutes.
    let seconds = expected.map_or(0, |bytes| bytes / (1024 * 1024));
    Duration::from_secs((120 + seconds).min(30 * 60))
}

fn download_at(
    url: reqwest::Url,
    address: SocketAddr,
    media: &crate::ChannelsMedia,
    max_bytes: usize,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<Vec<u8>> {
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
    let total = response.content_length();
    if let Some(length) = total {
        ensure!(length <= cap, "channels_media_too_large");
        ensure!(
            advertised.iter().flatten().next().is_none()
                || advertised.iter().flatten().any(|size| *size == length),
            "channels_integrity_failed"
        );
    }
    let stream = channels_prefix_keystream(media.decode_key());
    let mut bytes = Vec::with_capacity(total.unwrap_or(0).min(cap) as usize);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        progress(bytes.len() as u64, total)?;
        let n = response
            .read(&mut chunk)
            .map_err(|_| anyhow::anyhow!("channels_download_failed"))?;
        if n == 0 {
            break;
        }
        ensure!((bytes.len() + n) as u64 <= cap, "channels_media_too_large");
        let offset = bytes.len() as u64;
        apply_channels_prefix(&mut chunk[..n], offset, &stream);
        bytes.extend_from_slice(&chunk[..n]);
    }
    let received = bytes.len() as u64;
    ensure!(
        received > 0 && total.is_none_or(|n| n == received),
        "channels_integrity_failed"
    );
    ensure!(
        advertised.iter().flatten().next().is_none()
            || advertised.iter().flatten().any(|size| *size == received),
        "channels_integrity_failed"
    );
    Ok(bytes)
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
    ) -> (reqwest::Url, SocketAddr, std::thread::JoinHandle<String>) {
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
            reqwest::Url::parse(&format!("http://127.0.0.1:{}/video?x=1", address.port())).unwrap();
        (url, address, server)
    }

    #[test]
    fn decodes_the_prefix_and_checks_length_type_and_redirects() {
        let plain: Vec<u8> = (0..CHANNELS_PREFIX_BYTES + 1000)
            .map(|i| (i % 253) as u8)
            .collect();
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
        let bytes = download_at(
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
        assert_eq!(bytes, plain);
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
            ("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: video/mp4\r\nContent-Length: 4\r\n\r\n".to_string(), b"abcd".to_vec(), Some(5), "channels_integrity_failed"),
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
    fn an_oversized_advertised_size_fails_before_connecting() {
        let address: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:9/v").unwrap();
        let error = download_at(url, address, &media("u", "", 1, Some(100)), 99, |_, _| {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "channels_media_too_large");
    }
}
