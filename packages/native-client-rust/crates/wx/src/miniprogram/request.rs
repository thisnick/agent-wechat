//! Server domain policy and public HTTPS transport for one local Mini Program launch.

use crate::bounded_bytes;
use anyhow::{ensure, Context, Result};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    Client, Url,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::IpAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub(super) const MAX_REQUEST_BYTES: usize = 512 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_URL_BYTES: usize = 8192;

pub(super) struct Network {
    allowed: Vec<Url>,
    domains: Option<Vec<DomainPattern>>,
    client: Client,
    available: Semaphore,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    url: String,
    method: String,
    header: BTreeMap<String, String>,
    body: Option<String>,
    timeout: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageRequest {
    url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Reply {
    status_code: u16,
    header: BTreeMap<String, String>,
    cookies: Vec<String>,
    body: String,
}

impl Network {
    pub(super) fn new(domains: Option<Vec<String>>, urls: Vec<String>) -> Result<Option<Self>> {
        ensure!(
            urls.len() <= 256,
            "Too many allowed Mini Program request URLs"
        );
        let allowed = urls
            .iter()
            .map(|value| {
                let url = public_https_url(value).context("Invalid Mini Program request URL")?;
                ensure!(
                    url.query().is_none(),
                    "Allowed request URLs must omit the query"
                );
                Ok(url)
            })
            .collect::<Result<_>>()?;
        let domains = domains.map(|values| {
            values
                .iter()
                .filter_map(|value| DomainPattern::parse(value))
                .collect()
        });
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .dns_resolver(Arc::new(PublicDns))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Some(Self {
            allowed,
            domains,
            client,
            available: Semaphore::new(10),
        }))
    }

    pub(super) fn requests_enabled(&self) -> bool {
        self.domains
            .as_ref()
            .is_some_and(|domains| !domains.is_empty())
    }

    pub(super) async fn image(&self, input: ImageRequest) -> Result<Reply, StatusCode> {
        let url = public_https_url(&input.url).ok_or(StatusCode::BAD_REQUEST)?;
        if !self.allowed.is_empty()
            && !self
                .allowed
                .iter()
                .any(|allowed| same_endpoint(allowed, &url))
        {
            return Err(StatusCode::FORBIDDEN);
        }
        let mut reply = self
            .fetch(Request {
                url: input.url,
                method: "GET".into(),
                header: BTreeMap::new(),
                body: None,
                timeout: Some(20_000),
            })
            .await?;
        if !(200..300).contains(&reply.status_code) {
            return Err(StatusCode::BAD_GATEWAY);
        }
        let bytes = STANDARD
            .decode(&reply.body)
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        let mime = image_mime(&bytes, reply.header.get("content-type").map(String::as_str))
            .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        reply.header = BTreeMap::from([("content-type".into(), mime.into())]);
        reply.cookies.clear();
        Ok(reply)
    }

    pub(super) async fn send(&self, request: Request) -> Result<Reply, StatusCode> {
        let url = public_https_url(&request.url).ok_or(StatusCode::BAD_REQUEST)?;
        let allowed = match &self.domains {
            Some(domains) => {
                domains.iter().any(|pattern| pattern.matches(&url))
                    && (self.allowed.is_empty()
                        || self
                            .allowed
                            .iter()
                            .any(|allowed| same_endpoint(allowed, &url)))
            }
            None => false,
        };
        if !allowed {
            return Err(StatusCode::FORBIDDEN);
        }
        self.fetch(request).await
    }

    async fn fetch(&self, request: Request) -> Result<Reply, StatusCode> {
        let url = public_https_url(&request.url).ok_or(StatusCode::BAD_REQUEST)?;
        let method = Method::from_bytes(request.method.as_bytes())
            .ok()
            .filter(|method| {
                matches!(
                    *method,
                    Method::GET
                        | Method::HEAD
                        | Method::POST
                        | Method::PUT
                        | Method::DELETE
                        | Method::OPTIONS
                )
            })
            .ok_or(StatusCode::BAD_REQUEST)?;
        if request.header.len() > 64
            || (matches!(method, Method::GET | Method::HEAD) && request.body.is_some())
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let _permit = self
            .available
            .try_acquire()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        let mut outgoing = self
            .client
            .request(method, url)
            .timeout(Duration::from_millis(
                request.timeout.unwrap_or(60_000).clamp(1, 60_000),
            ));
        for (name, value) in request.header {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?;
            if !app_header(&name) {
                continue;
            }
            let value = HeaderValue::from_str(&value).map_err(|_| StatusCode::BAD_REQUEST)?;
            outgoing = outgoing.header(name, value);
        }
        if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }
        let response = outgoing.send().await.map_err(|error| {
            if error.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            }
        })?;
        let status_code = response.status().as_u16();
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect();
        let header = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                if *name == header::SET_COOKIE || !app_header(name) {
                    return None;
                }
                Some((name.to_string(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        let body = bounded_bytes(response, MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        Ok(Reply {
            status_code,
            header,
            cookies,
            body: STANDARD.encode(body),
        })
    }
}

fn image_mime(bytes: &[u8], content_type: Option<&str>) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[255, 216, 255]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if !bytes.is_empty()
        && content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("image/svg+xml")
        })
    {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn public_https_url(input: &str) -> Option<Url> {
    if input.len() > MAX_URL_BYTES || !input.starts_with("https://") {
        return None;
    }
    let url = Url::parse(input).ok()?;
    let domain = url.domain()?;
    if !domain.contains('.')
        || domain.ends_with('.')
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}

fn same_endpoint(allowed: &Url, url: &Url) -> bool {
    allowed.origin() == url.origin() && allowed.path() == url.path()
}

/// The host's API-domain check matches schemes and host suffixes, not paths
/// or ports. Transport-level public-address and HTTPS checks still apply.
struct DomainPattern {
    scheme: Option<String>,
    host: String,
    subdomains_only: bool,
}

impl DomainPattern {
    fn parse(value: &str) -> Option<Self> {
        if value.len() > MAX_URL_BYTES {
            return None;
        }
        if let Some(host) = value.strip_prefix("*.") {
            // Parse the hostname to normalize case/IDNA without accepting a URL,
            // user info, path, query, fragment or port in this branch.
            let url = Url::parse(&format!("https://{host}")).ok()?;
            let normalized = url.domain()?;
            if !host.contains('.')
                || host.contains(['/', ':', '@', '?', '#'])
                || normalized.ends_with('.')
            {
                return None;
            }
            return Some(Self {
                scheme: None,
                host: normalized.to_owned(),
                subdomains_only: true,
            });
        }
        let url = public_https_url(value)?;
        Some(Self {
            scheme: Some(url.scheme().to_owned()),
            host: url.domain()?.to_owned(),
            subdomains_only: false,
        })
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.domain() else {
            return false;
        };
        if self
            .scheme
            .as_deref()
            .is_some_and(|scheme| scheme != url.scheme())
        {
            return false;
        }
        (!self.subdomains_only && host == self.host)
            || host
                .strip_suffix(&self.host)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

fn app_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    !matches!(
        name,
        "host"
            | "origin"
            | "referer"
            | "connection"
            | "keep-alive"
            | "content-length"
            | "transfer-encoding"
            | "upgrade"
            | "te"
            | "trailer"
            | "accept-encoding"
    ) && !name.starts_with("proxy-")
        && !name.starts_with("sec-")
        && !name.starts_with("x-wx-launch")
}

struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(std::io::Error::other("No public Mini Program address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !matches!(
                    ip.octets(),
                    [0 | 224..=255, ..]
                        | [100, 64..=127, ..]
                        | [192, 0, 0, _]
                        | [192, 88, 99, _]
                        | [198, 18 | 19, ..]
                )
        }
        IpAddr::V6(ip) => {
            matches!(ip.segments(), [0x2000..=0x3fff, ..])
                && !matches!(
                    ip.segments(),
                    [0x2001, 0..=0x1ff | 0xdb8, ..] | [0x2002 | 0x3fff, ..]
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_configured_https_origin_and_path() {
        let allowed = public_https_url("https://example.com:8443/mini/login").unwrap();
        let matches =
            |value| public_https_url(value).is_some_and(|url| same_endpoint(&allowed, &url));
        assert!(matches("https://example.com:8443/mini/login?a=b"));
        for rejected in [
            "http://example.com:8443/mini/login",
            "https://example.com/mini/login",
            "https://other.example.com:8443/mini/login",
            "https://example.com:8443/mini/login/extra",
            "https://example.com:8443/mini/login#fragment",
            "https://user@example.com:8443/mini/login",
            "https://127.0.0.1:8443/mini/login",
        ] {
            assert!(!matches(rejected));
        }
    }

    #[test]
    fn server_domains_match_host_boundaries_instead_of_paths_or_ports() {
        let rule = DomainPattern::parse("https://example.com").unwrap();
        for accepted in [
            "https://example.com/any/path?q=1",
            "https://sub.example.com:8443/other",
            "https://a.b.example.com/another",
        ] {
            assert!(rule.matches(&Url::parse(accepted).unwrap()), "{accepted}");
        }
        for rejected in [
            "http://example.com/",
            "https://notexample.com/",
            "https://example.com.attacker.invalid/",
        ] {
            assert!(!rule.matches(&Url::parse(rejected).unwrap()), "{rejected}");
        }
        let wildcard = DomainPattern::parse("*.example.com").unwrap();
        assert!(wildcard.matches(&Url::parse("https://sub.example.com:8443/a").unwrap()));
        assert!(!wildcard.matches(&Url::parse("https://example.com/a").unwrap()));
        for invalid in [
            "*",
            "*.com",
            "*.example.com/path",
            "*.user@example.com",
            "https://127.0.0.1",
        ] {
            assert!(DomainPattern::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn image_bridge_accepts_only_display_content_and_no_request_options() {
        assert_eq!(
            image_mime(b"GIF89a", Some("application/octet-stream")),
            Some("image/gif")
        );
        assert_eq!(
            image_mime(b"<svg/>", Some("image/svg+xml; charset=utf-8")),
            Some("image/svg+xml")
        );
        assert_eq!(
            image_mime(b"<html>private data</html>", Some("text/html")),
            None
        );
        assert_eq!(image_mime(b"{}", Some("application/json")), None);
        assert!(serde_json::from_str::<ImageRequest>(
            r#"{"url":"https://example.com/a.png","header":{"Authorization":"secret"}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ImageRequest>(
            r#"{"url":"https://example.com/a.png","method":"POST"}"#
        )
        .is_err());
    }

    #[test]
    fn filters_private_and_special_use_addresses_before_connecting() {
        for rejected in [
            "0.0.0.0",
            "127.1.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::ffff:10.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
        ] {
            assert!(!is_public(rejected.parse().unwrap()), "{rejected}");
        }
        for public in ["1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }
}
