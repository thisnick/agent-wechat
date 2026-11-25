//! Original type-47 bytes from the verified CDN host. Signed URLs remain private.
use crate::transport::{Clock, HttpMethod, HttpRequest, HttpTransport};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::{io::Read, net::ToSocketAddrs};
use std::{net::IpAddr, time::Duration};
pub use wechat_media::{inspect_image, ImageInspection};
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerDescriptor {
    pub url: String,
    pub expected_bytes: u64,
    pub expected_md5: String,
    pub expires_at: Option<u64>,
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
const VWEIXINF_STICKER_HOST: &str = "vweixinf.tc.qq.com";

/// Tencent CDN hosts accepted for received sticker downloads regardless of path.
/// Callers must use `validate_sticker_url` for the full policy, which also accepts
/// `vweixinf.tc.qq.com` for its numeric `/stodownload` endpoint.
pub const STICKER_HOSTS: &[&str] = &["snsvideo.c2c.wechat.com", "wxapp.tc.qq.com"];

/// Restricts the vweixinf CDN to its numeric sticker download endpoint.
fn allowed_sticker_source(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host == VWEIXINF_STICKER_HOST {
        return url
            .path()
            .strip_prefix('/')
            .and_then(|path| path.strip_suffix("/stodownload"))
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(region, id)| {
                !region.is_empty()
                    && !id.is_empty()
                    && region.bytes().all(|byte| byte.is_ascii_digit())
                    && id.bytes().all(|byte| byte.is_ascii_digit())
            });
    }
    STICKER_HOSTS.contains(&host)
}

/// Validates a sticker source URL before download: HTTP or HTTPS on `STICKER_HOSTS` or
/// `vweixinf.tc.qq.com/<region>/<id>/stodownload`, default port only, with no credentials or
/// fragment.
pub fn validate_sticker_url(value: &str) -> Result<url::Url> {
    ensure!(
        value.len() <= 8192 && !value.chars().any(|c| c.is_control()),
        "sticker_source_not_allowed"
    );
    let url = url::Url::parse(value).map_err(|_| anyhow::anyhow!("sticker_source_not_allowed"))?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && allowed_sticker_source(&url)
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
/// Download and verify one sticker image from a validated sticker CDN host.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_sticker(
    descriptor: &StickerDescriptor,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    ensure!(
        descriptor.expected_bytes > 0 && descriptor.expected_bytes <= 16 * 1024 * 1024,
        "sticker_metadata_invalid"
    );
    if let Some(expiry) = descriptor.expires_at {
        ensure!(expiry > crate::wire::unix_time()? as u64, "media_expired");
    }
    fetch_sticker_source(
        &descriptor.url,
        &descriptor.expected_md5,
        Some(descriptor.expected_bytes),
        &mut progress,
    )
}

/// Fetch an original whose metadata has no advertised length. Enforces the same
/// host, DNS, stream-size, MD5 and image checks without inventing a source length.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_sticker_original(
    url: &str,
    expected_md5: &str,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    fetch_sticker_source(url, expected_md5, None, progress)
}

/// Asynchronously download and verify one sticker image from a validated
/// sticker CDN host using a pluggable [`HttpTransport`] and [`Clock`].
///
/// The caller's [`HttpTransport`] must not follow HTTP redirects and is
/// responsible for network/SSRF boundaries.
pub async fn fetch_sticker_async(
    descriptor: &StickerDescriptor,
    http: &dyn HttpTransport,
    clock: &dyn Clock,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    ensure!(
        descriptor.expected_bytes > 0 && descriptor.expected_bytes <= 16 * 1024 * 1024,
        "sticker_metadata_invalid"
    );
    if let Some(expiry) = descriptor.expires_at {
        ensure!(expiry > u64::from(clock.unix_time_secs()?), "media_expired");
    }
    fetch_sticker_source_async(
        &descriptor.url,
        &descriptor.expected_md5,
        Some(descriptor.expected_bytes),
        http,
        &mut progress,
    )
    .await
}

/// Asynchronously fetch a sticker original whose metadata has no advertised
/// length, enforcing the same host, size, MD5, and image checks.
///
/// The caller's [`HttpTransport`] must not follow HTTP redirects and is
/// responsible for network/SSRF boundaries.
pub async fn fetch_sticker_original_async(
    url: &str,
    expected_md5: &str,
    http: &dyn HttpTransport,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    fetch_sticker_source_async(url, expected_md5, None, http, progress).await
}

async fn fetch_sticker_source_async(
    url: &str,
    expected_md5: &str,
    expected_bytes: Option<u64>,
    http: &dyn HttpTransport,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    let url = validate_sticker_url(url)?;
    ensure!(
        expected_md5.len() == 32 && expected_md5.bytes().all(|b| b.is_ascii_hexdigit()),
        "sticker_metadata_invalid"
    );
    progress(0, expected_bytes)?;
    let response = http
        .send(HttpRequest {
            method: HttpMethod::Get,
            url,
            headers: vec![("Accept-Encoding".into(), "identity".into())],
            body: Vec::new(),
            timeout: Duration::from_secs(20),
            max_response_bytes: 16 * 1024 * 1024,
        })
        .await
        .map_err(|error| {
            if crate::transport::find_transport_error(&error)
                == Some(crate::transport::TransportError::ResponseTooLarge)
            {
                anyhow::anyhow!("sticker_response_too_large")
            } else {
                anyhow::anyhow!("sticker_fetch_failed")
            }
        })?;
    ensure!(
        response.status == 200,
        "sticker_http_status_{}",
        response.status
    );
    ensure!(
        response
            .header_values("content-encoding")
            .into_iter()
            .all(|v| v.trim().eq_ignore_ascii_case("identity")),
        "sticker_content_encoding_refused"
    );
    let content_length = response
        .content_length()
        .map_err(|_| anyhow::anyhow!("sticker_fetch_failed"))?;
    if let Some(length) = content_length {
        ensure!(length <= 16 * 1024 * 1024, "sticker_response_too_large");
    }
    ensure!(
        response.body.len() <= 16 * 1024 * 1024,
        "sticker_response_too_large"
    );
    ensure!(
        content_length.is_none_or(|length| response.body.len() as u64 == length),
        "media_integrity_failed"
    );
    progress(response.body.len() as u64, expected_bytes)?;
    ensure!(
        expected_bytes.is_none_or(|length| response.body.len() as u64 == length)
            && hex::encode(crate::crypto::md5(&response.body)).eq_ignore_ascii_case(expected_md5),
        "media_integrity_failed"
    );
    let inspection = inspect_image(&response.body)?;
    Ok((response.body, inspection))
}
#[cfg(not(target_arch = "wasm32"))]
fn fetch_sticker_source(
    url: &str,
    expected_md5: &str,
    expected_bytes: Option<u64>,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<(Vec<u8>, ImageInspection)> {
    let url = validate_sticker_url(url)?;
    ensure!(
        expected_md5.len() == 32 && expected_md5.bytes().all(|b| b.is_ascii_hexdigit()),
        "sticker_metadata_invalid"
    );
    progress(0, expected_bytes)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("sticker_source_not_allowed"))?;
    let port = if url.scheme() == "https" { 443 } else { 80 };
    let addresses = (host, port)
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
        .resolve(host, *address)
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
        progress(bytes.len() as u64, expected_bytes)?;
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
        expected_bytes.is_none_or(|length| bytes.len() as u64 == length)
            && hex::encode(crate::crypto::md5(&bytes)).eq_ignore_ascii_case(expected_md5),
        "media_integrity_failed"
    );
    let inspection = inspect_image(&bytes)?;
    Ok((bytes, inspection))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_receiving_transports_are_allowed() {
        for u in [
            "https://snsvideo.c2c.wechat.com/x",
            "http://wxapp.tc.qq.com/275/20304/stodownload?m=318341a160b08b297892a0e5741cc553&filekey=3035020101042130&bizid=1023",
            "https://wxapp.tc.qq.com:443/x",
        ] {
            assert!(validate_sticker_url(u).is_ok(), "{u}");
        }
        for u in [
            "http://wxapp.tc.qq.com.evil/x",
            "https://wxapp.tc.qq.com.cn/x",
            "http://evil.tc.qq.com/x",
            "http://wxapp.tc.qq.com:8080/x",
        ] {
            assert!(validate_sticker_url(u).is_err(), "{u}");
        }
    }

    #[test]
    fn vweixinf_requires_the_exact_sticker_download_path() {
        assert!(!STICKER_HOSTS.contains(&VWEIXINF_STICKER_HOST));
        for url in [
            "http://vweixinf.tc.qq.com/275/20304/stodownload?m=synthetic&filekey=synthetic",
            "https://vweixinf.tc.qq.com/1/42/stodownload",
            "http://vweixinf.tc.qq.com:80/275/20304/stodownload",
            "https://vweixinf.tc.qq.com:443/275/20304/stodownload",
        ] {
            assert!(validate_sticker_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://vweixinf.tc.qq.com.evil/275/20304/stodownload",
            "http://vweixinf.tc.qq.com/275/20304/other",
            "http://vweixinf.tc.qq.com/275/20304/STODOWNLOAD",
            "http://vweixinf.tc.qq.com/275/20304/stodownload/",
            "http://vweixinf.tc.qq.com/stodownload",
            "http://vweixinf.tc.qq.com/275/stodownload",
            "http://vweixinf.tc.qq.com/275/20304/42/stodownload",
            "http://vweixinf.tc.qq.com//42/stodownload",
            "http://vweixinf.tc.qq.com/275//stodownload",
            "http://vweixinf.tc.qq.com/275/abc/stodownload",
            "http://vweixinf.tc.qq.com/%32/42/stodownload",
            "http://vweixinf.tc.qq.com/275%2F20304/stodownload",
            "http://user@vweixinf.tc.qq.com/275/20304/stodownload",
            "http://vweixinf.tc.qq.com:8080/275/20304/stodownload",
            "http://vweixinf.tc.qq.com:443/275/20304/stodownload",
            "https://vweixinf.tc.qq.com:80/275/20304/stodownload",
            "http://vweixinf.tc.qq.com/275/20304/stodownload#fragment",
        ] {
            assert!(validate_sticker_url(url).is_err(), "{url}");
        }
    }

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

    #[tokio::test]
    async fn async_sticker_download_checks_expiry_md5_status_and_original() {
        let png = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63606060f80f00010401005fe5c34b0000000049454e44ae426082").unwrap();
        let md5_hex = hex::encode(crate::crypto::md5(&png));
        struct FixedClock(u64);
        impl Clock for FixedClock {
            fn monotonic_ms(&self) -> u64 {
                0
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(self.0 * 1000)
            }
        }
        struct StaticHttp(crate::transport::HttpResponse);
        impl HttpTransport for StaticHttp {
            fn send<'a>(
                &'a self,
                _req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                let resp = self.0.clone();
                Box::pin(async move { Ok(resp) })
            }
        }
        let desc = StickerDescriptor {
            url: "https://snsvideo.c2c.wechat.com/sticker".into(),
            expected_bytes: png.len() as u64,
            expected_md5: md5_hex.clone(),
            expires_at: Some(2_000_000_000),
        };
        let ok_http = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![("Content-Length".into(), png.len().to_string())],
            body: png.clone(),
        });
        let (bytes, info) =
            fetch_sticker_async(&desc, &ok_http, &FixedClock(1_700_000_000), |_, _| Ok(()))
                .await
                .unwrap();
        assert_eq!(bytes, png);
        assert_eq!(info.content_type, "image/png");

        let (orig, _) = fetch_sticker_original_async(
            "http://vweixinf.tc.qq.com/275/20304/stodownload?m=1",
            &md5_hex,
            &ok_http,
            |_, _| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(orig, png);

        assert_eq!(
            fetch_sticker_async(&desc, &ok_http, &FixedClock(2_000_000_000), |_, _| Ok(()))
                .await
                .unwrap_err()
                .to_string(),
            "media_expired"
        );

        let err_http = StaticHttp(crate::transport::HttpResponse {
            status: 404,
            headers: vec![],
            body: vec![],
        });
        assert_eq!(
            fetch_sticker_async(&desc, &err_http, &FixedClock(1_700_000_000), |_, _| Ok(()))
                .await
                .unwrap_err()
                .to_string(),
            "sticker_http_status_404"
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
            fetch_sticker_async(&desc, &TooLargeHttp, &FixedClock(1_700_000_000), |_, _| Ok(
                ()
            ))
            .await
            .unwrap_err()
            .to_string(),
            "sticker_response_too_large"
        );
    }
}
