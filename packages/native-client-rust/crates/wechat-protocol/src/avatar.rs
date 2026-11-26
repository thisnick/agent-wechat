//! Exact server-advertised avatar sources, with bounded pinned public DNS.
use super::*;
#[cfg(not(target_arch = "wasm32"))]
use std::net::ToSocketAddrs;
pub fn validate_avatar_url(value: &str) -> Result<url::Url> {
    let u = url::Url::parse(value).map_err(|_| anyhow::anyhow!("avatar_source_invalid"))?;
    ensure!(
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some_and(|h| [
                "wx.qlogo.cn",
                "mmhead.c2c.wechat.com",
                "mmhead.hk.wechat.com",
                "wework.qpic.cn"
            ]
            .contains(&h))
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.port().is_none(),
        "avatar_source_not_allowed"
    );
    Ok(u)
}
// Legacy blocking downloader kept for 3.x native compatibility (#701).
// Prefer `download_advertised_avatar_async` over `HttpTransport` for new code.
/// Download one contact avatar from a validated WeChat avatar host without redirects.
#[cfg(not(target_arch = "wasm32"))]
pub fn download_advertised_avatar(value: &str) -> Result<Vec<u8>> {
    let url = validate_avatar_url(value)?;
    let host = url.host_str().unwrap().to_string();
    let port = url.port_or_known_default().unwrap();
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| anyhow::anyhow!("avatar_dns_failed"))?
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty()
            && addresses.len() <= 64
            && addresses.iter().all(|a| super::sticker::public_ip(a.ip())),
        "avatar_dns_not_public"
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
        .resolve(&host, *address)
        .build()?;
    let mut response = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .send()
        .map_err(|_| anyhow::anyhow!("avatar_fetch_failed"))?;
    ensure!(response.status() == 200, "avatar_fetch_failed");
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= 8 * 1024 * 1024),
        "avatar_size_limit"
    );
    ensure!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|v| v.as_bytes().eq_ignore_ascii_case(b"identity")),
        "avatar_content_encoding_refused"
    );
    let mut bytes = vec![];
    response
        .by_ref()
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("avatar_fetch_failed"))?;
    ensure!(bytes.len() <= 8 * 1024 * 1024, "avatar_size_limit");
    let inspect = inspect_image(&bytes)?;
    ensure!(
        ["image/jpeg", "image/png"].contains(&inspect.content_type.as_str()),
        "avatar_format_unsupported"
    );
    Ok(bytes)
}

/// Asynchronously download one contact avatar from a validated WeChat avatar
/// host using a pluggable [`HttpTransport`].
///
/// The URL is validated against the WeChat avatar host allowlist before sending.
/// The caller's [`HttpTransport`] must not follow HTTP redirects and is
/// responsible for network/SSRF boundaries.
pub async fn download_advertised_avatar_async(
    value: &str,
    http: &dyn HttpTransport,
) -> Result<Vec<u8>> {
    let url = validate_avatar_url(value)?;
    let response = http
        .send(HttpRequest {
            method: HttpMethod::Get,
            url,
            headers: vec![("Accept-Encoding".into(), "identity".into())],
            body: Vec::new(),
            timeout: Duration::from_secs(20),
            max_response_bytes: 8 * 1024 * 1024,
        })
        .await
        .map_err(|error| {
            super::download_failure(
                &error,
                "avatar_size_limit",
                "avatar_dns_not_public",
                "avatar_fetch_failed",
            )
        })?;
    ensure!(response.status == 200, "avatar_fetch_failed");
    let content_length = response
        .content_length()
        .map_err(|_| anyhow::anyhow!("avatar_fetch_failed"))?;
    ensure!(
        content_length.is_none_or(|n| n <= 8 * 1024 * 1024),
        "avatar_size_limit"
    );
    ensure!(
        response
            .header_values("content-encoding")
            .into_iter()
            .all(|v| v.trim().eq_ignore_ascii_case("identity")),
        "avatar_content_encoding_refused"
    );
    ensure!(response.body.len() <= 8 * 1024 * 1024, "avatar_size_limit");
    ensure!(
        content_length.is_none_or(|n| n == response.body.len() as u64),
        "avatar_fetch_failed"
    );
    let inspect = inspect_image(&response.body)?;
    ensure!(
        ["image/jpeg", "image/png"].contains(&inspect.content_type.as_str()),
        "avatar_format_unsupported"
    );
    Ok(response.body)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_avatar_hosts_only() {
        assert!(validate_avatar_url("https://wx.qlogo.cn/a?opaque=1").is_ok());
        assert!(validate_avatar_url("https://wework.qpic.cn/wwhead/synthetic/100").is_ok());
        for url in [
            "https://wx.qlogo.cn.invalid/a",
            "https://wework.qpic.cn.invalid/a",
            "http://127.0.0.1/a",
            "file:///etc/passwd",
            "https://user@wx.qlogo.cn/a",
            "https://wx.qlogo.cn:8443/a",
        ] {
            assert!(validate_avatar_url(url).is_err());
        }
    }

    #[tokio::test]
    async fn async_avatar_download_checks_status_encoding_size_and_format() {
        let png = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63606060f80f00010401005fe5c34b0000000049454e44ae426082").unwrap();
        struct StaticHttp(crate::transport::HttpResponse);
        impl HttpTransport for StaticHttp {
            fn send<'a>(
                &'a self,
                req: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                assert_eq!(req.method, HttpMethod::Get);
                assert_eq!(req.url.host_str(), Some("wx.qlogo.cn"));
                let resp = self.0.clone();
                Box::pin(async move { Ok(resp) })
            }
        }
        let ok_http = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![("Content-Length".into(), png.len().to_string())],
            body: png.clone(),
        });
        assert_eq!(
            download_advertised_avatar_async("https://wx.qlogo.cn/a", &ok_http)
                .await
                .unwrap(),
            png
        );

        let gzip_http = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![("Content-Encoding".into(), "gzip".into())],
            body: png.clone(),
        });
        assert_eq!(
            download_advertised_avatar_async("https://wx.qlogo.cn/a", &gzip_http)
                .await
                .unwrap_err()
                .to_string(),
            "avatar_content_encoding_refused"
        );

        let redirect_http = StaticHttp(crate::transport::HttpResponse {
            status: 302,
            headers: vec![("Location".into(), "https://evil.invalid/x".into())],
            body: vec![],
        });
        assert_eq!(
            download_advertised_avatar_async("https://wx.qlogo.cn/a", &redirect_http)
                .await
                .unwrap_err()
                .to_string(),
            "avatar_fetch_failed"
        );

        let gif_http = StaticHttp(crate::transport::HttpResponse {
            status: 200,
            headers: vec![],
            body: hex::decode(
                "47494638396101000100800000ffffff0000002c00000000010001000002024401003b",
            )
            .unwrap(),
        });
        assert_eq!(
            download_advertised_avatar_async("https://wx.qlogo.cn/a", &gif_http)
                .await
                .unwrap_err()
                .to_string(),
            "avatar_format_unsupported"
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
            download_advertised_avatar_async("https://wx.qlogo.cn/a", &TooLargeHttp)
                .await
                .unwrap_err()
                .to_string(),
            "avatar_size_limit"
        );
    }
}
