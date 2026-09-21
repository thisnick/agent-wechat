use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::{sync::OnceLock, time::Duration};

const DEFAULT_BROWSER_BRIDGE: &str = "http://127.0.0.1:9223";
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

static CLIENT: OnceLock<Client> = OnceLock::new();

#[derive(Debug, PartialEq)]
pub enum FinderShortLinkError {
    InvalidObjectId,
    InvalidNonceId,
    SessionUnavailable,
    RequestFailed,
    ResponseInvalid,
    RemoteRejected,
    LinkMissing,
}

impl FinderShortLinkError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidObjectId => "FINDER_OBJECT_ID_INVALID",
            Self::InvalidNonceId => "FINDER_NONCE_ID_INVALID",
            Self::SessionUnavailable => "FINDER_SESSION_UNAVAILABLE",
            Self::RequestFailed => "FINDER_SHORT_LINK_REQUEST_FAILED",
            Self::ResponseInvalid => "FINDER_SHORT_LINK_RESPONSE_INVALID",
            Self::RemoteRejected => "FINDER_SHORT_LINK_REMOTE_REJECTED",
            Self::LinkMissing => "FINDER_SHORT_LINK_MISSING",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShortLinkRequest<'a> {
    object_id: &'a str,
    object_nonce_id: &'a str,
    scene: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortLinkResponse {
    short_url: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct FinderBrowserStatus {
    pub status: String,
}

fn valid_object_id(value: &str) -> bool {
    (6..=32).contains(&value.len())
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_nonce_id(value: &str) -> bool {
    (6..=512).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'.' | b'~' | b'+' | b'/' | b'=' | b'-')
        })
}

fn client() -> Result<&'static Client, FinderShortLinkError> {
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let created = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| FinderShortLinkError::RequestFailed)?;
    let _ = CLIENT.set(created);
    CLIENT.get().ok_or(FinderShortLinkError::RequestFailed)
}

fn bridge_url(path: &str) -> Result<Url, FinderShortLinkError> {
    let base =
        std::env::var("FINDER_BROWSER_URL").unwrap_or_else(|_| DEFAULT_BROWSER_BRIDGE.to_string());
    let parsed = Url::parse(&base).map_err(|_| FinderShortLinkError::RequestFailed)?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(FinderShortLinkError::RequestFailed);
    }
    parsed
        .join(path)
        .map_err(|_| FinderShortLinkError::RequestFailed)
}

fn trusted_link(value: &str) -> Option<String> {
    let parsed = Url::parse(value.trim()).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.port(), None | Some(443))
    {
        return None;
    }
    let trusted = matches!(
        (parsed.host_str(), parsed.path()),
        (Some("weixin.qq.com"), path) if path.starts_with("/sph/")
    ) || matches!(
        (parsed.host_str(), parsed.path()),
        (Some("channels.weixin.qq.com"), path)
            if path.starts_with("/finder-preview/pages/sph")
    );
    trusted.then(|| value.trim().to_string())
}

fn map_bridge_error(status: StatusCode, code: Option<&str>) -> FinderShortLinkError {
    match code {
        Some("FINDER_BROWSER_LOGIN_REQUIRED") => FinderShortLinkError::SessionUnavailable,
        Some("FINDER_SHORT_LINK_REMOTE_REJECTED") => FinderShortLinkError::RemoteRejected,
        Some("FINDER_SHORT_LINK_MISSING") => FinderShortLinkError::LinkMissing,
        _ if status == StatusCode::SERVICE_UNAVAILABLE => FinderShortLinkError::SessionUnavailable,
        _ => FinderShortLinkError::RequestFailed,
    }
}

async fn limited_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, FinderShortLinkError> {
    if response.content_length().unwrap_or(0) > MAX_RESPONSE_BYTES {
        return Err(FinderShortLinkError::ResponseInvalid);
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| FinderShortLinkError::RequestFailed)?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(FinderShortLinkError::ResponseInvalid);
    }
    serde_json::from_slice(&body).map_err(|_| FinderShortLinkError::ResponseInvalid)
}

pub async fn resolve_short_link(
    object_id: &str,
    nonce_id: &str,
) -> Result<String, FinderShortLinkError> {
    if !valid_object_id(object_id) {
        return Err(FinderShortLinkError::InvalidObjectId);
    }
    if !valid_nonce_id(nonce_id) {
        return Err(FinderShortLinkError::InvalidNonceId);
    }
    let response = client()?
        .post(bridge_url("/resolve")?)
        .json(&ShortLinkRequest {
            object_id,
            object_nonce_id: nonce_id,
            scene: 40,
        })
        .send()
        .await
        .map_err(|_| FinderShortLinkError::SessionUnavailable)?;
    let status = response.status();
    let payload: ShortLinkResponse = limited_json(response).await?;
    if !status.is_success() {
        return Err(map_bridge_error(status, payload.error.as_deref()));
    }
    payload
        .short_url
        .as_deref()
        .and_then(trusted_link)
        .ok_or(FinderShortLinkError::LinkMissing)
}

pub async fn browser_status() -> Result<FinderBrowserStatus, FinderShortLinkError> {
    let response = client()?
        .get(bridge_url("/status")?)
        .send()
        .await
        .map_err(|_| FinderShortLinkError::SessionUnavailable)?;
    if !response.status().is_success() {
        return Err(FinderShortLinkError::SessionUnavailable);
    }
    limited_json(response).await
}

pub async fn show_browser() -> Result<FinderBrowserStatus, FinderShortLinkError> {
    let response = client()?
        .post(bridge_url("/show")?)
        .send()
        .await
        .map_err(|_| FinderShortLinkError::SessionUnavailable)?;
    if !response.status().is_success() {
        return Err(FinderShortLinkError::SessionUnavailable);
    }
    limited_json(response).await
}

#[cfg(test)]
mod tests {
    use super::{
        bridge_url, map_bridge_error, trusted_link, valid_nonce_id, valid_object_id,
        FinderShortLinkError,
    };
    use reqwest::StatusCode;

    #[test]
    fn validates_exact_finder_identity() {
        assert!(valid_object_id("14726213509764749552"));
        assert!(valid_nonce_id(
            "11529959359054593652_4_20_13_1_1789211747091905_504lish"
        ));
        assert!(!valid_object_id("012345"));
        assert!(!valid_nonce_id("bad cookie"));
    }

    #[test]
    fn bridge_is_loopback_only() {
        std::env::remove_var("FINDER_BROWSER_URL");
        assert_eq!(
            bridge_url("/status").unwrap().as_str(),
            "http://127.0.0.1:9223/status"
        );
        std::env::set_var("FINDER_BROWSER_URL", "https://example.com");
        assert_eq!(
            bridge_url("/status"),
            Err(FinderShortLinkError::RequestFailed)
        );
        std::env::remove_var("FINDER_BROWSER_URL");
    }

    #[test]
    fn accepts_only_official_short_links() {
        assert_eq!(
            trusted_link("https://weixin.qq.com/sph/AbCdEf1234"),
            Some("https://weixin.qq.com/sph/AbCdEf1234".to_string())
        );
        assert_eq!(
            trusted_link("https://channels.weixin.qq.com/finder-preview/pages/sph?id=AbCd"),
            Some("https://channels.weixin.qq.com/finder-preview/pages/sph?id=AbCd".to_string())
        );
        assert_eq!(trusted_link("https://example.com/sph/AbCd"), None);
    }

    #[test]
    fn maps_remote_errors_without_exposing_details() {
        assert_eq!(
            map_bridge_error(
                StatusCode::SERVICE_UNAVAILABLE,
                Some("FINDER_BROWSER_LOGIN_REQUIRED")
            ),
            FinderShortLinkError::SessionUnavailable
        );
        assert_eq!(
            map_bridge_error(
                StatusCode::BAD_GATEWAY,
                Some("FINDER_SHORT_LINK_REMOTE_REJECTED")
            ),
            FinderShortLinkError::RemoteRejected
        );
    }
}
