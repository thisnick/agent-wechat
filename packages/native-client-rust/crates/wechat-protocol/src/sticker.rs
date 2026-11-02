//! Original type-47 bytes from the verified CDN host. Signed URLs remain private.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    net::{IpAddr, ToSocketAddrs},
    time::Duration,
};
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
/// Sticker bytes come from Tencent CDN hosts. Classic stored and pack emoji
/// carry a `wxapp.tc.qq.com` source, while shared stickers use
/// `snsvideo.c2c.wechat.com`; both are reachable directly, unlike the
/// encrypted attachment channels. Any other host stays refused so a crafted
/// message cannot turn this into an arbitrary fetch.
pub const STICKER_HOSTS: &[&str] = &["snsvideo.c2c.wechat.com", "wxapp.tc.qq.com"];

pub fn validate_sticker_url(value: &str) -> Result<reqwest::Url> {
    ensure!(
        value.len() <= 8192 && !value.chars().any(|c| c.is_control()),
        "sticker_source_not_allowed"
    );
    let url =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("sticker_source_not_allowed"))?;
    ensure!(
        ["http", "https"].contains(&url.scheme())
            && url
                .host_str()
                .is_some_and(|host| STICKER_HOSTS.contains(&host))
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
}
