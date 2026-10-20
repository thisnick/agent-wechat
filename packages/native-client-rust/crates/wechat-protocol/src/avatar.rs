//! Exact server-advertised avatar sources, with bounded pinned public DNS.
use super::*;
use std::net::ToSocketAddrs;
pub fn validate_avatar_url(value: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("avatar_source_invalid"))?;
    ensure!(
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some_and(|h| [
                "wx.qlogo.cn",
                "mmhead.c2c.wechat.com",
                "mmhead.hk.wechat.com"
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_avatar_hosts_only() {
        assert!(validate_avatar_url("https://wx.qlogo.cn/a?opaque=1").is_ok());
        for url in [
            "https://wx.qlogo.cn.invalid/a",
            "http://127.0.0.1/a",
            "file:///etc/passwd",
            "https://user@wx.qlogo.cn/a",
            "https://wx.qlogo.cn:8443/a",
        ] {
            assert!(validate_avatar_url(url).is_err());
        }
    }
}
