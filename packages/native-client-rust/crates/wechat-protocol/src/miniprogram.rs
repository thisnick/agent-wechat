//! Authenticated Mini Program operations for the selected Linux runtime profile.

use crate::{
    calls::{
        app_frame::{lz4_decode, lz4_literal, TaskFrame},
        app_session::{AppAuthority, AppSession, PendingAppAuth},
        host_policy::CallHostRoute,
        signaling::{CommonRequest, CommonResponse},
    },
    crypto, protobuf as p, wire, NativeClient,
};
use anyhow::{ensure, Context, Result};
use std::{collections::BTreeMap, io::Read, time::Duration};
use zeroize::Zeroizing;

const RUNTIME_APP: &[u8] = b"wxd7f003a9a2679061";
const LOGIN_ROUTE: &[u8] = b"/cgi-bin/mmbiz-bin/js-login";
const TRANSFER_ROUTE: &[u8] = b"/ilink/ilinkapp/mp/wxaruntime_transfer";
const TRANSFER: u32 = 2881;
const MAX_REPLY: usize = 8192;
const ATTR_ROUTE: &[u8] = b"/cgi-bin/mmbiz-bin/wxaattr/wxaattrsync";
const MAX_ATTR_REPLY: usize = 1024 * 1024;
const PACKAGE_ROUTE: &[u8] = b"/cgi-bin/mmbiz-bin/wxaapp/batchgetwxadownloadurl";
const PACKAGE_CDN: &str = "launchwxacode.wxqcloud.qq.com.cn";
const MAX_PACKAGE_BYTES: usize = 32 * 1024 * 1024;

/// Server-provided network domains for one Mini Program.
///
/// These are app attributes, not domains declared by downloaded application code.
/// Each API family has its own list. An empty list grants no destinations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiniProgramNetworkPolicy {
    /// Domains checked for `wx.request`.
    pub request_domains: Vec<String>,
    /// Domains checked for `wx.uploadFile`.
    pub upload_domains: Vec<String>,
    /// Domains checked for `wx.downloadFile`.
    pub download_domains: Vec<String>,
    /// Domains checked for WebSocket connections.
    pub websocket_domains: Vec<String>,
}

fn parse_network_policy(app_info: &[u8]) -> Result<MiniProgramNetworkPolicy> {
    ensure!(
        app_info.len() <= 256 * 1024,
        "Mini Program app attributes too large"
    );
    let info: serde_json::Value = serde_json::from_slice(app_info)?;
    let network = info
        .get("Network")
        .and_then(serde_json::Value::as_object)
        .context("Mini Program network policy unavailable")?;
    let domains = |name: &str| -> Result<Vec<String>> {
        let Some(values) = network.get(name) else {
            return Ok(Vec::new());
        };
        let values = values
            .as_array()
            .context("Invalid Mini Program domain list")?;
        ensure!(values.len() <= 1024, "Mini Program domain list too large");
        values
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|value| {
                ensure!(value.len() <= 8192, "Mini Program domain too long");
                Ok(value.to_owned())
            })
            .collect()
    };
    Ok(MiniProgramNetworkPolicy {
        request_domains: domains("RequestDomain")?,
        upload_domains: domains("UploadDomain")?,
        download_domains: domains("DownloadDomain")?,
        websocket_domains: domains("WsRequestDomain")?,
    })
}

impl NativeClient {
    /// Request one Mini Program login code for the selected account and AppID.
    ///
    /// Run on a blocking worker. The guard runs before each submission, including
    /// the app session setup. The returned code is sensitive. An ambiguous
    /// transport error does not authorize resubmission.
    pub fn mini_program_login_guarded(
        &self,
        app_id: &str,
        before_submit: impl Fn() -> Result<()>,
    ) -> Result<Zeroizing<String>> {
        let reply = self.mini_program_exchange_guarded(
            app_id,
            LOGIN_ROUTE,
            1029,
            MAX_REPLY,
            |base| {
                p::message(&[
                    p::bytes(1, base),
                    p::bytes(2, app_id.as_bytes()),
                    p::uint(4, 1),
                    p::bytes(5, b""),
                    p::bytes(6, b""),
                    p::uint(7, 1),
                ])
            },
            before_submit,
        )?;
        decode_login(&reply)
    }

    /// Fetch the app's network domains without issuing an app login code.
    ///
    /// Uses a fresh attribute lookup rather than trusting app-owned package JSON.
    /// Run on a blocking worker. The guard is checked before every submission.
    pub fn mini_program_network_policy_guarded(
        &self,
        app_id: &str,
        before_submit: impl Fn() -> Result<()>,
    ) -> Result<MiniProgramNetworkPolicy> {
        let reply = self.mini_program_exchange_guarded(
            app_id,
            ATTR_ROUTE,
            1151,
            MAX_ATTR_REPLY,
            |base| prepare_attributes(base, app_id),
            before_submit,
        )?;
        decode_attributes(&reply)
    }

    /// Read the app's advertised icon URL without issuing an app login code.
    ///
    /// Run on a blocking worker. The caller must validate and fetch the image
    /// separately without forwarding account credentials to its host.
    pub fn mini_program_icon_guarded(
        &self,
        app_id: &str,
        before_submit: impl Fn() -> Result<()>,
    ) -> Result<Option<String>> {
        let reply = self.mini_program_exchange_guarded(
            app_id,
            ATTR_ROUTE,
            1151,
            MAX_ATTR_REPLY,
            |base| prepare_attributes(base, app_id),
            before_submit,
        )?;
        decode_icon(&reply)
    }

    /// Download the current release's main Mini Program archive from WeChat.
    ///
    /// Run on a blocking worker. Each request checks the caller guard. The
    /// selected package must match the app, version, selector and server checksum.
    pub fn mini_program_download_guarded(
        &self,
        app_id: &str,
        before_submit: impl Fn() -> Result<()>,
    ) -> Result<(u32, Vec<u8>)> {
        let lease = self.account_lease()?;
        let check = || {
            ensure!(lease.current(), "Account authority changed");
            before_submit()
        };
        let attributes = self.mini_program_exchange_guarded(
            app_id,
            ATTR_ROUTE,
            1151,
            MAX_ATTR_REPLY,
            |base| prepare_attributes(base, app_id),
            check,
        )?;
        let version = decode_package_version(&attributes)?;
        let reply = self.mini_program_exchange_guarded(
            app_id,
            PACKAGE_ROUTE,
            1713,
            MAX_REPLY,
            |base| prepare_package(base, app_id, version),
            check,
        )?;
        let (url, digest) = decode_package_reply(&reply, app_id, version)?;
        check()?;
        let bytes = download_package(url, &digest)?;
        check()?;
        Ok((version, bytes))
    }

    fn mini_program_exchange_guarded(
        &self,
        app_id: &str,
        route: &[u8],
        command: u64,
        max_reply: usize,
        prepare: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
        before_submit: impl Fn() -> Result<()>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let id = app_id.as_bytes();
        ensure!(
            id.len() == 18
                && id.starts_with(b"wx")
                && id != RUNTIME_APP
                && id[2..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)),
            "Invalid target Mini Program AppID"
        );
        let lease = self.account_lease()?;
        let generation = self
            .account_status()?
            .credential_generation
            .checked_add(1)
            .context("App authority generation exhausted")?;
        let digest = self.request_blocking(|request| {
            ensure!(lease.current(), "Account authority changed");
            crypto::sha256(&request.account().session_key)
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid credential digest"))
        })?;
        let authority = AppAuthority {
            account_generation: generation,
            logout_epoch: 0,
            credential_digest: digest,
        };
        let task = u32::from_le_bytes(crypto::random::<4>()?) % (u32::MAX - 2) + 1;
        let exchange = |route, task, body: Vec<u8>| {
            self.request_blocking(|request| {
                let state = request.account();
                let bytes = Zeroizing::new(
                    CommonRequest {
                        base_request: Some(wire::base_request(&state.device, state.uin, task)?),
                        body: Some(body),
                        unknown_fields: vec![],
                    }
                    .encode()?,
                );
                request
                    .call_rpc(
                        route,
                        &bytes,
                        || {
                            ensure!(lease.current(), "Account authority changed");
                            before_submit()
                        },
                        |_| Ok(()),
                    )
                    .map(Zeroizing::new)
            })
        };
        let pending = PendingAppAuth::new(authority, crypto::random::<32>()?)?;
        let reply = exchange(CallHostRoute::AppAuth, task, pending.request_body()?)?;
        let session = pending.complete(&reply, authority)?;
        let device = format!("l{}", hex::encode(crypto::random::<16>()?));
        let body = prepare_transfer(&session, id, device.as_bytes(), route, command, prepare)?;
        let reply = exchange(CallHostRoute::Transfer, task + 1, body)?;
        ensure!(lease.current(), "Account authority changed");
        decode_reply(&reply, session.app_uin(), max_reply)
    }
}

fn prepare_transfer(
    session: &AppSession,
    app: &[u8],
    device: &[u8],
    route: &[u8],
    command: u64,
    prepare: impl FnOnce(&[u8]) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    ensure!(
        session.server_id().len() == 27,
        "Unsupported Mini Program session"
    );
    let uin = session.app_uin();
    let base = p::message(&[
        p::bytes(1, b"sessionkey"),
        p::uint(2, uin),
        p::bytes(3, device),
        p::int32(4, -227271398),
        p::bytes(5, b"UnifiedPCLinux"),
        p::uint(6, 0),
    ])?;
    let inner = Zeroizing::new(prepare(&base)?);
    let transfer = p::message(&[
        p::bytes(1, &base),
        p::bytes(2, route),
        p::bytes(3, RUNTIME_APP),
        p::uint(4, 6),
        p::bytes(5, &inner),
        p::bytes(6, app),
        p::uint(7, command),
        p::uint(8, 0xf27064ce),
        p::bytes(9, b"UnifiedPCLinuxWebPlugin"),
        p::uint(10, 0x256064ce),
    ])?;
    let frame = TaskFrame {
        command: TRANSFER,
        integers: BTreeMap::from([
            (1, 1),
            (2, uin),
            (3, 0),
            (4, 0),
            (5, 0xb1400),
            (6, 15),
            (7, 0),
            (8, 0),
            (9, 0),
            (10, 1),
            (11, 0),
            (12, 0),
            (13, 0),
            (17, 0),
            (18, 1),
            (20, 1506),
            (21, 0),
            (22, uin),
            (23, 0),
            (25, 0),
            (26, 4),
            (28, 1),
            (29, 1),
            (30, 0),
            (31, 0),
        ]),
        strings: BTreeMap::from([
            (14, vec![]),
            (24, b"wechat".to_vec()),
            (27, session.server_id().to_vec()),
        ]),
        payload: lz4_literal(&transfer)?,
    }
    .encode()?;
    p::message(&[
        p::uint(1, 1901),
        p::uint(2, u64::from(TRANSFER)),
        p::bytes(3, b"shortcloud.weixin.com"),
        p::bytes(4, TRANSFER_ROUTE),
        p::bytes(5, &frame),
    ])
}

fn decode_reply(reply: &[u8], app_uin: u64, max_reply: usize) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(reply.len() <= max_reply, "Mini Program reply too large");
    let host = CommonResponse::decode(reply)?;
    ensure!(
        host.return_code()? == 0,
        "Mini Program host transfer rejected"
    );
    let delegate = p::decode(
        host.body
            .as_deref()
            .context("Missing Mini Program delegate reply")?,
    )?;
    ensure!(
        p::scalar(&delegate, 1)? == 0,
        "Mini Program delegate rejected"
    );
    let frame = TaskFrame::decode(p::data(&delegate, 2)?, TRANSFER)?;
    for key in [4, 7, 8, 21, 23, 25] {
        ensure!(
            frame.integers.get(&key) == Some(&0),
            "Unsupported or rejected Mini Program frame"
        );
    }
    ensure!(
        frame.integers.get(&22) == Some(&app_uin) && frame.integers.get(&26) == Some(&4),
        "Invalid Mini Program session frame"
    );
    let clear = Zeroizing::new(lz4_decode(&frame.payload, max_reply)?);
    let transfer = p::decode(&clear)?;
    ensure!(
        p::base_code(&transfer)? == 0,
        "Mini Program transfer rejected"
    );
    Ok(Zeroizing::new(p::data(&transfer, 2)?.to_vec()))
}

fn decode_login(reply: &[u8]) -> Result<Zeroizing<String>> {
    let login = p::decode(reply)?;
    ensure!(p::base_code(&login)? == 0, "Mini Program login rejected");
    let code = p::data(&login, 3)?;
    ensure!(
        (1..=1024).contains(&code.len()) && code.iter().all(u8::is_ascii_graphic),
        "Invalid Mini Program login code"
    );
    Ok(Zeroizing::new(std::str::from_utf8(code)?.to_owned()))
}

fn prepare_attributes(base: &[u8], app_id: &str) -> Result<Vec<u8>> {
    p::message(&[
        p::bytes(1, base),
        p::bytes(2, b""),
        p::bytes(3, b"0"),
        p::bytes(4, app_id.as_bytes()),
        p::bytes(7, &p::message(&[p::int32(5, 2)])?),
    ])
}

fn decode_attributes(reply: &[u8]) -> Result<MiniProgramNetworkPolicy> {
    parse_network_policy(&attribute(reply, b"WxaAppInfo")?)
}

fn attribute(reply: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    optional_attribute(reply, key)?.context("Mini Program attribute unavailable")
}

fn decode_icon(reply: &[u8]) -> Result<Option<String>> {
    optional_attribute(reply, b"BrandIconURL")?
        .filter(|value| !value.is_empty())
        .map(|value| {
            let url = String::from_utf8(value)?;
            crate::media::validate_link_thumbnail_url(&url)?;
            Ok(url)
        })
        .transpose()
}

fn optional_attribute(reply: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>> {
    ensure!(
        reply.len() <= MAX_ATTR_REPLY,
        "Mini Program attribute reply too large"
    );
    let fields = p::decode(reply)?;
    ensure!(
        p::base_code(&fields)? == 0,
        "Mini Program attribute lookup rejected"
    );
    let mut value = None;
    for field in fields.iter().filter(|field| field.number == 3) {
        let p::Value::Bytes(bytes) = &field.value else {
            anyhow::bail!("Invalid Mini Program attribute entry");
        };
        let entry = p::decode(bytes)?;
        if p::data(&entry, 1)? == key {
            ensure!(value.is_none(), "Duplicate Mini Program app attributes");
            value = Some(p::data(&entry, 2)?.to_vec());
        }
    }
    Ok(value)
}

fn decode_package_version(reply: &[u8]) -> Result<u32> {
    #[derive(serde::Deserialize)]
    struct VersionInfo {
        #[serde(rename = "AppVersion")]
        version: u32,
    }
    let info: VersionInfo = serde_json::from_slice(&attribute(reply, b"WxaAppVersionInfo")?)?;
    ensure!(info.version != 0, "No published Mini Program package");
    Ok(info.version)
}

fn prepare_package(base: &[u8], app_id: &str, version: u32) -> Result<Vec<u8>> {
    let selector = p::message(&[
        p::bytes(1, app_id.as_bytes()),
        p::bytes(3, b"__APP__"),
        p::uint(4, 4),
        p::uint(5, 0),
    ])?;
    let item = |encoding| {
        let mut fields = vec![
            p::bytes(1, &selector),
            p::uint(4, u64::from(version)),
            p::uint(5, 0),
        ];
        if let Some(encoding) = encoding {
            fields.push(p::uint(6, encoding));
        }
        // Main archive selector used by the supported Linux runtime profile.
        fields.push(p::uint(7, 0xadfe7485));
        p::message(&fields)
    };
    p::message(&[
        p::bytes(1, base),
        p::bytes(2, &item(None)?),
        p::bytes(2, &item(Some(1))?),
    ])
}

fn decode_package_reply(
    reply: &[u8],
    app_id: &str,
    version: u32,
) -> Result<(reqwest::Url, [u8; 16])> {
    let fields = p::decode(reply)?;
    ensure!(p::base_code(&fields)? == 0, "Package lookup rejected");
    let mut package = None;
    for field in fields.iter().filter(|field| field.number == 2) {
        let p::Value::Bytes(bytes) = &field.value else {
            anyhow::bail!("Invalid package result");
        };
        let result = p::decode(bytes)?;
        if p::scalar(&result, 1)? != 0 || p::scalar(&result, 7)? != 0 {
            continue;
        }
        let selector = p::decode(p::data(&result, 20)?)?;
        ensure!(
            p::data(&selector, 1)? == app_id.as_bytes()
                && p::data(&selector, 3)? == b"__APP__"
                && p::scalar(&selector, 4)? == 4
                && p::scalar(&selector, 5)? == 0
                && p::scalar(&result, 5)? == u64::from(version)
                && p::scalar(&result, 6)? == 0,
            "Package result does not match the request"
        );
        let url = reqwest::Url::parse(std::str::from_utf8(p::data(&result, 2)?)?)?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some(PACKAGE_CDN)
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "Unsupported Mini Program package origin"
        );
        let mut digest = [0; 16];
        hex::decode_to_slice(p::data(&result, 3)?, &mut digest)?;
        ensure!(package.is_none(), "Duplicate Mini Program package result");
        package = Some((url, digest));
    }
    package.context("No supported Mini Program main archive")
}

fn download_package(url: reqwest::Url, digest: &[u8; 16]) -> Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .build()?;
    let response = client
        .get(url)
        .send()
        .map_err(reqwest::Error::without_url)?;
    ensure!(
        response.status() == 200
            && response
                .content_length()
                .is_none_or(|len| len <= MAX_PACKAGE_BYTES as u64),
        "Mini Program package HTTP response rejected"
    );
    let mut bytes = Vec::new();
    response
        .take((MAX_PACKAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        (18..=MAX_PACKAGE_BYTES).contains(&bytes.len())
            && bytes[0] == 0xbe
            && bytes[13] == 0xed
            && &openssl::hash::hash(openssl::hash::MessageDigest::md5(), &bytes)?[..] == digest,
        "Mini Program archive failed verification"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_reply_selects_only_the_requested_raw_release_archive() {
        let app = "wx0123456789abcdef";
        let selector = p::message(&[
            p::bytes(1, app.as_bytes()),
            p::bytes(3, b"__APP__"),
            p::uint(4, 4),
            p::uint(5, 0),
        ])
        .unwrap();
        let item = |url: &[u8], encoding| {
            p::message(&[
                p::uint(1, 0),
                p::bytes(2, url),
                p::bytes(3, b"00112233445566778899aabbccddeeff"),
                p::uint(4, 10_800),
                p::uint(5, 7),
                p::uint(6, 0),
                p::uint(7, encoding),
                p::bytes(20, &selector),
            ])
            .unwrap()
        };
        let reply = |url: &[u8]| {
            p::message(&[
                p::bytes(1, &p::int32(1, 0)),
                p::bytes(2, &item(url, 0)),
                p::bytes(2, &item(url, 1)),
            ])
            .unwrap()
        };
        let good = reply(b"https://launchwxacode.wxqcloud.qq.com.cn/package");
        let (url, digest) = decode_package_reply(&good, app, 7).unwrap();
        assert_eq!(url.path(), "/package");
        assert_eq!(digest[15], 255);
        assert!(decode_package_reply(&good, app, 8).is_err());
        assert!(decode_package_reply(&good, "wx1111111111111111", 7).is_err());
        let wrong_origin = reply(b"https://launchwxacode.wxqcloud.qq.com.cn.example.org/package");
        assert!(decode_package_reply(&wrong_origin, app, 7).is_err());
    }

    #[test]
    fn cold_attribute_request_uses_app_id_and_empty_username() {
        let fields =
            p::decode(&prepare_attributes(b"base", "wx0123456789abcdef").unwrap()).unwrap();
        assert_eq!(p::data(&fields, 2).unwrap(), b"");
        assert_eq!(p::data(&fields, 3).unwrap(), b"0");
        assert_eq!(p::data(&fields, 4).unwrap(), b"wx0123456789abcdef");
        let source = p::decode(p::data(&fields, 7).unwrap()).unwrap();
        assert_eq!(p::scalar(&source, 5).unwrap(), 2);
        // Unknown optional context fields are not fabricated.
        assert!(p::optional(&fields, 9).unwrap().is_none());
    }

    #[test]
    fn only_server_app_info_supplies_separate_api_domain_lists() {
        let info = br#"{"Network":{"RequestDomain":["https://api.example.com",4],"DownloadDomain":["https://files.example.com"]}}"#;
        let entry = p::message(&[p::bytes(1, b"WxaAppInfo"), p::bytes(2, info)]).unwrap();
        let success = p::bytes(1, &p::int32(1, 0));
        let reply = p::message(&[success.clone(), p::bytes(3, &entry)]).unwrap();
        let policy = decode_attributes(&reply).unwrap();
        assert_eq!(policy.request_domains, ["https://api.example.com"]);
        assert_eq!(policy.download_domains, ["https://files.example.com"]);
        assert!(policy.upload_domains.is_empty());
        assert!(decode_attributes(
            &p::message(&[success, p::bytes(3, &entry), p::bytes(3, &entry)]).unwrap()
        )
        .is_err());
        assert!(parse_network_policy(br#"{"RequestDomain":["https://example.com"]}"#).is_err());
        assert!(parse_network_policy(br#"{"Network":{"RequestDomain":"*"}} "#).is_err());
    }

    #[test]
    fn attribute_reply_limit_is_supported_by_the_shared_task_decoder() {
        let bytes = br#"{"Network":{"RequestDomain":["https://example.com"]}}"#;
        let compressed = lz4_literal(bytes).unwrap();
        assert_eq!(lz4_decode(&compressed, MAX_ATTR_REPLY).unwrap(), bytes);
    }

    #[test]
    fn login_rejections_and_malformed_codes_remain_errors() {
        let success = p::bytes(1, &p::int32(1, 0));
        let valid = p::message(&[success.clone(), p::bytes(3, b"synthetic-code")]).unwrap();
        assert_eq!(&**decode_login(&valid).unwrap(), "synthetic-code");
        for code in [b"".as_slice(), b"bad code", b"code\n"] {
            assert!(
                decode_login(&p::message(&[success.clone(), p::bytes(3, code)]).unwrap()).is_err()
            );
        }
        assert!(decode_login(
            &p::message(&[p::bytes(1, &p::int32(1, -1)), p::bytes(3, b"code")]).unwrap()
        )
        .is_err());
    }
    #[test]
    fn icon_attributes_are_optional_exact_and_validated() {
        let reply = |key: &[u8], value: &[u8]| {
            p::message(&[
                p::bytes(1, &p::message(&[p::int32(1, 0)]).unwrap()),
                p::bytes(
                    3,
                    &p::message(&[p::bytes(1, key), p::bytes(2, value)]).unwrap(),
                ),
            ])
            .unwrap()
        };
        let url = "https://example.invalid/app-icon.png";
        assert_eq!(
            decode_icon(&reply(b"BrandIconURL", url.as_bytes()))
                .unwrap()
                .as_deref(),
            Some(url)
        );
        assert_eq!(
            decode_icon(&reply(b"BrandIconUrl", url.as_bytes())).unwrap(),
            None
        );
        assert_eq!(decode_icon(&reply(b"BrandIconURL", b"")).unwrap(), None);
        for unsafe_url in [
            b"http://127.0.0.1/icon".as_slice(),
            b"https://name:secret@example.invalid/icon",
            b"file:///icon",
            &[255],
        ] {
            assert!(decode_icon(&reply(b"BrandIconURL", unsafe_url)).is_err());
        }
        let mut duplicate = reply(b"BrandIconURL", url.as_bytes());
        duplicate.extend(
            p::message(&[p::bytes(
                3,
                &p::message(&[p::bytes(1, b"BrandIconURL"), p::bytes(2, url.as_bytes())]).unwrap(),
            )])
            .unwrap(),
        );
        assert!(decode_icon(&duplicate).is_err());
        assert!(decode_icon(
            &p::message(&[p::bytes(1, &p::message(&[p::int32(1, -1)]).unwrap())]).unwrap()
        )
        .is_err());
    }
}
