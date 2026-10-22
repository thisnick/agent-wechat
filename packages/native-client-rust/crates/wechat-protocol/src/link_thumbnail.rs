//! Fetch only a card's advertised thumbnail, without opening its target page.
use anyhow::{ensure, Result};
use std::{
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    time::Duration,
};
pub const MAX_LINK_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;

pub fn validate_shared_link_url(value: &str) -> Result<reqwest::Url> {
    ensure!(
        !value.is_empty() && value.len() <= 8192 && !value.chars().any(|c| c.is_control()),
        "link_source_not_allowed"
    );
    let url = reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("link_source_not_allowed"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "link_source_not_allowed"
    );
    Ok(url)
}
pub fn validate_link_thumbnail_url(value: &str) -> Result<reqwest::Url> {
    let url = validate_shared_link_url(value)?;
    ensure!(
        url.fragment().is_none() && url.port().is_none(),
        "link_thumbnail_source_not_allowed"
    );
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    if let Ok(ip) = host.parse::<IpAddr>() {
        ensure!(
            super::sticker::public_ip(ip),
            "link_thumbnail_dns_not_public"
        );
    }
    Ok(url)
}
pub fn fetch_link_thumbnail(
    value: &str,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<Vec<u8>> {
    let url = validate_link_thumbnail_url(value)?;
    let host = url.host_str().unwrap().trim_matches(['[', ']']).to_owned();
    let port = url.port_or_known_default().unwrap();
    let addresses = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| anyhow::anyhow!("link_thumbnail_dns_failed"))?
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty()
            && addresses.len() <= 64
            && addresses.iter().all(|a| super::sticker::public_ip(a.ip())),
        "link_thumbnail_dns_not_public"
    );
    let address = addresses
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addresses.first())
        .unwrap();
    fetch_at(url, *address, progress)
}
fn fetch_at(
    url: reqwest::Url,
    address: SocketAddr,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<Vec<u8>> {
    let host = url.host_str().unwrap().to_owned();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve(&host, address)
        .build()?;
    let mut response = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .send()
        .map_err(|_| anyhow::anyhow!("link_thumbnail_fetch_failed"))?;
    ensure!(response.status() == 200, "link_thumbnail_fetch_failed");
    ensure!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|v| v.as_bytes().eq_ignore_ascii_case(b"identity")),
        "link_thumbnail_encoding_refused"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_LINK_THUMBNAIL_BYTES as u64),
        "link_thumbnail_size_limit"
    );
    let total = response.content_length();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        progress(bytes.len() as u64, total)?;
        let n = response
            .read(&mut chunk)
            .map_err(|_| anyhow::anyhow!("link_thumbnail_fetch_failed"))?;
        if n == 0 {
            break;
        }
        ensure!(
            bytes.len() + n <= MAX_LINK_THUMBNAIL_BYTES,
            "link_thumbnail_size_limit"
        );
        bytes.extend_from_slice(&chunk[..n]);
    }
    ensure!(
        !bytes.is_empty() && total.is_none_or(|n| n == bytes.len() as u64),
        "media_integrity_failed"
    );
    super::inspect_image(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_sources_are_public_http_without_credentials_or_redirect_targets() {
        assert!(validate_link_thumbnail_url("https://example.invalid/preview.jpg?a=1&b=2").is_ok());
        for value in [
            "file:///etc/passwd",
            "https://user:pass@example.invalid/x",
            "http://127.0.0.1/x",
            "http://[::1]/x",
            "http://169.254.169.254/x",
            "https://example.invalid:8443/x",
            "https://example.invalid/x#fragment",
            "https://example.invalid/\nx",
        ] {
            assert!(validate_link_thumbnail_url(value).is_err(), "{value}");
        }
    }
    #[test]
    fn http_thumbnail_checks_status_encoding_bounds_and_image_bytes_without_redirects() {
        use std::{io::Write, net::TcpListener};
        let image = hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c63606060f80f00010401005fe5c34b0000000049454e44ae426082").unwrap();
        for (status, headers, body, expected) in [
            (
                "200 OK",
                format!("Content-Length: {}\r\n", image.len()),
                image.clone(),
                None,
            ),
            (
                "302 Found",
                "Location: http://127.0.0.1/target\r\nContent-Length: 0\r\n".into(),
                vec![],
                Some("link_thumbnail_fetch_failed"),
            ),
            (
                "200 OK",
                "Content-Encoding: gzip\r\nContent-Length: 1\r\n".into(),
                vec![0],
                Some("link_thumbnail_encoding_refused"),
            ),
            (
                "200 OK",
                "Content-Length: 8388609\r\n".into(),
                vec![],
                Some("link_thumbnail_size_limit"),
            ),
            (
                "200 OK",
                "".into(),
                vec![0; MAX_LINK_THUMBNAIL_BYTES + 1],
                Some("link_thumbnail_size_limit"),
            ),
            (
                "200 OK",
                "Content-Length: 4\r\n".into(),
                b"html".to_vec(),
                Some("unsupported"),
            ),
        ] {
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
                assert!(String::from_utf8(request)
                    .unwrap()
                    .starts_with("GET /preview HTTP/1.1\r\n"));
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").as_bytes(),
                );
                let _ = stream.write_all(&body);
            });
            // Only this private transport helper accepts a local test socket;
            // the public entry point validates and pins public DNS answers.
            let result = fetch_at(
                reqwest::Url::parse(&format!(
                    "http://preview.example.invalid:{}/preview",
                    address.port()
                ))
                .unwrap(),
                address,
                |_, _| Ok(()),
            );
            server.join().unwrap();
            if let Some(code) = expected {
                assert!(result.unwrap_err().to_string().contains(code));
            } else {
                assert_eq!(result.unwrap(), image);
            }
        }
    }
}
