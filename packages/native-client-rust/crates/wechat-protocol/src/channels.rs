//! Authenticated lookup of a WeChat Channels post's playable video.
//!
//! A received Channels card names its post by object ID and nonce. Fresh playback material (a
//! signed address, its token and the prefix decode key) comes from three authenticated requests:
//! an H5 ticket, an authenticated page address for the post, and the post detail carried through
//! the H5 transfer proxy. Each request is submitted once; callers decide whether to renew and
//! retry. Tickets, page addresses, tokens and keys are credential material: they never appear in
//! `Debug` output or error text.
use super::*;
use base64::Engine as _;
use zeroize::Zeroizing;

const H5_AUTH_CGI: u16 = 5805;
const H5_AUTH_PATH: &str = "/cgi-bin/micromsg-bin/h5auth";
/// Unsigned H5 context the client sends with its H5 ticket request.
const H5_AUTH_CONTEXT: u64 = 0xe100_1000;
const PAGE_CGI: u16 = 233;
const PAGE_PATH: &str = "/cgi-bin/micromsg-bin/geta8key";
const TRANSFER_CGI: u16 = 6673;
const TRANSFER_PATH: &str = "/cgi-bin/micromsg-bin/transfer/h5exttransfer";
const TRANSFER_SCOPE: &str = "finderLive";
/// Proxy control value; unrelated to the H5 context above.
const TRANSFER_CONTROL: u64 = 400;
const DETAIL_ROUTE: &str = "/cgi-bin/micromsg-bin/pc_findergetcommentdetail";
const DETAIL_COMMAND: u64 = 5259;
const PAGE_BASE: &str = "https://channels.weixin.qq.com/web/pages/feed";
const MAX_CREDENTIAL_TEXT: usize = 8192;
const MAX_DETAIL_JSON: usize = 4 * 1024 * 1024;

/// A post this client may resolve: its object ID and the card's full nonce.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelsPost {
    /// Post object ID from the card.
    pub object_id: u64,
    /// The card's full `objectNonceId`; requests use its numeric prefix.
    pub nonce: String,
}

impl fmt::Debug for ChannelsPost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChannelsPost(<redacted>)")
    }
}

impl ChannelsPost {
    /// Numeric prefix of the nonce, before the first `_`.
    pub fn nonce_number(&self) -> Result<u64> {
        let prefix = self.nonce.split('_').next().unwrap_or_default();
        ensure!(
            !prefix.is_empty() && prefix.len() <= 20 && prefix.bytes().all(|b| b.is_ascii_digit()),
            "channels_unsupported_media"
        );
        prefix
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("channels_unsupported_media"))
    }

    /// The unsigned page address for the post, as the client builds it locally.
    pub fn page_url(&self) -> Result<String> {
        ensure!(self.object_id != 0, "channels_unsupported_media");
        let encode = |value: u64| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_be_bytes())
        };
        Ok(format!(
            "{PAGE_BASE}?oid={}&nid={}&entrance_id=1002",
            encode(self.object_id),
            encode(self.nonce_number()?)
        ))
    }
}

/// A current H5 ticket for the transfer proxy.
pub struct ChannelsTicket {
    ticket: Zeroizing<String>,
    /// Seconds the ticket stays valid, when the reply says.
    pub lifetime_seconds: Option<u64>,
}

impl fmt::Debug for ChannelsTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelsTicket")
            .field("ticket", &"<redacted>")
            .field("lifetime_seconds", &self.lifetime_seconds)
            .finish()
    }
}

impl Clone for ChannelsTicket {
    fn clone(&self) -> Self {
        Self {
            ticket: Zeroizing::new(self.ticket.as_str().to_owned()),
            lifetime_seconds: self.lifetime_seconds,
        }
    }
}

/// The authenticated page address for one post. Credential material, not a share link.
pub struct ChannelsPage(Zeroizing<String>);

impl fmt::Debug for ChannelsPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChannelsPage(<redacted>)")
    }
}

/// Fresh playback material for a post's single video, kept together as returned.
pub struct ChannelsMedia {
    url: Zeroizing<String>,
    url_token: Zeroizing<String>,
    decode_key: u64,
    /// Byte length the reply advertises for the video, when present.
    pub file_size: Option<u64>,
    /// Byte length the reply advertises for the CDN copy, when present.
    pub cdn_file_size: Option<u64>,
    /// Video width in pixels, when present.
    pub width: Option<u32>,
    /// Video height in pixels, when present.
    pub height: Option<u32>,
    /// Play length in seconds, when present.
    pub duration_seconds: Option<u32>,
}

impl fmt::Debug for ChannelsMedia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelsMedia")
            .field("material", &"<redacted>")
            .field("file_size", &self.file_size)
            .field("cdn_file_size", &self.cdn_file_size)
            .finish()
    }
}

impl Drop for ChannelsMedia {
    fn drop(&mut self) {
        self.decode_key = 0;
    }
}

impl ChannelsMedia {
    /// Synthetic material for tests of code that downloads or decodes a video.
    #[doc(hidden)]
    pub fn for_tests(url: &str, url_token: &str, decode_key: u64, file_size: Option<u64>) -> Self {
        Self {
            url: Zeroizing::new(url.into()),
            url_token: Zeroizing::new(url_token.into()),
            decode_key,
            file_size,
            cdn_file_size: None,
            width: None,
            height: None,
            duration_seconds: None,
        }
    }
    /// Address and token as returned, for building the download request.
    pub fn transport(&self) -> (&str, &str) {
        (&self.url, &self.url_token)
    }
    /// Seed of the prefix keystream.
    pub fn decode_key(&self) -> u64 {
        self.decode_key
    }
}

fn text(value: &[u8], max: usize) -> Result<Zeroizing<String>> {
    let value = std::str::from_utf8(value).map_err(|_| anyhow::anyhow!("channels_rejected"))?;
    ensure!(
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control),
        "channels_rejected"
    );
    Ok(Zeroizing::new(value.to_owned()))
}

fn h5_auth_request(state: &AccountState) -> Result<Vec<u8>> {
    state.validate()?;
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::uint(2, H5_AUTH_CONTEXT),
    ])
}

fn h5_auth_response(bytes: &[u8]) -> Result<ChannelsTicket> {
    let fields = p::decode(bytes)?;
    ServiceRejected::check(Service::ChannelsAuthorization, p::base_code(&fields)?)?;
    let ticket = text(
        p::optional_data(&fields, 2)?.ok_or_else(|| anyhow::anyhow!("channels_rejected"))?,
        MAX_CREDENTIAL_TEXT,
    )?;
    let lifetime_seconds = match p::optional(&fields, 3)? {
        Some(p::Value::Varint(value)) => Some(*value),
        _ => None,
    };
    // Field 5 repeats one transfer route per API scope. Only the fixed route this client uses
    // is accepted; a server-supplied path is never used as a destination.
    let mut routed = false;
    for field in fields.iter().filter(|field| field.number == 5) {
        let p::Value::Bytes(entry) = &field.value else {
            bail!("channels_rejected");
        };
        let entry = p::decode(entry)?;
        let scope = p::optional_data(&entry, 1)?;
        if scope != Some(TRANSFER_SCOPE.as_bytes()) {
            continue;
        }
        let cgi = p::scalar(&entry, 2)?;
        let path = p::optional_data(&entry, 3)?;
        ensure!(
            cgi == u64::from(TRANSFER_CGI) && path == Some(TRANSFER_PATH.as_bytes()),
            "channels_route_unsupported"
        );
        routed = true;
    }
    ensure!(routed, "channels_route_unsupported");
    Ok(ChannelsTicket {
        ticket,
        lifetime_seconds,
    })
}

fn page_request(state: &AccountState, post: &ChannelsPost) -> Result<Vec<u8>> {
    state.validate()?;
    let url = post.page_url()?;
    p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::uint(2, 2),
        p::bytes(7, url.as_bytes()),
        p::uint(10, 7),
        p::uint(14, 0),
        p::uint(16, 0),
        p::uint(18, 0),
        p::uint(19, 0),
        p::uint(20, u64::from(state.uin)),
        p::uint(33, 0),
        p::uint(34, 0),
        p::bytes(39, b"en_US"),
    ])
}

fn page_response(bytes: &[u8]) -> Result<ChannelsPage> {
    let fields = p::decode(bytes)?;
    ServiceRejected::check(Service::ChannelsPage, p::base_code(&fields)?)?;
    let url = text(
        p::optional_data(&fields, 2)?.ok_or_else(|| anyhow::anyhow!("channels_rejected"))?,
        MAX_CREDENTIAL_TEXT,
    )?;
    ensure!(
        url.starts_with("https://channels.weixin.qq.com/"),
        "channels_rejected"
    );
    Ok(ChannelsPage(url))
}

fn detail_json(post: &ChannelsPost) -> Result<String> {
    let request_id: u64 = u64::from_be_bytes(crate::crypto::random::<8>()?) >> 1;
    Ok(serde_json::json!({
        "direction": 2,
        "finder_basereq": {
            "ctx_info": {"client_report_buff": "{\"entranceId\":\"1002\"}"},
            "expt_flag": 1,
            "request_id": request_id.to_string(),
            "scene": 140,
        },
        "identity_scene": 2,
        "last_buffer": "",
        "need_object": 1,
        "object_nonce_id": post.nonce_number()?.to_string(),
        "objectid": post.object_id.to_string(),
        "platform_scene": 2,
        "pull_scene": 6,
        "scene": 146,
        "tolang": "en",
    })
    .to_string())
}

fn transfer_request(
    state: &AccountState,
    ticket: &ChannelsTicket,
    page: &ChannelsPage,
    json: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    state.validate()?;
    let inner = p::message(&[
        p::bytes(1, DETAIL_ROUTE.as_bytes()),
        p::bytes(2, json.as_bytes()),
        p::uint(3, DETAIL_COMMAND),
    ])?;
    Ok(Zeroizing::new(p::message(&[
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &inner),
        p::bytes(3, ticket.ticket.as_bytes()),
        p::bytes(5, page.0.as_bytes()),
        p::uint(6, TRANSFER_CONTROL),
        p::bytes(7, TRANSFER_SCOPE.as_bytes()),
        p::uint(10, 0),
    ])?))
}

fn transfer_response(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let fields = p::decode(bytes)?;
    ServiceRejected::check(Service::Channels, p::base_code(&fields)?)?;
    let inner = p::decode(p::data(&fields, 2)?)?;
    let status = p::optional(&inner, 1)?
        .map(|value| match value {
            p::Value::Varint(raw) => Ok(*raw as u32 as i32),
            _ => Err(anyhow::anyhow!("channels_rejected")),
        })
        .transpose()?
        .unwrap_or(0);
    ServiceRejected::check(Service::Channels, status)?;
    let json = p::optional_data(&inner, 3)?.ok_or_else(|| anyhow::anyhow!("channels_rejected"))?;
    ensure!(json.len() <= MAX_DETAIL_JSON, "channels_rejected");
    Ok(Zeroizing::new(json.to_vec()))
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn json_u32(value: Option<&serde_json::Value>) -> Option<u32> {
    value
        .and_then(json_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// Read the detail reply. It must name the requested post and carry exactly one video entry with
/// usable playback material.
fn parse_detail(json: &[u8], post: &ChannelsPost) -> Result<ChannelsMedia> {
    let reply: serde_json::Value =
        serde_json::from_slice(json).map_err(|_| anyhow::anyhow!("channels_rejected"))?;
    let ret = reply
        .pointer("/BaseResponse/Ret")
        .or_else(|| reply.pointer("/base_response/ret"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    ensure!(ret == 0, "channels_post_unavailable");
    let object = reply
        .get("object")
        .filter(|object| object.is_object())
        .ok_or_else(|| anyhow::anyhow!("channels_post_unavailable"))?;
    ensure!(
        object.get("id").and_then(json_u64) == Some(post.object_id),
        "channels_unsupported_media"
    );
    let media = object
        .pointer("/object_desc/media")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("channels_unsupported_media"))?;
    let [entry] = media.as_slice() else {
        bail!("channels_unsupported_media");
    };
    let string = |name: &str| {
        entry
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= MAX_CREDENTIAL_TEXT)
            .filter(|s| !s.chars().any(char::is_control))
            .map(|s| Zeroizing::new(s.to_owned()))
    };
    let url = string("url").ok_or_else(|| anyhow::anyhow!("channels_unsupported_media"))?;
    let url_token = string("url_token").unwrap_or_else(|| Zeroizing::new(String::new()));
    // A seed of "0" is valid; only an absent or malformed key is rejected.
    let decode_key = entry
        .get("decode_key")
        .and_then(json_u64)
        .ok_or_else(|| anyhow::anyhow!("channels_unsupported_media"))?;
    let size = |name: &str| entry.get(name).and_then(json_u64).filter(|n| *n > 0);
    Ok(ChannelsMedia {
        url,
        url_token,
        decode_key,
        file_size: size("file_size"),
        cdn_file_size: size("cdn_file_size"),
        width: json_u32(entry.get("width")),
        height: json_u32(entry.get("height")),
        duration_seconds: json_u32(entry.get("video_play_len")),
    })
}

impl NativeClient {
    /// Request a new H5 ticket. Submitted once, without renewal or retry.
    #[doc(hidden)]
    pub(crate) fn channels_ticket_snapshot(
        &self,
        state: &mut AccountState,
    ) -> Result<ChannelsTicket> {
        let body = h5_auth_request(state)?;
        h5_auth_response(&self.authenticated_rpc(state, H5_AUTH_CGI, H5_AUTH_PATH, &body)?)
    }

    /// Request the authenticated page address for a post. Submitted once.
    #[doc(hidden)]
    pub(crate) fn channels_page_snapshot(
        &self,
        state: &mut AccountState,
        post: &ChannelsPost,
    ) -> Result<ChannelsPage> {
        let body = page_request(state, post)?;
        page_response(&self.authenticated_rpc(state, PAGE_CGI, PAGE_PATH, &body)?)
    }

    /// Request the post detail and return its single video's playback material. Submitted once.
    #[doc(hidden)]
    pub(crate) fn channels_media_snapshot(
        &self,
        state: &mut AccountState,
        post: &ChannelsPost,
        ticket: &ChannelsTicket,
        page: &ChannelsPage,
    ) -> Result<ChannelsMedia> {
        let json = Zeroizing::new(detail_json(post)?);
        let body = transfer_request(state, ticket, page, &json)?;
        let reply = transfer_response(&Zeroizing::new(self.authenticated_rpc(
            state,
            TRANSFER_CGI,
            TRANSFER_PATH,
            &body,
        )?))?;
        parse_detail(&reply, post)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post() -> ChannelsPost {
        ChannelsPost {
            object_id: 18_446_744_073_709_551_000,
            nonce: "12345678901234567890_0_38_12_4_context".into(),
        }
    }

    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: b"0123456789abcdef".to_vec(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 123_456,
            own_username: "synthetic_user".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![4; 16],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }

    fn base_response(code: i64) -> Vec<u8> {
        p::bytes(1, &p::uint(1, code as u64))
    }

    #[test]
    fn page_url_encodes_big_endian_ids_and_the_numeric_nonce() {
        let url = post().page_url().unwrap();
        let expected = |value: u64| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_be_bytes())
        };
        assert_eq!(
            url,
            format!(
                "https://channels.weixin.qq.com/web/pages/feed?oid={}&nid={}&entrance_id=1002",
                expected(18_446_744_073_709_551_000),
                expected(12_345_678_901_234_567_890)
            )
        );
        assert_eq!(expected(1), "AAAAAAAAAAE");
        for nonce in ["", "_1", "abc_1", "184467440737095516160_1"] {
            let bad = ChannelsPost {
                object_id: 1,
                nonce: nonce.into(),
            };
            assert!(bad.page_url().is_err(), "{nonce}");
        }
        assert_eq!(format!("{:?}", post()), "ChannelsPost(<redacted>)");
    }

    #[test]
    fn ticket_request_sends_the_h5_context_as_an_unsigned_varint() {
        let state = state();
        let fields = p::decode(&h5_auth_request(&state).unwrap()).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(p::scalar(&fields, 2).unwrap(), 3_774_877_696);
        // An unsigned uint32 varint takes five bytes; a sign-extended int32 would take ten.
        assert!(h5_auth_request(&state)
            .unwrap()
            .ends_with(&[0x10, 0x80, 0xa0, 0x80, 0x88, 0x0e]));
    }

    fn route(scope: &str, cgi: u64, path: &str) -> Vec<u8> {
        p::bytes(
            5,
            &p::message(&[
                p::bytes(1, scope.as_bytes()),
                p::uint(2, cgi),
                p::bytes(3, path.as_bytes()),
            ])
            .unwrap(),
        )
    }

    #[test]
    fn ticket_reply_requires_the_known_route_and_keeps_the_ticket_private() {
        let good = p::message(&[
            base_response(0),
            p::bytes(2, b"dGlja2V0"),
            p::uint(3, 7200),
            route("finderInteraction", 6673, TRANSFER_PATH),
            route("finderLive", 6673, TRANSFER_PATH),
            p::uint(6, 1),
        ])
        .unwrap();
        let ticket = h5_auth_response(&good).unwrap();
        assert_eq!(ticket.ticket.as_str(), "dGlja2V0");
        assert_eq!(ticket.lifetime_seconds, Some(7200));
        assert!(!format!("{ticket:?}").contains("dGlja2V0"));

        for routes in [
            vec![],
            vec![route("finderLive", 6674, TRANSFER_PATH)],
            vec![route("finderLive", 6673, "/cgi-bin/elsewhere")],
        ] {
            let mut parts = vec![base_response(0), p::bytes(2, b"dGlja2V0")];
            parts.extend(routes);
            let error = h5_auth_response(&p::message(&parts).unwrap()).unwrap_err();
            assert_eq!(error.to_string(), "channels_route_unsupported");
        }

        let rejected = p::message(&[base_response(-13)]).unwrap();
        let error = h5_auth_response(&rejected).unwrap_err();
        let failure = ServiceRejected::find(&error).unwrap();
        assert_eq!(
            (failure.service, failure.code),
            (Service::ChannelsAuthorization, -13)
        );
    }

    #[test]
    fn page_request_carries_the_unsigned_page_and_account() {
        let state = state();
        let fields = p::decode(&page_request(&state, &post()).unwrap()).unwrap();
        let numbers: Vec<u32> = fields.iter().map(|f| f.number).collect();
        assert_eq!(
            numbers,
            [1, 2, 7, 10, 14, 16, 18, 19, 20, 33, 34, 39],
            "field order"
        );
        assert_eq!(p::scalar(&fields, 2).unwrap(), 2);
        assert_eq!(p::scalar(&fields, 10).unwrap(), 7);
        assert_eq!(p::scalar(&fields, 20).unwrap(), 123_456);
        assert_eq!(
            p::data(&fields, 7).unwrap(),
            post().page_url().unwrap().as_bytes()
        );
        assert_eq!(p::data(&fields, 39).unwrap(), b"en_US");

        let reply = p::message(&[
            base_response(0),
            p::bytes(
                2,
                b"https://channels.weixin.qq.com/web/pages/feed?oid=a&exportkey=secret",
            ),
        ])
        .unwrap();
        let page = page_response(&reply).unwrap();
        assert!(!format!("{page:?}").contains("secret"));
        let elsewhere =
            p::message(&[base_response(0), p::bytes(2, b"https://example.invalid/")]).unwrap();
        assert!(page_response(&elsewhere).is_err());
    }

    #[test]
    fn detail_request_rides_the_transfer_proxy_with_the_ticket_and_page() {
        let state = state();
        let ticket = ChannelsTicket {
            ticket: Zeroizing::new("ticket".into()),
            lifetime_seconds: None,
        };
        let page = ChannelsPage(Zeroizing::new(
            "https://channels.weixin.qq.com/web/pages/feed?x".into(),
        ));
        let json = detail_json(&post()).unwrap();
        let fields = p::decode(&transfer_request(&state, &ticket, &page, &json).unwrap()).unwrap();
        let numbers: Vec<u32> = fields.iter().map(|f| f.number).collect();
        assert_eq!(numbers, [1, 2, 3, 5, 6, 7, 10]);
        assert_eq!(p::data(&fields, 3).unwrap(), b"ticket");
        assert_eq!(p::data(&fields, 5).unwrap(), page.0.as_bytes());
        assert_eq!(p::scalar(&fields, 6).unwrap(), 400);
        assert_eq!(p::data(&fields, 7).unwrap(), b"finderLive");
        assert_eq!(p::scalar(&fields, 10).unwrap(), 0);
        let inner = p::decode(p::data(&fields, 2).unwrap()).unwrap();
        assert_eq!(p::data(&inner, 1).unwrap(), DETAIL_ROUTE.as_bytes());
        assert_eq!(p::scalar(&inner, 3).unwrap(), 5259);
        let body: serde_json::Value = serde_json::from_slice(p::data(&inner, 2).unwrap()).unwrap();
        assert_eq!(body["objectid"], "18446744073709551000");
        assert_eq!(body["object_nonce_id"], "12345678901234567890");
        assert_eq!(body["scene"], 146);
        assert_eq!(body["pull_scene"], 6);
        assert_eq!(body["finder_basereq"]["scene"], 140);
        assert_eq!(
            body["finder_basereq"]["ctx_info"]["client_report_buff"],
            "{\"entranceId\":\"1002\"}"
        );
        let request_id = body["finder_basereq"]["request_id"].as_str().unwrap();
        assert!(request_id.parse::<u64>().is_ok());
    }

    fn transfer_reply(status: i64, json: &serde_json::Value) -> Vec<u8> {
        p::message(&[
            base_response(0),
            p::bytes(
                2,
                &p::message(&[
                    p::uint(1, status as u64),
                    p::bytes(3, json.to_string().as_bytes()),
                ])
                .unwrap(),
            ),
        ])
        .unwrap()
    }

    fn detail(media: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "BaseResponse": {"Ret": 0},
            "object": {"id": "18446744073709551000", "object_desc": {"media": media}},
        })
    }

    #[test]
    fn detail_reply_must_match_the_post_and_carry_one_video() {
        let entry = serde_json::json!({
            "url": "http://wxapp.tc.qq.com/251/20302/stodownload?encfilekey=secret-file",
            "url_token": "&token=secret-token",
            "decode_key": "0",
            "file_size": 4796270,
            "cdn_file_size": "4796270",
            "md5sum": "not-used",
            "width": 888,
            "height": 576,
            "video_play_len": 59,
        });
        let reply =
            transfer_response(&transfer_reply(0, &detail(serde_json::json!([entry])))).unwrap();
        let media = parse_detail(&reply, &post()).unwrap();
        assert_eq!(media.decode_key(), 0);
        assert_eq!(media.file_size, Some(4_796_270));
        assert_eq!(media.cdn_file_size, Some(4_796_270));
        assert_eq!((media.width, media.height), (Some(888), Some(576)));
        assert_eq!(media.duration_seconds, Some(59));
        let debug = format!("{media:?}");
        for secret in ["secret-file", "secret-token"] {
            assert!(!debug.contains(secret));
        }

        let cases = [
            (
                detail(serde_json::json!([entry.clone(), entry.clone()])),
                "channels_unsupported_media",
            ),
            (detail(serde_json::json!([])), "channels_unsupported_media"),
            (
                detail(serde_json::json!([{"url": "x", "url_token": "y"}])),
                "channels_unsupported_media",
            ),
            (
                serde_json::json!({"BaseResponse": {"Ret": 0},
                    "object": {"id": "1", "object_desc": {"media": [entry.clone()]}}}),
                "channels_unsupported_media",
            ),
            (
                serde_json::json!({"BaseResponse": {"Ret": -4011}}),
                "channels_post_unavailable",
            ),
            (serde_json::json!({}), "channels_post_unavailable"),
        ];
        for (json, code) in cases {
            let reply = transfer_response(&transfer_reply(0, &json)).unwrap();
            assert_eq!(parse_detail(&reply, &post()).unwrap_err().to_string(), code);
        }

        let error = transfer_response(&transfer_reply(-2, &serde_json::json!({}))).unwrap_err();
        let failure = ServiceRejected::find(&error).unwrap();
        assert_eq!((failure.service, failure.code), (Service::Channels, -2));
        assert!(parse_detail(b"not json", &post()).is_err());
    }
}
