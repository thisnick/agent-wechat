//! Bounded native CDN transport. This uses the observed HTTP carrier on port 443,
//! not HTTPS. Media encryption and advertised size/digests are checked separately.
use crate::errors::{Service, ServiceRejected};
use crate::{
    crypto::{md5, random, sha256},
    protobuf as p, AccountState,
};
use anyhow::{bail, ensure, Result};
use openssl::{
    hash::{Hasher, MessageDigest},
    symm::{Cipher, Crypter, Mode},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, io::Read, net::Ipv4Addr, time::Duration};
use zeroize::Zeroize;
pub type CdnFields = BTreeMap<String, Vec<u8>>;
const MAX_CDN_BODY: usize = 2 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 65536;
pub const MAX_CDN_TRANSFER_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_NATIVE_UPLOAD_BYTES: usize = 25 * 1024 * 1024 - 17;
pub const MAX_LARGE_FILE_BYTES: usize = wechat_media::MAX_FILE_BYTES;
/// Incoming video plaintext limit, matching the ordinary-file download and
/// automatic-download ceiling. Every such transfer stays bound to the
/// advertised length and MD5. Outgoing preparation keeps the separate
/// `wechat_media::MAX_VIDEO_SOURCE_BYTES` source limit.
pub const MAX_VIDEO_DOWNLOAD_BYTES: usize = wechat_media::MAX_FILE_BYTES;
/// A video advertised at or below the ordinary transfer threshold is fetched
/// before its length is compared, so its reply keeps the earlier 50 MiB bound.
const MAX_SMALL_VIDEO_REPLY_BYTES: usize = 50 * 1024 * 1024;
#[path = "large_cdn_config.rs"]
mod large_cdn_config;
#[path = "large_file.rs"]
mod large_file;
pub use large_cdn_config::{decode_large_cdn_config, LargeCdnRoute};
#[path = "avatar.rs"]
mod avatar;
pub use avatar::*;
#[path = "link_thumbnail.rs"]
mod link_thumbnail;
pub use link_thumbnail::*;
#[path = "sticker.rs"]
mod sticker;
pub use sticker::*;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct CdnLocation {
    pub province: u32,
    pub city: u32,
    pub isp: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CdnConfig {
    pub version: u32,
    pub uin: u64,
    pub client_version: u32,
    pub client_os_type: String,
    pub auth_key: Vec<u8>,
    pub server_ips: Vec<Ipv4Addr>,
    #[serde(default)]
    pub front_id: u32,
    #[serde(default)]
    pub location: Option<CdnLocation>,
}
impl Drop for CdnConfig {
    fn drop(&mut self) {
        self.auth_key.zeroize();
    }
}
impl fmt::Debug for CdnConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CdnConfig")
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl CdnConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.uin != 0
                && !self.auth_key.is_empty()
                && self.auth_key.len() <= 65536
                && !self.client_os_type.is_empty()
                && self.client_os_type.len() <= 1024
                && !self.server_ips.is_empty()
                && self.server_ips.len() <= 32,
            "Invalid CDN config"
        );
        for ip in &self.server_ips {
            ensure!(public_ipv4(*ip), "CDN endpoint is not public IPv4");
        }
        Ok(())
    }
}
pub fn decode_cdn_config(input: &[u8], account: &AccountState) -> Result<CdnConfig> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    ServiceRejected::check(Service::CdnBootstrap, code)?;
    let group = p::decode(p::data(&f, 2)?)?;
    let version = u32::try_from(p::scalar(&group, 1)?)?;
    let uin = p::scalar(&group, 2)?;
    ensure!(uin == account.uin as u64, "CDN account identity mismatch");
    let auth_key = p::unbuffer(p::data(&group, 8)?)?;
    let mut server_ips = vec![];
    for field in group.iter().filter(|f| f.number == 6) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("Invalid CDN endpoint record");
        };
        let ip = p::decode(raw)?;
        server_ips.push(std::str::from_utf8(p::data(&ip, 1)?)?.parse()?);
    }
    let conf = CdnConfig {
        version,
        uin,
        client_version: account.device.client_version as u32,
        client_os_type: account.device.device_type.clone(),
        auth_key,
        server_ips,
        front_id: 0,
        location: None,
    };
    conf.validate()?;
    Ok(conf)
}
fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && (b == 0 || b == 168 || (b == 2 && c == 0)))
        || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113))
}
pub fn pack_cdn(fields: &CdnFields) -> Result<Vec<u8>> {
    // A 512 MiB completion can contain 1025 ordered block etags. The byte
    // envelope remains independently bounded, including individual values.
    ensure!(fields.len() <= 2048, "Too many CDN fields");
    let mut out = vec![];
    for (k, v) in fields {
        ensure!(
            !k.is_empty()
                && k.len() <= 128
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid CDN key"
        );
        for b in [k.as_bytes(), v.as_slice()] {
            ensure!(b.len() <= MAX_CDN_BODY, "CDN field too large");
            out.extend((b.len() as u32).to_be_bytes());
            out.extend(b);
            ensure!(out.len() <= MAX_CDN_BODY, "CDN request exceeds limit");
        }
    }
    Ok(out)
}
pub fn unpack_cdn(input: &[u8]) -> Result<CdnFields> {
    ensure!(input.len() <= MAX_CDN_BODY, "CDN reply exceeds limit");
    let mut c = crate::mmtls::Cursor::new(input);
    let mut out = CdnFields::new();
    let mut consumed = 0;
    while consumed < input.len() {
        ensure!(out.len() < 256, "Too many CDN reply fields");
        let key = c.sized(4)?;
        let value = c.sized(4)?;
        consumed += 8 + key.len() + value.len();
        ensure!(
            !key.is_empty()
                && key.len() <= 128
                && key
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-'),
            "Invalid CDN reply key"
        );
        let key = std::str::from_utf8(key)?.to_string();
        ensure!(
            out.insert(key, value.to_vec()).is_none(),
            "Duplicate CDN reply key"
        );
    }
    c.end()?;
    Ok(out)
}
fn integer(fields: &CdnFields, key: &str) -> Result<i64> {
    let raw = fields
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("Missing CDN integer {key}"))?;
    let text = std::str::from_utf8(raw)?;
    ensure!(
        !text.is_empty()
            && text.len() <= 16
            && text
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-')),
        "Invalid CDN integer {key}"
    );
    Ok(text.parse()?)
}
fn put(fields: &mut CdnFields, key: &str, value: impl ToString) {
    fields.insert(key.to_string(), value.to_string().into_bytes());
}
fn shared(conf: &CdnConfig, sequence: u32, filetype: u32) -> CdnFields {
    let mut f = CdnFields::new();
    for (k, v) in [
        ("ver", conf.version as u64),
        ("weixinnum", conf.uin),
        ("seq", sequence as u64),
        ("clientversion", conf.client_version as u64),
        ("nettype", 1),
        ("acceptdupack", 1),
        ("safeproto", 1),
        ("filetype", filetype as u64),
        ("wxchattype", 0),
        ("lastretcode", 0),
        ("ipseq", 0),
        ("largesvideo", 0),
        ("sourceflag", 0),
    ] {
        put(&mut f, k, v);
    }
    f.insert(
        "clientostype".into(),
        conf.client_os_type.as_bytes().to_vec(),
    );
    f.insert("authkey".into(), conf.auth_key.clone());
    f
}
pub fn encrypt_recipient(destination: &str) -> Result<String> {
    ensure!(
        !destination.is_empty()
            && destination.len() <= 512
            && !destination
                .chars()
                .any(|c| c.is_control() || c.is_whitespace()),
        "Invalid CDN recipient"
    );
    let key = b"wxusrname2016cdn";
    let encrypted = openssl::symm::encrypt(
        Cipher::aes_128_cbc(),
        key,
        Some(key),
        destination.as_bytes(),
    )?;
    Ok(format!("@cdn2_{}", hex::encode(encrypted)))
}
fn ecb_encrypt(raw: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    ensure!(key.len() == 16, "Invalid CDN key");
    Ok(openssl::symm::encrypt(
        Cipher::aes_128_ecb(),
        key,
        None,
        raw,
    )?)
}
pub fn decrypt_cdn(encrypted: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        key.len() == 16
            && !encrypted.is_empty()
            && encrypted.len().is_multiple_of(16)
            && encrypted.len() <= MAX_LARGE_FILE_BYTES + 16,
        "Invalid CDN ciphertext"
    );
    Ok(openssl::symm::decrypt(
        Cipher::aes_128_ecb(),
        key,
        None,
        encrypted,
    )?)
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaQuality {
    Original,
    Standard,
    Thumbnail,
    Video,
    File,
    LargeFile,
}
impl MediaQuality {
    pub fn filetype(self) -> u32 {
        match self {
            Self::Original => 1,
            Self::Standard => 2,
            Self::Thumbnail => 3,
            Self::Video => 4,
            Self::File => 5,
            Self::LargeFile => 7,
        }
    }
}
fn upload_plaintext_limit(kind: MediaQuality) -> usize {
    match kind {
        MediaQuality::File => MAX_CDN_TRANSFER_BYTES,
        MediaQuality::LargeFile => MAX_LARGE_FILE_BYTES,
        MediaQuality::Video => wechat_media::MAX_VIDEO_SOURCE_BYTES,
        _ => MAX_NATIVE_UPLOAD_BYTES,
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MediaDescriptor {
    pub file_id: String,
    pub aes_key: Vec<u8>,
    pub quality: MediaQuality,
    pub expected_bytes: Option<u64>,
    pub expected_md5: Option<String>,
    pub strict_size: bool,
    pub expires_at: Option<u64>,
}
impl Drop for MediaDescriptor {
    fn drop(&mut self) {
        self.aes_key.zeroize();
        self.file_id.zeroize();
    }
}
impl fmt::Debug for MediaDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaDescriptor")
            .field("quality", &self.quality)
            .field("metadata", &"[redacted]")
            .finish()
    }
}
pub struct DownloadedMedia {
    pub bytes: Vec<u8>,
    pub requested_quality: MediaQuality,
    pub substituted_filetype: Option<u32>,
    pub picture_format: Option<u32>,
    pub md5: String,
    pub sha256: String,
    pub advertised_size_matches: Option<bool>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedUpload {
    pub kind: MediaQuality,
    pub aes_key: Vec<u8>,
    pub file_key: String,
    pub encrypted: Vec<u8>,
    pub plaintext_bytes: u32,
    pub md5: String,
    pub sha256: String,
    pub file_name: String,
    pub destination: String,
}
impl Drop for PreparedUpload {
    fn drop(&mut self) {
        self.aes_key.zeroize();
        self.file_key.zeroize();
        self.encrypted.zeroize();
    }
}
impl fmt::Debug for PreparedUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedUpload")
            .field("kind", &self.kind)
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("material", &"[redacted]")
            .finish()
    }
}
impl PreparedUpload {
    /// Generate independent upload material. Persist either this result or the
    /// source bytes and its key/file-key before the first upload request.
    pub fn prepare(
        raw: &[u8],
        kind: MediaQuality,
        file_name: &str,
        destination: &str,
    ) -> Result<Self> {
        Self::with_material(
            raw,
            kind,
            file_name,
            destination,
            &random::<16>()?,
            &hex::encode(random::<16>()?),
        )
    }
    /// Rebuild exactly the same encrypted upload from durable owned source bytes.
    pub fn with_material(
        raw: &[u8],
        kind: MediaQuality,
        file_name: &str,
        destination: &str,
        key: &[u8],
        file_key: &str,
    ) -> Result<Self> {
        ensure!(
            !raw.is_empty()
                && raw.len() <= upload_plaintext_limit(kind)
                && [
                    MediaQuality::File,
                    MediaQuality::LargeFile,
                    MediaQuality::Original,
                    MediaQuality::Video
                ]
                .contains(&kind),
            "Unsupported upload profile"
        );
        ensure!(
            !file_name.is_empty()
                && file_name.len() <= 255
                && file_name != "."
                && file_name != ".."
                && !file_name.contains(['/', '\\'])
                && !file_name.chars().any(|c| c.is_control()),
            "Invalid upload filename"
        );
        ensure!(
            key.len() == 16
                && file_key.len() == 32
                && file_key.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid durable upload material"
        );
        encrypt_recipient(destination)?;
        Ok(Self {
            kind,
            aes_key: key.to_vec(),
            file_key: file_key.to_string(),
            encrypted: ecb_encrypt(raw, key)?,
            plaintext_bytes: raw.len() as u32,
            md5: hex::encode(md5(raw)),
            sha256: hex::encode(sha256(raw)),
            file_name: file_name.into(),
            destination: destination.into(),
        })
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.aes_key.len() == 16
                && self.file_key.len() == 32
                && self.file_key.bytes().all(|b| b.is_ascii_hexdigit())
                && self.plaintext_bytes > 0
                && self.plaintext_bytes as usize <= upload_plaintext_limit(self.kind)
                && self.encrypted.len() == ((self.plaintext_bytes as usize / 16) + 1) * 16,
            "Invalid prepared upload"
        );
        // Validate persisted ciphertext without allocating a second whole file.
        let mut decryptor =
            Crypter::new(Cipher::aes_128_ecb(), Mode::Decrypt, &self.aes_key, None)?;
        let mut buffer = zeroize::Zeroizing::new(vec![0; 128 * 1024 + 16]);
        let mut md5 = Hasher::new(MessageDigest::md5())?;
        let mut sha256 = Hasher::new(MessageDigest::sha256())?;
        let mut plaintext_bytes = 0usize;
        for chunk in self.encrypted.chunks(128 * 1024) {
            let written = decryptor.update(chunk, &mut buffer)?;
            md5.update(&buffer[..written])?;
            sha256.update(&buffer[..written])?;
            plaintext_bytes += written;
        }
        let written = decryptor.finalize(&mut buffer)?;
        md5.update(&buffer[..written])?;
        sha256.update(&buffer[..written])?;
        plaintext_bytes += written;
        ensure!(
            plaintext_bytes == self.plaintext_bytes as usize
                && hex::encode(md5.finish()?) == self.md5
                && hex::encode(sha256.finish()?) == self.sha256,
            "Prepared upload digest mismatch"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct UploadedMedia {
    pub descriptor: MediaDescriptor,
    pub plaintext_bytes: u32,
    pub md5: String,
    pub sha256: String,
    pub file_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
pub struct CdnClient {
    config: CdnConfig,
    timeout: Duration,
}
fn large_file_path(path: &str, uin: u64, sequence: u32) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 6
        && parts[0].is_empty()
        && parts[1] == "cdn"
        && parts[2] == uin.to_string()
        && sequence > 0
        && parts[3] == sequence.to_string()
        && matches!(parts[4], "4" | "5" | "7")
        && matches!(parts[5], "apply" | "uploadblock" | "fin" | "complete")
}
impl CdnClient {
    pub fn new(config: CdnConfig, timeout: Duration) -> Result<Self> {
        config.validate()?;
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(120),
            "Invalid CDN timeout"
        );
        Ok(Self { config, timeout })
    }
    fn post(&self, path: &str, fields: &CdnFields) -> Result<CdnFields> {
        let sequence = u32::try_from(integer(fields, "seq")?)?;
        ensure!(sequence != 0, "Invalid CDN sequence");
        ensure!(
            ["/download", "/downloadbig", "/uploadv3"].contains(&path)
                || large_file_path(path, self.config.uin, sequence),
            "Invalid CDN path"
        );
        let body = pack_cdn(fields)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        let mut response = client
            .post(format!("http://{}:443{path}", self.config.server_ips[0]))
            .header("Accept", "*/*")
            .header("Content-Type", "application/octet-stream")
            .header("User-Agent", "MicroMessenger Client")
            .header("Connection", "close")
            .header("C-Seq", sequence)
            .body(body)
            .send()?;
        ensure!(
            response.status() == 200,
            "CDN HTTP status {}",
            response.status().as_u16()
        );
        let mut raw = vec![];
        response
            .by_ref()
            .take((MAX_CDN_BODY + 1) as u64)
            .read_to_end(&mut raw)?;
        unpack_cdn(&raw)
    }
    pub fn download(&self, descriptor: &MediaDescriptor) -> Result<DownloadedMedia> {
        self.download_with_progress(descriptor, |_, _| Ok(()))
    }
    pub fn download_with_progress(
        &self,
        descriptor: &MediaDescriptor,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<DownloadedMedia> {
        self.download_with_transport(descriptor, progress, |path, fields| self.post(path, fields))
    }
    fn download_with_transport(
        &self,
        descriptor: &MediaDescriptor,
        mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    ) -> Result<DownloadedMedia> {
        ensure!(
            !descriptor.file_id.is_empty()
                && descriptor.file_id.len() <= 8192
                && !descriptor.file_id.chars().any(|c| c.is_control())
                && descriptor.aes_key.len() == 16,
            "Invalid download descriptor"
        );
        if let Some(expiry) = descriptor.expires_at {
            ensure!(expiry > crate::wire::unix_time()? as u64, "media_expired");
        }
        let large = descriptor.quality == MediaQuality::LargeFile;
        let large_video = descriptor.quality == MediaQuality::Video
            && descriptor.expected_bytes.unwrap_or(0) > MAX_CDN_TRANSFER_BYTES as u64;
        let strict_transfer = large || large_video;
        let plaintext_limit = if large {
            MAX_LARGE_FILE_BYTES
        } else if large_video {
            MAX_VIDEO_DOWNLOAD_BYTES
        } else if descriptor.quality == MediaQuality::Video && descriptor.expected_bytes.is_some() {
            MAX_SMALL_VIDEO_REPLY_BYTES
        } else {
            MAX_CDN_TRANSFER_BYTES
        };
        let chunk_bytes = if strict_transfer { 512 * 1024 } else { 65536 };
        let ciphertext_limit = (plaintext_limit / 16 + 1) * 16;
        let expected_ciphertext = if strict_transfer {
            let expected = usize::try_from(
                descriptor
                    .expected_bytes
                    .ok_or_else(|| anyhow::anyhow!("Large file size is required"))?,
            )?;
            ensure!(
                expected > 0 && expected <= plaintext_limit,
                "Invalid large file size"
            );
            Some((expected / 16 + 1) * 16)
        } else {
            ensure!(
                descriptor
                    .expected_bytes
                    .is_none_or(|size| size <= plaintext_limit as u64),
                "Advertised media size outside limit"
            );
            None
        };
        if large_video {
            ensure!(
                descriptor.expected_md5.as_ref().is_some_and(
                    |md5| md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit())
                ),
                "Large video source checksum is required"
            );
        }
        let mut position = 0usize;
        let mut total = 0usize;
        let mut encrypted = vec![];
        let mut substituted_filetype = None;
        let mut picture_format = None;
        let mut server_md5 = None;
        let observe_video = descriptor.quality == MediaQuality::Video && !strict_transfer;
        let mut video_reply = VideoReplyMetadata::default();
        for sequence in 1..=u32::try_from(ciphertext_limit.div_ceil(chunk_bytes))? {
            progress(
                position as u64,
                if total == 0 { None } else { Some(total as u64) },
            )?;
            let mut fields = shared(&self.config, sequence, descriptor.quality.filetype());
            let requested_end = if total == 0 {
                chunk_bytes - 1
            } else {
                (total - 1).min(position + chunk_bytes - 1)
            };
            for (k, v) in [
                ("wxautostart", 0),
                ("downpicformat", 1),
                ("rangestart", position as u64),
                ("rangeend", requested_end as u64),
            ] {
                put(&mut fields, k, v);
            }
            fields.insert("wxmsgflag".into(), vec![]);
            fields.insert("fileid".into(), descriptor.file_id.as_bytes().to_vec());
            let reply = post(if large { "/downloadbig" } else { "/download" }, &fields)?;
            let code = integer(&reply, "retcode")?;
            ServiceRejected::check_wide(Service::CdnDownload, code)?;
            if strict_transfer || reply.contains_key("seq") {
                ensure!(
                    integer(&reply, "seq")? == i64::from(sequence),
                    "CDN download reply sequence mismatch"
                );
            }
            if large {
                validate_large_download_reply(&reply, descriptor, &mut server_md5)?;
            } else if large_video {
                validate_large_video_download_reply(&reply, descriptor, &mut server_md5)?;
            }
            let start = usize::try_from(integer(&reply, "rangestart")?)?;
            let end = usize::try_from(integer(&reply, "rangeend")?)?;
            let size = usize::try_from(integer(&reply, "totalsize")?)?;
            let data = reply
                .get("filedata")
                .ok_or_else(|| anyhow::anyhow!("Missing CDN payload"))?;
            ensure!(
                start == position
                    && !data.is_empty()
                    && size > 0
                    && size <= ciphertext_limit
                    && expected_ciphertext.is_none_or(|expected| expected == size)
                    && size >= position
                    && data.len() <= size - position
                    && end <= requested_end
                    && (!strict_transfer || data.len() == (size - position).min(chunk_bytes))
                    && (total == 0 || total == size)
                    && end == start + data.len() - 1,
                "Inconsistent CDN range response"
            );
            total = size;
            if encrypted.is_empty() {
                encrypted.try_reserve_exact(total)?;
            }
            position += data.len();
            encrypted.extend(data);
            if !large {
                for (key, target) in [
                    ("substituteftype", &mut substituted_filetype),
                    ("rsppicformat", &mut picture_format),
                ] {
                    if reply.contains_key(key) {
                        let value = u32::try_from(integer(&reply, key)?)?;
                        ensure!(
                            target.is_none_or(|previous| previous == value),
                            "CDN representation changed during transfer"
                        );
                        *target = Some(value);
                    }
                }
            }
            if observe_video {
                video_reply.observe(&reply);
            }
            progress(position as u64, Some(total as u64))?;
            if position == total {
                break;
            }
        }
        ensure!(
            total > 0 && position == total,
            "CDN transfer did not finish within bound"
        );
        let bytes = decrypt_cdn(&encrypted, &descriptor.aes_key)?;
        ensure!(bytes.len() <= plaintext_limit, "Decoded CDN body too large");
        let size_matches = descriptor.expected_bytes.map(|n| n == bytes.len() as u64);
        let size_rejected =
            (strict_transfer || descriptor.strict_size) && size_matches == Some(false);
        let digest = hex::encode(md5(&bytes));
        let digest_matches = descriptor.expected_md5.as_ref().map(|expected| {
            expected.len() == 32
                && expected.bytes().all(|b| b.is_ascii_hexdigit())
                && digest.eq_ignore_ascii_case(expected)
        });
        if observe_video && (size_rejected || digest_matches == Some(false)) {
            // Reject exactly as before, but keep comparison facts so a different
            // server-selected representation can be told apart from corruption.
            return Err(video_reply
                .mismatch(
                    descriptor,
                    bytes.len(),
                    &digest,
                    digest_matches,
                    substituted_filetype,
                )
                .into());
        }
        ensure!(!size_rejected, "Advertised media size mismatch");
        ensure!(
            digest_matches != Some(false),
            "Advertised media checksum mismatch"
        );
        if let Some(expected) = server_md5 {
            ensure!(
                digest.eq_ignore_ascii_case(&expected),
                "CDN source checksum mismatch"
            );
        }
        let sha = hex::encode(sha256(&bytes));
        Ok(DownloadedMedia {
            bytes,
            requested_quality: descriptor.quality,
            substituted_filetype,
            picture_format,
            md5: digest,
            sha256: sha,
            advertised_size_matches: size_matches,
        })
    }
    pub fn upload(&self, prepared: &PreparedUpload) -> Result<UploadedMedia> {
        self.upload_with_progress(prepared, |_, _| Ok(()))
    }
    pub fn upload_with_progress(
        &self,
        prepared: &PreparedUpload,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        self.upload_with_transport(prepared, progress, |path, fields| self.post(path, fields))
    }
    fn upload_with_transport(
        &self,
        prepared: &PreparedUpload,
        mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    ) -> Result<UploadedMedia> {
        prepared.validate()?;
        let path = legacy_upload_path(
            prepared.kind,
            prepared.plaintext_bytes as usize,
            prepared.encrypted.len(),
        )?;
        let mut file_id = None;
        for (sequence, chunk) in prepared.encrypted.chunks(UPLOAD_CHUNK).enumerate() {
            let start = sequence * UPLOAD_CHUNK;
            progress(start as u64, Some(prepared.encrypted.len() as u64))?;
            let end = start + chunk.len() - 1;
            let mut fields =
                upload_fields(&self.config, prepared, sequence as u32 + 1, start, end)?;
            fields.insert("filedata".into(), chunk.to_vec());
            let reply = post(path, &fields)?;
            validate_upload_reply(&reply, sequence as u32 + 1, start, end, &mut file_id)?;
            // The final correlated acknowledgement must reach the caller so it
            // can save completion before applying cancellation/logout fences.
            if end + 1 < prepared.encrypted.len() {
                progress((end + 1) as u64, Some(prepared.encrypted.len() as u64))?;
            }
        }
        let file_id =
            file_id.ok_or_else(|| anyhow::anyhow!("CDN upload returned no file identifier"))?;
        Ok(UploadedMedia {
            descriptor: MediaDescriptor {
                file_id,
                aes_key: prepared.aes_key.clone(),
                quality: prepared.kind,
                expected_bytes: Some(prepared.plaintext_bytes as u64),
                expected_md5: Some(prepared.md5.clone()),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: prepared.plaintext_bytes,
            md5: prepared.md5.clone(),
            sha256: prepared.sha256.clone(),
            file_name: prepared.file_name.clone(),
            width: None,
            height: None,
        })
    }
}
/// Only these fixed, nonsecret numeric controls can enter a stored diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeFileDownloadControl {
    Version,
    VideoFormat,
}
impl LargeFileDownloadControl {
    pub const fn field_name(self) -> &'static str {
        match self {
            Self::Version => "ver",
            Self::VideoFormat => "videoformat",
        }
    }
}
/// Strict rejection details. Unknown keys and response bytes are never retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeFileDownloadError {
    UnknownField,
    FixedControlMismatch {
        control: LargeFileDownloadControl,
        actual: i64,
    },
}
impl fmt::Display for LargeFileDownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownField => "Unsupported large-file download response field",
            Self::FixedControlMismatch { .. } => "Unsupported large-file download control",
        })
    }
}
impl std::error::Error for LargeFileDownloadError {}

/// How an optional reply value relates to the selected message tuple and to
/// the bytes actually delivered. Values are compared, never retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceEvidence {
    pub matches_advertised: bool,
    pub matches_delivered: bool,
}

/// A complete video transfer at or below the ordinary transfer threshold
/// decrypted successfully but did not match the selected message tuple. The
/// download is still rejected. Only fixed numeric controls and comparison
/// results are kept, so a caller can store a diagnostic without reply bytes or
/// digests. Fields use wire key names; their meanings are not established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoRepresentationMismatch {
    pub advertised_bytes: Option<u64>,
    pub delivered_bytes: u64,
    pub advertised_digest_matches: Option<bool>,
    pub substituted_filetype: Option<u32>,
    pub istrans: Option<i64>,
    pub videoformat: Option<i64>,
    pub rawfilesize: Option<SourceEvidence>,
    pub rawfilemd5: Option<SourceEvidence>,
}
impl fmt::Display for VideoRepresentationMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Advertised video representation mismatch")
    }
}
impl std::error::Error for VideoRepresentationMismatch {}

/// First value of each optional video reply field on the ordinary path. This is
/// diagnostic only: an absent or malformed value never rejects a reply.
#[derive(Default)]
struct VideoReplyMetadata {
    istrans: Option<i64>,
    videoformat: Option<i64>,
    rawfilesize: Option<Option<u64>>,
    rawfilemd5: Option<Option<String>>,
}
impl VideoReplyMetadata {
    fn observe(&mut self, reply: &CdnFields) {
        for (key, target) in [
            ("istrans", &mut self.istrans),
            ("videoformat", &mut self.videoformat),
        ] {
            if target.is_none() {
                *target = integer(reply, key).ok();
            }
        }
        if self.rawfilesize.is_none() && reply.contains_key("rawfilesize") {
            self.rawfilesize = Some(
                integer(reply, "rawfilesize")
                    .ok()
                    .and_then(|value| u64::try_from(value).ok()),
            );
        }
        if self.rawfilemd5.is_none() {
            if let Some(raw) = reply.get("rawfilemd5") {
                self.rawfilemd5 = Some(
                    (raw.len() == 32 && raw.iter().all(u8::is_ascii_hexdigit))
                        .then(|| String::from_utf8_lossy(raw).to_ascii_lowercase()),
                );
            }
        }
    }
    fn mismatch(
        &self,
        descriptor: &MediaDescriptor,
        delivered_bytes: usize,
        delivered_md5: &str,
        advertised_digest_matches: Option<bool>,
        substituted_filetype: Option<u32>,
    ) -> VideoRepresentationMismatch {
        let delivered_bytes = delivered_bytes as u64;
        VideoRepresentationMismatch {
            advertised_bytes: descriptor.expected_bytes,
            delivered_bytes,
            advertised_digest_matches,
            substituted_filetype,
            istrans: self.istrans,
            videoformat: self.videoformat,
            rawfilesize: self.rawfilesize.map(|value| SourceEvidence {
                matches_advertised: value.is_some() && value == descriptor.expected_bytes,
                matches_delivered: value == Some(delivered_bytes),
            }),
            rawfilemd5: self.rawfilemd5.as_ref().map(|value| SourceEvidence {
                matches_advertised: value
                    .as_ref()
                    .zip(descriptor.expected_md5.as_ref())
                    .is_some_and(|(value, expected)| value.eq_ignore_ascii_case(expected)),
                matches_delivered: value.as_deref() == Some(delivered_md5),
            }),
        }
    }
}

// Type 4 retains the normal download endpoint. Unlike file7, representation
// and routing metadata cannot be ignored for a byte-exact original video.
fn validate_large_video_download_reply(
    reply: &CdnFields,
    descriptor: &MediaDescriptor,
    server_md5: &mut Option<String>,
) -> Result<()> {
    const FIELDS: &[&str] = &[
        "ver",
        "seq",
        "retcode",
        "filedata",
        "rangestart",
        "rangeend",
        "totalsize",
        "retrysec",
        "isretry",
        "isoverload",
        "isgetcdn",
        "istrans",
        "backsrcnum",
        "rawfilesize",
        "rawfilemd5",
        "substituteftype",
        "rsppicformat",
        "videoformat",
        "x-ClientIp",
    ];
    ensure!(
        reply.keys().all(|key| FIELDS.contains(&key.as_str())),
        "Unsupported large-video download response field"
    );
    ensure!(
        integer(reply, "ver")? == 0,
        "Unsupported large-video download version"
    );
    for key in [
        "retrysec",
        "isretry",
        "isoverload",
        "isgetcdn",
        "backsrcnum",
        "rsppicformat",
    ] {
        if reply.contains_key(key) {
            ensure!(
                integer(reply, key)? == 0,
                "Unsupported large-video download control"
            );
        }
    }
    // Linux 4.1.13.26's normal pipeline response map discards `istrans`.
    // Its success handler's format lookup is literal `KEY_VIDEOFORMAT`
    // (CRC fb520bf9), not the retained wire key `videoformat` (dbe097e1).
    // Neither affects the selected ECB/range writer. Admit only the values
    // observed with byte-identical original-video ciphertext; retain the
    // independent representation, source size and digest checks below.
    for (key, observed) in [("istrans", 999), ("videoformat", 1)] {
        if reply.contains_key(key) {
            let value = integer(reply, key)?;
            ensure!(
                value == 0 || value == observed,
                "Unsupported large-video download control"
            );
        }
    }
    if reply.contains_key("substituteftype") {
        ensure!(
            matches!(integer(reply, "substituteftype")?, 0 | 4),
            "Unsupported large-video download representation"
        );
    }
    if reply.contains_key("rawfilesize") {
        ensure!(
            u64::try_from(integer(reply, "rawfilesize")?)?
                == descriptor.expected_bytes.unwrap_or(0),
            "Large-video download source size mismatch"
        );
    }
    if let Some(raw) = reply.get("rawfilemd5") {
        ensure!(
            raw.len() == 32 && raw.iter().all(u8::is_ascii_hexdigit),
            "Invalid CDN source checksum"
        );
        let value = std::str::from_utf8(raw)?;
        ensure!(
            server_md5
                .as_ref()
                .is_none_or(|previous| previous.eq_ignore_ascii_case(value))
                && descriptor
                    .expected_md5
                    .as_ref()
                    .is_some_and(|expected| expected.eq_ignore_ascii_case(value)),
            "CDN source checksum changed"
        );
        *server_md5 = Some(value.to_string());
    }
    Ok(())
}

fn validate_large_download_reply(
    reply: &CdnFields,
    descriptor: &MediaDescriptor,
    server_md5: &mut Option<String>,
) -> Result<()> {
    const FIELDS: &[&str] = &[
        "ver",
        "seq",
        "retcode",
        "filedata",
        "rangestart",
        "rangeend",
        "totalsize",
        "retrysec",
        "isretry",
        "isoverload",
        "isgetcdn",
        "istrans",
        "backsrcnum",
        "rawfilesize",
        "rawfilemd5",
        "substituteftype",
        "rsppicformat",
        "videoformat",
        "x-ClientIp",
    ];
    if !reply.keys().all(|key| FIELDS.contains(&key.as_str())) {
        return Err(LargeFileDownloadError::UnknownField.into());
    }
    use LargeFileDownloadControl as Control;
    for (control, expected) in [(Control::Version, 0), (Control::VideoFormat, 0)] {
        let actual = integer(reply, control.field_name())?;
        if actual != expected {
            return Err(LargeFileDownloadError::FixedControlMismatch { control, actual }.into());
        }
    }
    // Linux 4.1.13.26's file7 pipeline (0x8d12d60 -> 0x8ce3000 ->
    // 0x8ca0ec0) does not consume these on a valid retcode-zero response.
    // The first four are optional template values; the remaining four never
    // enter its response template. They do not trigger retries or substitution.
    for key in [
        "retrysec",
        "isretry",
        "isoverload",
        "isgetcdn",
        "istrans",
        "backsrcnum",
        "substituteftype",
        "rsppicformat",
    ] {
        if let Some(raw) = reply.get(key) {
            // Keep the existing 16-byte integer parser bound, with unsigned
            // syntax. This is a native parsing policy, not a service limit.
            ensure!(
                raw.iter().all(u8::is_ascii_digit),
                "Invalid large-file metadata integer"
            );
            integer(reply, key)?;
        }
    }
    // File replies can omit this redundant source size. The transfer still
    // checks the encrypted length and the final decrypted length against the
    // message descriptor. Reject conflicting metadata when it is supplied.
    if reply.contains_key("rawfilesize") {
        ensure!(
            u64::try_from(integer(reply, "rawfilesize")?)?
                == descriptor.expected_bytes.unwrap_or(0),
            "Large-file download source size mismatch"
        );
    }
    let raw = reply
        .get("rawfilemd5")
        .ok_or_else(|| anyhow::anyhow!("Missing CDN source checksum"))?;
    ensure!(
        raw.len() == 32 && raw.iter().all(u8::is_ascii_hexdigit),
        "Invalid CDN source checksum"
    );
    let value = std::str::from_utf8(raw)?;
    ensure!(
        server_md5
            .as_ref()
            .is_none_or(|previous| previous.eq_ignore_ascii_case(value))
            && descriptor
                .expected_md5
                .as_ref()
                .is_none_or(|expected| expected.eq_ignore_ascii_case(value)),
        "CDN source checksum changed"
    );
    *server_md5 = Some(value.to_string());
    Ok(())
}
fn legacy_upload_path(
    kind: MediaQuality,
    plaintext: usize,
    ciphertext: usize,
) -> Result<&'static str> {
    ensure!(
        kind != MediaQuality::LargeFile
            && (kind != MediaQuality::File || plaintext <= MAX_NATIVE_UPLOAD_BYTES)
            && ciphertext < MAX_CDN_TRANSFER_BYTES,
        "File requires multiphase upload"
    );
    Ok("/uploadv3")
}
fn escaped_component(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn upload_fields(
    conf: &CdnConfig,
    p: &PreparedUpload,
    sequence: u32,
    start: usize,
    end: usize,
) -> Result<CdnFields> {
    ensure!(
        sequence > 0 && start <= end && end < p.encrypted.len() && end - start < UPLOAD_CHUNK,
        "Invalid upload range"
    );
    let mut f = shared(conf, sequence, p.kind.filetype());
    for (k, v) in [
        ("hasthumb", 0),
        ("compresstype", 0),
        ("nocheckaeskey", 1),
        ("enablehit", 0),
        ("existancecheck", 0),
        ("apptype", 0),
        ("totalsize", p.encrypted.len() as u64),
        ("rawtotalsize", p.plaintext_bytes as u64),
        ("thumbtotalsize", 0),
        ("rawthumbsize", 0),
        ("encthumbcrc", 0),
        ("advideoflag", 0),
        ("rangestart", start as u64),
        ("rangeend", end as u64),
    ] {
        put(&mut f, k, v);
    }
    for (k, v) in [
        ("touser", encrypt_recipient(&p.destination)?),
        ("filekey", p.file_key.clone()),
        ("localname", escaped_component(&p.file_name)),
        ("filedatamd5", hex::encode(md5(&p.encrypted[start..=end]))),
        ("filemd5", p.md5.clone()),
        ("rawfilemd5", p.md5.clone()),
        ("blockmd5", hex::encode(md5(&p.encrypted[start..=end]))),
    ] {
        f.insert(k.into(), v.into_bytes());
    }
    f.insert("rawthumbmd5".into(), vec![]);
    Ok(f)
}
fn validate_upload_reply(
    reply: &CdnFields,
    sequence: u32,
    start: usize,
    end: usize,
    file_id: &mut Option<String>,
) -> Result<()> {
    let code = integer(reply, "retcode")?;
    ServiceRejected::check_wide(Service::CdnUpload, code)?;
    for (key, expected) in [
        ("seq", sequence as u64),
        ("rangestart", start as u64),
        ("rangeend", end as u64),
    ] {
        if reply.contains_key(key) {
            ensure!(
                u64::try_from(integer(reply, key)?)? == expected,
                "CDN upload correlation mismatch"
            );
        }
    }
    if let Some(raw) = reply.get("fileid") {
        let value = std::str::from_utf8(raw)?;
        ensure!(
            !value.is_empty()
                && value.len() <= 4096
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_@.+/=".contains(&b)),
            "Invalid uploaded file identifier"
        );
        ensure!(
            file_id.as_deref().is_none_or(|previous| previous == value),
            "CDN upload file identifier changed"
        );
        *file_id = Some(value.to_string());
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedVariant {
    pub encrypted: Vec<u8>,
    pub plaintext_bytes: u32,
    pub width: u32,
    pub height: u32,
    pub md5: String,
    pub sha256: String,
    pub checksum: u32,
    pub encrypted_checksum: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedImageUpload {
    pub original: PreparedUpload,
    pub standard: PreparedVariant,
    pub thumbnail: PreparedVariant,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedVideoUpload {
    pub video: PreparedUpload,
    pub thumbnail: PreparedVariant,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct UploadedImage {
    pub original: UploadedMedia,
    pub standard: UploadedMedia,
    pub thumbnail: UploadedMedia,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct UploadedVideo {
    pub video: UploadedMedia,
    pub thumbnail: UploadedMedia,
    #[serde(default)]
    pub original: Option<UploadedMedia>,
}
/// Dimensions of an image original after decoding it once. An original is not
/// held to the 65,520-byte limit of the standard and thumbnail copies, and may
/// be an unchanged camera JPEG larger than their 4,096-pixel edge limit.
fn original_jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_NATIVE_UPLOAD_BYTES,
        "JPEG original outside upload limit"
    );
    let inspection = inspect_image(bytes)?;
    ensure!(
        inspection.content_type == "image/jpeg",
        "Expected JPEG original"
    );
    Ok((inspection.width, inspection.height))
}
/// Dimensions from the JPEG frame header, without decoding its pixels.
fn jpeg_header_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    Ok(
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg)
            .into_dimensions()?,
    )
}
fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    use image::GenericImageView;
    ensure!(
        !bytes.is_empty() && bytes.len() <= 65520,
        "JPEG variant outside small-object profile"
    );
    ensure!(
        image::guess_format(bytes)? == image::ImageFormat::Jpeg,
        "Expected JPEG variant"
    );
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    let (w, h) = image.dimensions();
    ensure!(
        w > 0 && h > 0 && w <= 4096 && h <= 4096,
        "JPEG dimensions outside limit"
    );
    Ok((w, h))
}
pub fn adler32_zero(bytes: &[u8]) -> u32 {
    let mut a = 0u32;
    let mut b = 0u32;
    for &v in bytes {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
impl PreparedVariant {
    fn new(raw: &[u8], key: &[u8]) -> Result<Self> {
        let (width, height) = jpeg_dimensions(raw)?;
        let encrypted = ecb_encrypt(raw, key)?;
        Ok(Self {
            checksum: adler32_zero(raw),
            encrypted_checksum: adler32_zero(&encrypted),
            encrypted,
            plaintext_bytes: raw.len() as u32,
            width,
            height,
            md5: hex::encode(md5(raw)),
            sha256: hex::encode(sha256(raw)),
        })
    }
    fn validate(&self, key: &[u8]) -> Result<()> {
        let raw = decrypt_cdn(&self.encrypted, key)?;
        let (w, h) = jpeg_dimensions(&raw)?;
        ensure!(
            raw.len() == self.plaintext_bytes as usize
                && w == self.width
                && h == self.height
                && hex::encode(md5(&raw)) == self.md5
                && hex::encode(sha256(&raw)) == self.sha256
                && adler32_zero(&raw) == self.checksum
                && adler32_zero(&self.encrypted) == self.encrypted_checksum,
            "Prepared JPEG variant mismatch"
        );
        Ok(())
    }
}
impl PreparedImageUpload {
    pub fn with_material(
        original: &[u8],
        standard: &[u8],
        thumbnail: &[u8],
        file_name: &str,
        destination: &str,
        key: &[u8],
        file_key: &str,
    ) -> Result<Self> {
        let (width, height) = original_jpeg_dimensions(original)?;
        let original = PreparedUpload::with_material(
            original,
            MediaQuality::Original,
            file_name,
            destination,
            key,
            file_key,
        )?;
        let standard = PreparedVariant::new(standard, key)?;
        let thumbnail = PreparedVariant::new(thumbnail, key)?;
        let p = Self {
            original,
            standard,
            thumbnail,
            width,
            height,
        };
        p.validate()?;
        Ok(p)
    }
    /// Supplied variants must be independent real JPEGs. This function decodes
    /// them fully but does not resize or invent an unsupplied representation.
    pub fn prepare(
        original: &[u8],
        standard: &[u8],
        thumbnail: &[u8],
        file_name: &str,
        destination: &str,
    ) -> Result<Self> {
        let (width, height) = original_jpeg_dimensions(original)?;
        let original =
            PreparedUpload::prepare(original, MediaQuality::Original, file_name, destination)?;
        let standard = PreparedVariant::new(standard, &original.aes_key)?;
        let thumbnail = PreparedVariant::new(thumbnail, &original.aes_key)?;
        let p = Self {
            original,
            standard,
            thumbnail,
            width,
            height,
        };
        p.validate()?;
        Ok(p)
    }
    fn validate(&self) -> Result<()> {
        self.original.validate()?;
        let raw = decrypt_cdn(&self.original.encrypted, &self.original.aes_key)?;
        ensure!(
            jpeg_header_dimensions(&raw)? == (self.width, self.height),
            "Prepared original dimensions differ"
        );
        self.standard.validate(&self.original.aes_key)?;
        self.thumbnail.validate(&self.original.aes_key)?;
        ensure!(
            self.original.plaintext_bytes <= MAX_NATIVE_UPLOAD_BYTES as u32
                // A preview of a rotated camera original is drawn upright, so its
                // edges may be the original's stored edges exchanged.
                && ((self.standard.width <= self.width && self.standard.height <= self.height)
                    || (self.standard.width <= self.height
                        && self.standard.height <= self.width))
                && self.thumbnail.width <= self.standard.width
                && self.thumbnail.height <= self.standard.height
                && self.thumbnail.width <= 320
                && self.thumbnail.height <= 320,
            "Invalid JPEG variant sizes"
        );
        Ok(())
    }
}
impl PreparedVideoUpload {
    pub fn with_material(
        video: &[u8],
        thumbnail: &[u8],
        file_name: &str,
        destination: &str,
        key: &[u8],
        file_key: &str,
    ) -> Result<Self> {
        ensure!(
            video.len() >= 12 && video.len() <= MAX_NATIVE_UPLOAD_BYTES && video[4..8] == *b"ftyp",
            "Expected bounded MP4 container"
        );
        let video = PreparedUpload::with_material(
            video,
            MediaQuality::Video,
            file_name,
            destination,
            key,
            file_key,
        )?;
        let thumbnail = PreparedVariant::new(thumbnail, key)?;
        ensure!(
            thumbnail.width <= 320 && thumbnail.height <= 320,
            "Video thumbnail outside limit"
        );
        Ok(Self { video, thumbnail })
    }
    pub fn prepare(
        video: &[u8],
        thumbnail: &[u8],
        file_name: &str,
        destination: &str,
    ) -> Result<Self> {
        ensure!(
            video.len() >= 12 && video.len() <= MAX_NATIVE_UPLOAD_BYTES && video[4..8] == *b"ftyp",
            "Expected bounded MP4 container"
        );
        let video = PreparedUpload::prepare(video, MediaQuality::Video, file_name, destination)?;
        let thumbnail = PreparedVariant::new(thumbnail, &video.aes_key)?;
        ensure!(
            thumbnail.width <= 320 && thumbnail.height <= 320,
            "Video thumbnail outside limit"
        );
        Ok(Self { video, thumbnail })
    }
}
fn add_thumbnail(fields: &mut CdnFields, v: &PreparedVariant) {
    for (k, n) in [
        ("hasthumb", 1),
        ("thumbfiletype", 3),
        ("thumbtotalsize", v.encrypted.len() as u64),
        ("rawthumbsize", v.plaintext_bytes as u64),
        ("thumbcrc", v.checksum as u64),
        ("encthumbcrc", v.encrypted_checksum as u64),
    ] {
        put(fields, k, n);
    }
    fields.insert("rawthumbmd5".into(), v.md5.as_bytes().to_vec());
    fields.insert("thumbdata".into(), v.encrypted.clone());
}
fn uploaded_primary(
    prepared: &PreparedUpload,
    file_id: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> UploadedMedia {
    UploadedMedia {
        descriptor: MediaDescriptor {
            file_id: file_id.to_string(),
            aes_key: prepared.aes_key.clone(),
            quality: prepared.kind,
            expected_bytes: Some(prepared.plaintext_bytes as u64),
            expected_md5: Some(prepared.md5.clone()),
            strict_size: true,
            expires_at: None,
        },
        plaintext_bytes: prepared.plaintext_bytes,
        md5: prepared.md5.clone(),
        sha256: prepared.sha256.clone(),
        file_name: prepared.file_name.clone(),
        width,
        height,
    }
}
fn uploaded_variant(
    primary: &PreparedUpload,
    v: &PreparedVariant,
    file_id: &str,
    quality: MediaQuality,
) -> UploadedMedia {
    UploadedMedia {
        descriptor: MediaDescriptor {
            file_id: file_id.to_string(),
            aes_key: primary.aes_key.clone(),
            quality,
            expected_bytes: Some(v.plaintext_bytes as u64),
            expected_md5: Some(v.md5.clone()),
            strict_size: true,
            expires_at: None,
        },
        plaintext_bytes: v.plaintext_bytes,
        md5: v.md5.clone(),
        sha256: v.sha256.clone(),
        file_name: primary.file_name.clone(),
        width: Some(v.width),
        height: Some(v.height),
    }
}
impl CdnClient {
    pub fn upload_image(&self, p: &PreparedImageUpload) -> Result<UploadedImage> {
        self.upload_image_with_progress(p, |_, _| Ok(()))
    }
    /// Upload an image's original, standard and thumbnail copies, reporting the encrypted bytes
    /// of the original sent so far and their total before each request.
    pub fn upload_image_with_progress(
        &self,
        p: &PreparedImageUpload,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedImage> {
        self.upload_image_with_transport(p, progress, |path, fields| self.post(path, fields))
    }
    /// Upload the original in 64 KiB ranges like a video. The first request
    /// also carries the standard and thumbnail copies and the image metadata,
    /// so an original that fits one range is still a single request.
    fn upload_image_with_transport(
        &self,
        p: &PreparedImageUpload,
        mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    ) -> Result<UploadedImage> {
        p.validate()?;
        let total = p.original.encrypted.len();
        let mut file_id = None;
        for (sequence, chunk) in p.original.encrypted.chunks(UPLOAD_CHUNK).enumerate() {
            let start = sequence * UPLOAD_CHUNK;
            let end = start + chunk.len() - 1;
            progress(start as u64, Some(total as u64))?;
            let mut f = upload_fields(&self.config, &p.original, sequence as u32 + 1, start, end)?;
            f.insert("filedata".into(), chunk.to_vec());
            if start == 0 {
                add_thumbnail(&mut f, &p.thumbnail);
                for (k, v) in [
                    ("imgheight", p.height as u64),
                    ("imgwidth", p.width as u64),
                    ("midimgrawsize", p.standard.plaintext_bytes as u64),
                    ("midimgtotalsize", p.standard.encrypted.len() as u64),
                    ("midimgchecksum", p.standard.encrypted_checksum as u64),
                    ("file_format", 0),
                    ("mid_format", 0),
                ] {
                    put(&mut f, k, v);
                }
                f.insert("midimgmd5".into(), p.standard.md5.as_bytes().to_vec());
                f.insert("midimgdata".into(), p.standard.encrypted.clone());
            }
            let reply = post("/uploadv3", &f)?;
            validate_upload_reply(&reply, sequence as u32 + 1, start, end, &mut file_id)?;
            if end + 1 < total {
                progress((end + 1) as u64, Some(total as u64))?;
            }
        }
        let id =
            file_id.ok_or_else(|| anyhow::anyhow!("Image upload returned no file identifier"))?;
        Ok(UploadedImage {
            original: uploaded_primary(&p.original, &id, Some(p.width), Some(p.height)),
            standard: uploaded_variant(&p.original, &p.standard, &id, MediaQuality::Standard),
            thumbnail: uploaded_variant(&p.original, &p.thumbnail, &id, MediaQuality::Thumbnail),
        })
    }
    pub fn upload_video(&self, p: &PreparedVideoUpload) -> Result<UploadedVideo> {
        self.upload_video_with_progress(p, |_, _| Ok(()))
    }
    pub fn upload_video_with_progress(
        &self,
        p: &PreparedVideoUpload,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedVideo> {
        self.upload_video_with_transport(p, progress, |path, fields| self.post(path, fields))
    }
    fn upload_video_with_transport(
        &self,
        p: &PreparedVideoUpload,
        mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    ) -> Result<UploadedVideo> {
        p.video.validate()?;
        ensure!(
            p.video.kind == MediaQuality::Video
                && p.video.plaintext_bytes as usize <= MAX_NATIVE_UPLOAD_BYTES,
            "Invalid normal video upload profile"
        );
        p.thumbnail.validate(&p.video.aes_key)?;
        ensure!(
            p.thumbnail.width <= 320 && p.thumbnail.height <= 320,
            "Video thumbnail outside limit"
        );
        let mut file_id = None;
        for (sequence, chunk) in p.video.encrypted.chunks(UPLOAD_CHUNK).enumerate() {
            let start = sequence * UPLOAD_CHUNK;
            let end = start + chunk.len() - 1;
            progress(start as u64, Some(p.video.encrypted.len() as u64))?;
            let mut fields =
                upload_fields(&self.config, &p.video, sequence as u32 + 1, start, end)?;
            fields.insert("filedata".into(), chunk.to_vec());
            if start == 0 {
                add_thumbnail(&mut fields, &p.thumbnail);
            }
            put(&mut fields, "smallvideoflag", 0);
            let reply = post("/uploadv3", &fields)?;
            validate_upload_reply(&reply, sequence as u32 + 1, start, end, &mut file_id)?;
            if end + 1 < p.video.encrypted.len() {
                progress((end + 1) as u64, Some(p.video.encrypted.len() as u64))?;
            }
        }
        let id =
            file_id.ok_or_else(|| anyhow::anyhow!("Video upload returned no file identifier"))?;
        Ok(UploadedVideo {
            video: uploaded_primary(&p.video, &id, None, None),
            thumbnail: uploaded_variant(&p.video, &p.thumbnail, &id, MediaQuality::Thumbnail),
            original: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client() -> CdnClient {
        CdnClient::new(
            CdnConfig {
                version: 1,
                uin: 12345,
                client_version: 456,
                client_os_type: "synthetic-client".into(),
                auth_key: b"synthetic-auth".to_vec(),
                server_ips: vec![Ipv4Addr::new(1, 1, 1, 1)],
                front_id: 0,
                location: None,
            },
            Duration::from_secs(25),
        )
        .unwrap()
    }
    fn download_fixture(size: usize, quality: MediaQuality) -> (MediaDescriptor, Vec<u8>, Vec<u8>) {
        let raw = vec![0x5a; size];
        let key = vec![0x31; 16];
        let ciphertext = ecb_encrypt(&raw, &key).unwrap();
        let descriptor = MediaDescriptor {
            file_id: "synthetic-object".into(),
            aes_key: key,
            quality,
            expected_bytes: Some(size as u64),
            expected_md5: Some(hex::encode(md5(&raw))),
            strict_size: true,
            expires_at: None,
        };
        (descriptor, raw, ciphertext)
    }
    fn download_reply(
        fields: &CdnFields,
        descriptor: &MediaDescriptor,
        ciphertext: &[u8],
    ) -> CdnFields {
        let mut reply = CdnFields::new();
        let start = integer(fields, "rangestart").unwrap() as usize;
        let end = (integer(fields, "rangeend").unwrap() as usize).min(ciphertext.len() - 1);
        assert_eq!(fields["fileid"], descriptor.file_id.as_bytes());
        assert_eq!(integer(fields, "weixinnum").unwrap(), 12345);
        assert!(!fields.contains_key("aeskey"));
        for (key, value) in [
            ("ver", 0),
            ("seq", integer(fields, "seq").unwrap()),
            ("retcode", 0),
            ("rangestart", start as i64),
            ("rangeend", end as i64),
            ("totalsize", ciphertext.len() as i64),
        ] {
            put(&mut reply, key, value);
        }
        reply.insert("filedata".into(), ciphertext[start..=end].to_vec());
        if descriptor.quality == MediaQuality::LargeFile {
            for (key, value) in [
                ("retrysec", 0),
                ("isretry", 0),
                ("isoverload", 0),
                ("isgetcdn", 0),
                ("istrans", 999),
                ("backsrcnum", 0),
                ("substituteftype", 7),
                ("rsppicformat", 0),
                ("videoformat", 0),
                ("rawfilesize", descriptor.expected_bytes.unwrap() as i64),
            ] {
                put(&mut reply, key, value);
            }
            reply.insert(
                "rawfilemd5".into(),
                descriptor
                    .expected_md5
                    .as_ref()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            );
        }
        reply
    }
    #[test]
    fn large_video_keeps_normal_endpoint_type_and_exact_source_bytes() {
        let (descriptor, raw, ciphertext) =
            download_fixture(MAX_CDN_TRANSFER_BYTES + 17, MediaQuality::Video);
        let mut requests = 0;
        let downloaded = client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |path, fields| {
                    requests += 1;
                    assert_eq!(path, "/download");
                    assert_eq!(integer(fields, "filetype")?, 4);
                    assert_eq!(integer(fields, "seq")?, requests);
                    assert_eq!(integer(fields, "rangestart")?, (requests - 1) * 512 * 1024);
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    put(&mut reply, "substituteftype", 4);
                    put(&mut reply, "videoformat", 1);
                    put(&mut reply, "istrans", 999);
                    Ok(reply)
                },
            )
            .unwrap();
        assert_eq!(requests, 51);
        assert_eq!(downloaded.bytes, raw);
        assert_eq!(downloaded.md5, descriptor.expected_md5.as_deref().unwrap());
        assert_eq!(downloaded.sha256, hex::encode(sha256(&raw)));
        assert_eq!(downloaded.substituted_filetype, Some(4));
    }

    #[test]
    fn large_video_rejects_unknown_controls_substitution_and_unbound_source() {
        let (mut descriptor, _, ciphertext) = download_fixture(16, MediaQuality::Video);
        descriptor.expected_bytes = Some(MAX_CDN_TRANSFER_BYTES as u64 + 1);
        let fields = BTreeMap::from([
            ("rangestart".into(), b"0".to_vec()),
            ("rangeend".into(), b"31".to_vec()),
            ("seq".into(), b"1".to_vec()),
            ("weixinnum".into(), b"12345".to_vec()),
            ("fileid".into(), descriptor.file_id.as_bytes().to_vec()),
        ]);
        let valid = download_reply(&fields, &descriptor, &ciphertext);
        for (key, value) in [
            ("unknown", "private"),
            ("ver", "1"),
            ("videoformat", "2"),
            ("istrans", "1"),
            ("KEY_VIDEOFORMAT", "1"),
            ("isretry", "1"),
            ("isgetcdn", "1"),
            ("rsppicformat", "3"),
            ("substituteftype", "7"),
            ("rawfilesize", "1"),
            ("rawfilemd5", "00000000000000000000000000000000"),
        ] {
            let mut reply = valid.clone();
            reply.insert(key.into(), value.as_bytes().to_vec());
            let error = validate_large_video_download_reply(&reply, &descriptor, &mut None)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private"));
        }
        for (size, checksum) in [
            (
                Some(MAX_VIDEO_DOWNLOAD_BYTES as u64 + 1),
                descriptor.expected_md5.clone(),
            ),
            (descriptor.expected_bytes, None),
        ] {
            descriptor.expected_bytes = size;
            descriptor.expected_md5 = checksum;
            let mut requests = 0;
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, _| {
                        requests += 1;
                        bail!("must not request")
                    }
                )
                .is_err());
            assert_eq!(requests, 0);
        }
    }

    #[test]
    fn cold_large_descriptor_downloads_with_padding_and_hashes() {
        let (descriptor, raw, ciphertext) =
            download_fixture(32 * 1024 * 1024, MediaQuality::LargeFile);
        assert_eq!(ciphertext.len(), 32 * 1024 * 1024 + 16);
        let stored = serde_json::to_vec(&descriptor).unwrap();
        let cold: MediaDescriptor = serde_json::from_slice(&stored).unwrap();
        let events = std::cell::RefCell::new(Vec::new());
        let mut requests = 0;
        let mut previous = 0;
        let downloaded = client()
            .download_with_transport(
                &cold,
                |done, total| {
                    events.borrow_mut().push("progress");
                    assert!(done >= previous);
                    previous = done;
                    if let Some(total) = total {
                        assert_eq!(total, ciphertext.len() as u64);
                    }
                    Ok(())
                },
                |path, fields| {
                    events.borrow_mut().push("request");
                    requests += 1;
                    assert_eq!(path, "/downloadbig");
                    assert_eq!(integer(fields, "filetype")?, 7);
                    assert_eq!(integer(fields, "seq")?, requests);
                    assert_eq!(integer(fields, "rangestart")?, (requests - 1) * 512 * 1024);
                    Ok(download_reply(fields, &cold, &ciphertext))
                },
            )
            .unwrap();
        assert_eq!(requests, 65);
        assert_eq!(previous, ciphertext.len() as u64);
        for chunk in events.borrow().chunks(3) {
            assert_eq!(chunk, ["progress", "request", "progress"]);
        }
        assert_eq!(downloaded.bytes, raw);
        assert_eq!(
            downloaded.md5,
            descriptor.expected_md5.as_ref().unwrap().as_str()
        );
        assert_eq!(downloaded.sha256, hex::encode(sha256(&raw)));
        assert_eq!(downloaded.requested_quality, MediaQuality::LargeFile);
        assert_eq!(downloaded.substituted_filetype, None);
        assert_eq!(downloaded.picture_format, None);
        assert_eq!(downloaded.advertised_size_matches, Some(true));
    }
    #[test]
    fn large_download_accepts_missing_size_but_checks_supplied_size() {
        let (descriptor, raw, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        let downloaded = client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    reply.remove("rawfilesize");
                    Ok(reply)
                },
            )
            .unwrap();
        assert_eq!(downloaded.bytes, raw);
        for value in ["", "invalid", "-1", "1"] {
            let result = client().download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    reply.insert("rawfilesize".into(), value.as_bytes().to_vec());
                    Ok(reply)
                },
            );
            assert!(result.is_err(), "Accepted invalid source size {value}");
        }
    }
    #[test]
    fn large_download_rejections_expose_only_typed_static_control_details() {
        use LargeFileDownloadControl as Control;
        let (descriptor, _, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        for (control, actual) in [(Control::Version, 1), (Control::VideoFormat, 1)] {
            let mut calls = 0;
            let error = client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| {
                        calls += 1;
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
                        put(&mut reply, control.field_name(), actual);
                        Ok(reply)
                    },
                )
                .err()
                .unwrap();
            assert_eq!(calls, 1);
            assert_eq!(
                error.downcast_ref::<LargeFileDownloadError>(),
                Some(&LargeFileDownloadError::FixedControlMismatch { control, actual })
            );
            assert_eq!(error.to_string(), "Unsupported large-file download control");
        }
        let secret = "private-response-token-and-file-id";
        let error = client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    reply.insert(secret.into(), secret.as_bytes().to_vec());
                    Ok(reply)
                },
            )
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<LargeFileDownloadError>(),
            Some(&LargeFileDownloadError::UnknownField)
        );
        assert_eq!(
            error.to_string(),
            "Unsupported large-file download response field"
        );
        assert!(!format!("{error:?}").contains(secret));
    }
    #[test]
    fn large_download_rejects_correlation_format_unknown_fields_and_ranges() {
        let (descriptor, _, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        for (key, value) in [
            ("seq", 2),
            ("ver", 1),
            ("retcode", -1),
            ("videoformat", 1),
            ("rangestart", 1),
            ("rangeend", 512 * 1024),
            ("totalsize", ciphertext.len() as i64 + 16),
            ("rawfilesize", 1),
            ("futureauth", 1),
        ] {
            let mut requests = 0;
            let outcome = client().download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    requests += 1;
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    put(&mut reply, key, value);
                    Ok(reply)
                },
            );
            assert!(outcome.is_err(), "Accepted invalid {key}");
            assert_eq!(requests, 1);
        }
        for missing in ["seq", "ver", "videoformat", "rawfilemd5", "filedata"] {
            assert!(
                client()
                    .download_with_transport(
                        &descriptor,
                        |_, _| Ok(()),
                        |_, fields| {
                            let mut reply = download_reply(fields, &descriptor, &ciphertext);
                            reply.remove(missing);
                            Ok(reply)
                        }
                    )
                    .is_err(),
                "Accepted missing {missing}"
            );
        }
    }
    #[test]
    fn file7_success_metadata_is_optional_and_does_not_change_verified_bytes() {
        // These eight names come from the reference template/consumer join,
        // independently of the native validator's allowlist.
        let metadata = [
            "isretry",
            "retrysec",
            "isoverload",
            "isgetcdn",
            "istrans",
            "backsrcnum",
            "substituteftype",
            "rsppicformat",
        ];
        let (descriptor, raw, ciphertext) = download_fixture(1024 * 1024, MediaQuality::LargeFile);
        assert_eq!(ciphertext.len(), 1024 * 1024 + 16);
        // Each field may be absent or vary independently on the first, middle,
        // and final padding-only ranges, without creating another request.
        for selected in metadata.into_iter().map(Some).chain([None]) {
            for omit in [false, true] {
                let mut requests = 0;
                let result = client()
                    .download_with_transport(
                        &descriptor,
                        |_, _| Ok(()),
                        |path, fields| {
                            requests += 1;
                            assert_eq!(path, "/downloadbig");
                            assert_eq!(integer(fields, "seq")?, requests);
                            let mut reply = download_reply(fields, &descriptor, &ciphertext);
                            for key in metadata {
                                if selected.is_none_or(|selected| selected == key) {
                                    if omit {
                                        reply.remove(key);
                                    } else {
                                        put(
                                            &mut reply,
                                            key,
                                            match requests {
                                                1 => 1,
                                                2 => 999,
                                                3 => 9_999_999_999_999_999i64,
                                                _ => panic!("Unexpected retry or extra range"),
                                            },
                                        );
                                    }
                                }
                            }
                            Ok(reply)
                        },
                    )
                    .unwrap();
                assert_eq!(requests, 3);
                assert_eq!(result.bytes, raw);
                assert_eq!(result.md5, descriptor.expected_md5.as_deref().unwrap());
                assert_eq!(result.sha256, hex::encode(sha256(&raw)));
                assert_eq!(result.requested_quality, MediaQuality::LargeFile);
                assert_eq!(result.substituted_filetype, None);
                assert_eq!(result.picture_format, None);
            }
        }
    }
    #[test]
    fn file7_metadata_parser_bounds_and_errors_remain_strict() {
        let (descriptor, _, ciphertext) = download_fixture(512 * 1024, MediaQuality::LargeFile);
        for key in [
            "isretry",
            "retrysec",
            "isoverload",
            "isgetcdn",
            "istrans",
            "backsrcnum",
            "substituteftype",
            "rsppicformat",
        ] {
            for malformed in [
                b"".as_slice(),
                b"-1",
                b"-0",
                b"+1",
                b" 1",
                b"1x",
                b"99999999999999999",
                b"\xff",
            ] {
                let mut requests = 0;
                assert!(
                    client()
                        .download_with_transport(
                            &descriptor,
                            |_, _| Ok(()),
                            |_, fields| {
                                requests += 1;
                                let mut reply = download_reply(fields, &descriptor, &ciphertext);
                                reply.insert(key.into(), malformed.to_vec());
                                Ok(reply)
                            },
                        )
                        .is_err(),
                    "Accepted malformed {key}"
                );
                assert_eq!(requests, 1);
            }
        }
        let mut requests = 0;
        let error = client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    requests += 1;
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    for key in ["isretry", "retrysec", "isoverload", "isgetcdn"] {
                        put(&mut reply, key, 1);
                    }
                    put(&mut reply, "retcode", -1);
                    Ok(reply)
                },
            )
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "CDN download returned -1");
        assert_eq!(requests, 1);
    }
    #[test]
    fn normal_download_keeps_representation_metadata_and_consistency_checks() {
        let (descriptor, raw, ciphertext) = download_fixture(65536, MediaQuality::File);
        let result = client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |path, fields| {
                    assert_eq!(path, "/download");
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    put(&mut reply, "substituteftype", 5);
                    put(&mut reply, "rsppicformat", 3);
                    Ok(reply)
                },
            )
            .unwrap();
        assert_eq!(result.bytes, raw);
        assert_eq!(result.substituted_filetype, Some(5));
        assert_eq!(result.picture_format, Some(3));
        let mut requests = 0;
        assert!(client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    requests += 1;
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    put(&mut reply, "rsppicformat", requests);
                    Ok(reply)
                },
            )
            .is_err());
        assert_eq!(requests, 2);
    }
    #[test]
    fn download_refuses_truncated_overlong_and_changing_source_bytes() {
        let (descriptor, _, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        for failure in [
            "empty",
            "overlong",
            "source-checksum",
            "changed-checksum",
            "changed-total",
            "short-nonfinal",
        ] {
            let mut requests = 0;
            let outcome = client().download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    requests += 1;
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    match failure {
                        "empty" => {
                            reply.insert("filedata".into(), vec![]);
                        }
                        "overlong" => {
                            reply.get_mut("filedata").unwrap().push(1);
                            put(&mut reply, "rangeend", 512 * 1024);
                        }
                        "source-checksum" => {
                            reply.insert(
                                "rawfilemd5".into(),
                                b"0123456789abcdef0123456789abcdef".to_vec(),
                            );
                        }
                        "changed-checksum" if requests == 2 => {
                            reply.insert(
                                "rawfilemd5".into(),
                                b"0123456789abcdef0123456789abcdef".to_vec(),
                            );
                        }
                        "changed-total" if requests == 2 => {
                            put(&mut reply, "totalsize", ciphertext.len() + 16)
                        }
                        "short-nonfinal" => {
                            reply.insert("filedata".into(), vec![1]);
                            put(&mut reply, "rangeend", integer(fields, "rangestart")?);
                        }
                        _ => {}
                    }
                    Ok(reply)
                },
            );
            assert!(outcome.is_err(), "Accepted {failure}");
            assert!(requests <= 2);
        }
    }
    #[test]
    fn large_download_checks_padding_actual_size_and_digest_independently() {
        for omit_source_size in [false, true] {
            let reply_for_test =
                |fields: &CdnFields, descriptor: &MediaDescriptor, bytes: &[u8]| {
                    let mut reply = download_reply(fields, descriptor, bytes);
                    if omit_source_size {
                        reply.remove("rawfilesize");
                    }
                    reply
                };
            let (descriptor, _, ciphertext) =
                download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
            let mut invalid_padding = ciphertext.clone();
            let mut encoder = openssl::symm::Crypter::new(
                Cipher::aes_128_ecb(),
                openssl::symm::Mode::Encrypt,
                &descriptor.aes_key,
                None,
            )
            .unwrap();
            encoder.pad(false);
            let mut block = [0u8; 32];
            let written = encoder.update(&[0u8; 16], &mut block).unwrap();
            assert_eq!(
                written + encoder.finalize(&mut block[written..]).unwrap(),
                16
            );
            let last = invalid_padding.len() - 16;
            invalid_padding[last..].copy_from_slice(&block[..16]);
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| Ok(reply_for_test(fields, &descriptor, &invalid_padding))
                )
                .is_err());
            let mut without_advertised_md5 = descriptor.clone();
            without_advertised_md5.expected_md5 = None;
            for change_later in [false, true] {
                let mut requests = 0;
                assert!(client()
                    .download_with_transport(
                        &without_advertised_md5,
                        |_, _| Ok(()),
                        |_, fields| {
                            requests += 1;
                            let mut reply = reply_for_test(fields, &descriptor, &ciphertext);
                            if !change_later || requests == 2 {
                                reply.insert(
                                    "rawfilemd5".into(),
                                    b"0123456789abcdef0123456789abcdef".to_vec(),
                                );
                            }
                            Ok(reply)
                        }
                    )
                    .is_err());
                assert_eq!(requests, 2);
            }
            let mut wrong_source = ciphertext.clone();
            wrong_source[0] ^= 1;
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| Ok(reply_for_test(fields, &descriptor, &wrong_source))
                )
                .is_err());
            let mut wrong_size = descriptor.clone();
            wrong_size.expected_bytes = Some(descriptor.expected_bytes.unwrap() + 1);
            wrong_size.strict_size = false;
            assert!(client()
                .download_with_transport(
                    &wrong_size,
                    |_, _| Ok(()),
                    |_, fields| Ok(reply_for_test(fields, &wrong_size, &ciphertext))
                )
                .is_err());
        }
    }
    #[test]
    fn cancellation_and_transport_failure_never_start_another_download_request() {
        let (descriptor, _, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        for stop in [1, 2, 3, 4] {
            let mut callbacks = 0;
            let mut requests = 0;
            let outcome = client().download_with_transport(
                &descriptor,
                |_, _| {
                    callbacks += 1;
                    ensure!(callbacks != stop, "cancelled");
                    Ok(())
                },
                |_, fields| {
                    requests += 1;
                    Ok(download_reply(fields, &descriptor, &ciphertext))
                },
            );
            assert!(outcome.is_err());
            assert_eq!(callbacks, stop);
            assert_eq!(requests, stop / 2);
        }
        let mut requests = 0;
        assert!(client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, _| {
                    requests += 1;
                    bail!("transport failure")
                }
            )
            .is_err());
        assert_eq!(requests, 1);
    }
    #[test]
    fn normal_download_keeps_optional_sequence_and_original_limits() {
        for quality in [
            MediaQuality::Original,
            MediaQuality::Standard,
            MediaQuality::Thumbnail,
            MediaQuality::Video,
            MediaQuality::File,
        ] {
            let (descriptor, raw, ciphertext) = download_fixture(65536 + 17, quality);
            let mut requests = 0;
            let downloaded = client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |path, fields| {
                        requests += 1;
                        assert_eq!(path, "/download");
                        assert_eq!(
                            integer(fields, "rangeend")? - integer(fields, "rangestart")? + 1,
                            if requests == 1 { 65536 } else { 32 }
                        );
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
                        reply.remove("seq");
                        Ok(reply)
                    },
                )
                .unwrap();
            assert_eq!(downloaded.bytes, raw);
            assert_eq!(requests, 2);
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| {
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
                        put(&mut reply, "seq", 99);
                        Ok(reply)
                    }
                )
                .is_err());
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| {
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
                        put(&mut reply, "totalsize", MAX_CDN_TRANSFER_BYTES + 32);
                        Ok(reply)
                    }
                )
                .is_err());
        }
        assert!(unpack_cdn(&vec![0; MAX_CDN_BODY + 1]).is_err());
    }
    #[test]
    fn large_descriptor_limits_fail_before_transport() {
        let (descriptor, _, _) = download_fixture(16, MediaQuality::LargeFile);
        for expected in [
            None,
            Some(0),
            Some(MAX_LARGE_FILE_BYTES as u64 + 1),
            Some(u64::MAX),
        ] {
            let mut descriptor = descriptor.clone();
            descriptor.expected_bytes = expected;
            let mut requests = 0;
            assert!(client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, _| {
                        requests += 1;
                        bail!("must not request")
                    }
                )
                .is_err());
            assert_eq!(requests, 0);
        }
    }

    #[test]
    fn legacy_completion_receipt_survives_cancellation_after_final_ack() {
        let prepared = PreparedUpload::prepare(
            b"synthetic source",
            MediaQuality::File,
            "source.bin",
            "filehelper",
        )
        .unwrap();
        let cancelled = std::cell::Cell::new(false);
        let calls = std::cell::Cell::new(0);
        let upload = client()
            .upload_with_transport(
                &prepared,
                |_, _| {
                    ensure!(!cancelled.get(), "cancelled");
                    Ok(())
                },
                |path, fields| {
                    assert_eq!(path, "/uploadv3");
                    calls.set(calls.get() + 1);
                    let mut reply = CdnFields::new();
                    put(&mut reply, "retcode", 0);
                    for key in ["seq", "rangestart", "rangeend"] {
                        reply.insert(key.into(), fields[key].clone());
                    }
                    reply.insert("fileid".into(), b"synthetic-completed-object".to_vec());
                    cancelled.set(true);
                    Ok(reply)
                },
            )
            .unwrap();
        assert!(cancelled.get());
        assert_eq!(calls.get(), 1);
        assert_eq!(upload.descriptor.file_id, "synthetic-completed-object");
    }

    #[test]
    fn paired_video_preserves_final_receipt_and_does_not_admit_large_normal() {
        let mut thumbnail = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut thumbnail)
            .encode_image(&image::DynamicImage::new_rgb8(16, 12))
            .unwrap();
        let p = PreparedVideoUpload::with_material(
            b"\0\0\0\x0cftypisom",
            &thumbnail,
            "normal.mp4",
            "filehelper",
            &[0x31; 16],
            "00112233445566778899aabbccddeeff",
        )
        .unwrap();
        let cancelled = std::cell::Cell::new(false);
        let uploaded = client()
            .upload_video_with_transport(
                &p,
                |_, _| {
                    ensure!(!cancelled.get(), "cancelled");
                    Ok(())
                },
                |path, fields| {
                    assert_eq!(path, "/uploadv3");
                    assert!(fields.contains_key("thumbdata"));
                    assert_eq!(integer(fields, "filetype")?, 4);
                    let mut reply = CdnFields::new();
                    put(&mut reply, "retcode", 0);
                    for key in ["seq", "rangestart", "rangeend"] {
                        reply.insert(key.into(), fields[key].clone());
                    }
                    reply.insert("fileid".into(), b"paired-object".to_vec());
                    cancelled.set(true);
                    Ok(reply)
                },
            )
            .unwrap();
        assert!(uploaded.original.is_none());
        assert_eq!(uploaded.video.descriptor.file_id, "paired-object");
        assert_eq!(uploaded.thumbnail.descriptor.file_id, "paired-object");
        let mut altered = p;
        altered.video.plaintext_bytes = MAX_NATIVE_UPLOAD_BYTES as u32 + 1;
        assert!(client()
            .upload_video_with_transport(
                &altered,
                |_, _| Ok(()),
                |_, _| { panic!("Large normal video reached legacy transport") }
            )
            .is_err());
    }
    #[test]
    fn boundary_matrix_keeps_ciphertext_and_media_kind_limits_separate() {
        let boundary = MAX_CDN_TRANSFER_BYTES;
        for (size, padded, legacy) in [
            (boundary - 17, boundary - 16, Some("/uploadv3")),
            (boundary - 16, boundary, None),
            (boundary - 1, boundary, None),
            (boundary, boundary + 16, None),
            (boundary + 1, boundary + 16, None),
            (MAX_LARGE_FILE_BYTES, MAX_LARGE_FILE_BYTES + 16, None),
            (MAX_LARGE_FILE_BYTES + 1, MAX_LARGE_FILE_BYTES + 16, None),
        ] {
            assert_eq!((size / 16 + 1) * 16, padded);
            let kind = if size > boundary {
                MediaQuality::LargeFile
            } else {
                MediaQuality::File
            };
            assert_eq!(legacy_upload_path(kind, size, padded).ok(), legacy);
            assert_eq!(
                size <= upload_plaintext_limit(kind),
                size <= MAX_LARGE_FILE_BYTES
            );
            assert_eq!(
                size <= upload_plaintext_limit(MediaQuality::Original),
                size <= MAX_NATIVE_UPLOAD_BYTES
            );
            assert_eq!(
                size <= upload_plaintext_limit(MediaQuality::Video),
                size <= wechat_media::MAX_VIDEO_SOURCE_BYTES
            );
        }
        assert_eq!(MAX_LARGE_FILE_BYTES, 536_870_912);
        assert_eq!(MAX_NATIVE_UPLOAD_BYTES, 26_214_383);
        assert_eq!(MAX_CDN_TRANSFER_BYTES, 26_214_400);
    }
    #[test]
    fn multiphase_paths_bind_account_sequence_type_and_phase() {
        for filetype in [4, 5, 7] {
            for phase in ["apply", "uploadblock", "fin", "complete"] {
                assert!(large_file_path(
                    &format!("/cdn/12345/3/{filetype}/{phase}"),
                    12345,
                    3
                ));
            }
        }
        for path in [
            "/cdn/999/3/7/apply",
            "/cdn/12345/2/7/apply",
            "/cdn/12345/03/7/apply",
            "/cdn/12345/0/7/apply",
            "/cdn/12345/3/3/apply",
            "/cdn/12345/3/7/unknown",
            "/cdn/12345/3/7/apply?x=1",
            "/cdn/12345/3/7/apply/extra",
            "//cdn/12345/3/7/apply",
            "/cdn/12345/3/7/../apply",
        ] {
            assert!(!large_file_path(path, 12345, 3), "Accepted {path}");
        }
        assert!(!large_file_path("/cdn/12345/0/7/apply", 12345, 0));
    }
    #[test]
    fn cdn_codec_rejects_duplicate_and_truncated() {
        let fields = BTreeMap::from([
            ("retcode".to_string(), b"0".to_vec()),
            ("filedata".into(), vec![0, 1, 2, 255]),
        ]);
        let packed = pack_cdn(&fields).unwrap();
        assert_eq!(unpack_cdn(&packed).unwrap(), fields);
        let one = pack_cdn(&BTreeMap::from([("x".into(), vec![1])])).unwrap();
        for n in 1..one.len() {
            assert!(unpack_cdn(&one[..n]).is_err());
        }
        let duplicate = [one.clone(), one].concat();
        assert!(unpack_cdn(&duplicate).is_err());
    }
    #[test]
    fn prepared_upload_roundtrip() {
        let p = PreparedUpload::prepare(
            b"synthetic attachment",
            MediaQuality::File,
            "test.txt",
            "filehelper",
        )
        .unwrap();
        p.validate().unwrap();
        assert_eq!(
            decrypt_cdn(&p.encrypted, &p.aes_key).unwrap(),
            b"synthetic attachment"
        );
        let mut corrupt = p.clone();
        corrupt.encrypted[0] ^= 1;
        assert!(corrupt.validate().is_err());
    }
    #[test]
    fn nonpublic_addresses_rejected() {
        for ip in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.1.2.3",
        ] {
            assert!(!public_ipv4(ip.parse().unwrap()));
        }
        assert!(public_ipv4("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn upload_reply_correlates_ranges() {
        let mut f = CdnFields::from([
            ("retcode".into(), b"0".to_vec()),
            ("seq".into(), b"2".to_vec()),
        ]);
        assert!(validate_upload_reply(&f, 1, 0, 15, &mut None).is_err());
        put(&mut f, "seq", 1);
        put(&mut f, "rangestart", 0);
        put(&mut f, "rangeend", 15);
        f.insert("fileid".into(), b"synthetic-file-id".to_vec());
        let mut id = None;
        validate_upload_reply(&f, 1, 0, 15, &mut id).unwrap();
        assert_eq!(id, Some("synthetic-file-id".into()));
    }
    #[test]
    fn prepared_jpeg_variants_are_real_and_independent() {
        fn jpeg(w: u32, h: u32) -> Vec<u8> {
            let rgb = image::RgbImage::from_pixel(w, h, image::Rgb([12, 40, 80]));
            let mut out = vec![];
            image::codecs::jpeg::JpegEncoder::new(&mut out)
                .encode_image(&rgb)
                .unwrap();
            out
        }
        let p = PreparedImageUpload::prepare(
            &jpeg(64, 48),
            &jpeg(32, 24),
            &jpeg(16, 12),
            "image.jpg",
            "filehelper",
        )
        .unwrap();
        p.validate().unwrap();
        assert_eq!((p.width, p.height), (64, 48));
        assert_eq!((p.thumbnail.width, p.thumbnail.height), (16, 12));
        assert!(PreparedImageUpload::prepare(
            &jpeg(8, 8),
            &jpeg(16, 16),
            &jpeg(4, 4),
            "image.jpg",
            "filehelper"
        )
        .is_err());
        assert!(PreparedImageUpload::prepare(
            b"not image",
            &jpeg(4, 4),
            &jpeg(2, 2),
            "image.jpg",
            "filehelper"
        )
        .is_err());
    }
    #[test]
    fn image_original_uploads_in_ranges_with_previews_in_the_first_request() {
        fn jpeg(w: u32, h: u32, noisy: bool) -> Vec<u8> {
            let mut seed = 7u32;
            let rgb = image::RgbImage::from_fn(w, h, |_, _| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let [r, g, b, _] = if noisy { seed } else { 0x0050_280c }.to_le_bytes();
                image::Rgb([r, g, b])
            });
            let mut out = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95)
                .encode_image(&rgb)
                .unwrap();
            out
        }
        // Wider than the 4,096-pixel preview limit and several ranges long.
        let original = jpeg(4200, 64, true);
        assert!(original.len() > 3 * UPLOAD_CHUNK);
        let p = PreparedImageUpload::prepare(
            &original,
            &jpeg(640, 10, false),
            &jpeg(160, 3, false),
            "wide.jpg",
            "filehelper",
        )
        .unwrap();
        assert_eq!((p.width, p.height), (4200, 64));
        let total = p.original.encrypted.len();
        let requests = std::cell::RefCell::new(vec![]);
        let reported = std::cell::RefCell::new(vec![]);
        let uploaded = client()
            .upload_image_with_transport(
                &p,
                |sent, all| {
                    assert_eq!(all, Some(total as u64));
                    reported.borrow_mut().push(sent);
                    Ok(())
                },
                |path, fields| {
                    assert_eq!(path, "/uploadv3");
                    assert_eq!(integer(fields, "filetype")?, 1);
                    assert_eq!(integer(fields, "totalsize")?, total as i64);
                    // Every request must fit the bounded CDN envelope.
                    pack_cdn(fields)?;
                    let start = integer(fields, "rangestart")? as usize;
                    let end = integer(fields, "rangeend")? as usize;
                    assert_eq!(fields["filedata"], p.original.encrypted[start..=end]);
                    let first = start == 0;
                    for key in ["thumbdata", "midimgdata", "midimgmd5", "imgwidth"] {
                        assert_eq!(fields.contains_key(key), first, "{key}");
                    }
                    assert_eq!(integer(fields, "hasthumb")?, i64::from(first));
                    requests.borrow_mut().push((start, end));
                    let mut reply = CdnFields::new();
                    put(&mut reply, "retcode", 0);
                    for key in ["seq", "rangestart", "rangeend"] {
                        reply.insert(key.into(), fields[key].clone());
                    }
                    reply.insert("fileid".into(), b"image-object".to_vec());
                    Ok(reply)
                },
            )
            .unwrap();
        let requests = requests.into_inner();
        assert_eq!(requests.len(), total.div_ceil(UPLOAD_CHUNK));
        assert_eq!(requests[0], (0, UPLOAD_CHUNK - 1));
        assert!(requests.windows(2).all(|pair| pair[0].1 + 1 == pair[1].0));
        assert_eq!(requests.last().unwrap().1, total - 1);
        assert_eq!(reported.borrow()[0], 0);
        assert!(reported.borrow().iter().all(|sent| *sent < total as u64));
        assert_eq!(uploaded.original.plaintext_bytes as usize, original.len());
        assert_eq!(uploaded.original.width, Some(4200));
        for copy in [&uploaded.original, &uploaded.standard, &uploaded.thumbnail] {
            assert_eq!(copy.descriptor.file_id, "image-object");
        }
        // A standard copy keeps its own limits.
        assert!(PreparedImageUpload::prepare(
            &original,
            &jpeg(4200, 64, false),
            &jpeg(160, 3, false),
            "wide.jpg",
            "filehelper"
        )
        .is_err());
    }
    fn video_mismatch(
        descriptor: &MediaDescriptor,
        ciphertext: &[u8],
        extra: &[(&str, &[u8])],
    ) -> VideoRepresentationMismatch {
        let error = client()
            .download_with_transport(
                descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    let mut reply = download_reply(fields, descriptor, ciphertext);
                    for (key, value) in extra {
                        reply.insert((*key).into(), value.to_vec());
                    }
                    Ok(reply)
                },
            )
            .err()
            .expect("mismatched video must be rejected");
        let text = format!("{error:#} {error:?}");
        assert!(!text.contains(descriptor.expected_md5.as_deref().unwrap()));
        *error
            .downcast_ref::<VideoRepresentationMismatch>()
            .expect("typed video mismatch")
    }
    #[test]
    fn normal_video_of_another_length_is_rejected_with_comparison_facts_only() {
        // Synthetic values: the advertised tuple describes one object while the
        // CDN returns a complete, correctly encrypted object of another length.
        let (descriptor, _, _) = download_fixture(9_390, MediaQuality::Video);
        let delivered = vec![0x6b; 15_272];
        let delivered_md5 = hex::encode(md5(&delivered));
        let ciphertext = ecb_encrypt(&delivered, &descriptor.aes_key).unwrap();
        let bare = video_mismatch(&descriptor, &ciphertext, &[]);
        assert_eq!(
            bare,
            VideoRepresentationMismatch {
                advertised_bytes: Some(9_390),
                delivered_bytes: 15_272,
                advertised_digest_matches: Some(false),
                substituted_filetype: None,
                istrans: None,
                videoformat: None,
                rawfilesize: None,
                rawfilemd5: None,
            }
        );
        let delivered_size = delivered.len().to_string();
        let described = video_mismatch(
            &descriptor,
            &ciphertext,
            &[
                ("substituteftype", b"4"),
                ("istrans", b"7"),
                ("videoformat", b"3"),
                ("rawfilesize", delivered_size.as_bytes()),
                ("rawfilemd5", delivered_md5.to_uppercase().as_bytes()),
            ],
        );
        assert_eq!(described.substituted_filetype, Some(4));
        assert_eq!(
            (described.istrans, described.videoformat),
            (Some(7), Some(3))
        );
        let delivered_only = Some(SourceEvidence {
            matches_advertised: false,
            matches_delivered: true,
        });
        assert_eq!(described.rawfilesize, delivered_only);
        assert_eq!(described.rawfilemd5, delivered_only);
        let advertised_size = descriptor.expected_bytes.unwrap().to_string();
        let advertised_md5 = descriptor.expected_md5.clone().unwrap();
        let source = video_mismatch(
            &descriptor,
            &ciphertext,
            &[
                ("rawfilesize", advertised_size.as_bytes()),
                ("rawfilemd5", advertised_md5.as_bytes()),
            ],
        );
        let advertised_only = Some(SourceEvidence {
            matches_advertised: true,
            matches_delivered: false,
        });
        assert_eq!(source.rawfilesize, advertised_only);
        assert_eq!(source.rawfilemd5, advertised_only);
        let malformed = video_mismatch(
            &descriptor,
            &ciphertext,
            &[("rawfilesize", b"-1"), ("rawfilemd5", b"not-a-digest")],
        );
        let neither = Some(SourceEvidence {
            matches_advertised: false,
            matches_delivered: false,
        });
        assert_eq!(
            (malformed.rawfilesize, malformed.rawfilemd5),
            (neither, neither)
        );
        // A representation that only some chunks label is still refused.
        assert!(client()
            .download_with_transport(
                &descriptor,
                |_, _| Ok(()),
                |_, fields| {
                    let mut reply = download_reply(fields, &descriptor, &ciphertext);
                    put(&mut reply, "substituteftype", integer(fields, "seq")?);
                    Ok(reply)
                },
            )
            .is_err());
    }
    #[test]
    fn normal_video_digest_mismatch_and_valid_metadata_keep_existing_outcomes() {
        let (mut descriptor, raw, ciphertext) = download_fixture(9_390, MediaQuality::Video);
        // Optional metadata is diagnostic only; malformed values never reject
        // a transfer that matches its advertised tuple.
        for (key, value) in [
            ("rawfilesize", "-1"),
            ("rawfilemd5", "not-a-digest"),
            ("istrans", "x"),
            ("videoformat", "12345678901234567"),
        ] {
            let downloaded = client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |_, fields| {
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
                        reply.insert(key.into(), value.as_bytes().to_vec());
                        Ok(reply)
                    },
                )
                .unwrap();
            assert_eq!(downloaded.bytes, raw);
            assert_eq!(downloaded.advertised_size_matches, Some(true));
        }
        descriptor.expected_md5 = Some("0".repeat(32));
        let same_size = video_mismatch(&descriptor, &ciphertext, &[]);
        assert_eq!(same_size.delivered_bytes, 9_390);
        assert_eq!(same_size.advertised_digest_matches, Some(false));
        // Other kinds keep their plain messages.
        let (mut image, _, image_ciphertext) = download_fixture(9_390, MediaQuality::Original);
        image.expected_bytes = Some(9_391);
        let error = client()
            .download_with_transport(
                &image,
                |_, _| Ok(()),
                |_, fields| Ok(download_reply(fields, &image, &image_ciphertext)),
            )
            .err()
            .expect("download must fail");
        assert!(error
            .downcast_ref::<VideoRepresentationMismatch>()
            .is_none());
        assert_eq!(error.to_string(), "Advertised media size mismatch");
    }
    #[test]
    fn video_limits_admit_sizes_above_the_former_50_mib_bound() {
        assert_eq!(MAX_VIDEO_DOWNLOAD_BYTES, 536_870_912);
        assert_eq!(wechat_media::MAX_VIDEO_SOURCE_BYTES, 536_870_912);
        assert_eq!(
            upload_plaintext_limit(MediaQuality::Video),
            wechat_media::MAX_VIDEO_SOURCE_BYTES
        );
        // A small advertised video keeps its own bound on an unchecked reply.
        assert_eq!(MAX_SMALL_VIDEO_REPLY_BYTES, 52_428_800);
        let (mut descriptor, _, _) = download_fixture(16, MediaQuality::Video);
        for size in [MAX_SMALL_VIDEO_REPLY_BYTES + 1, MAX_VIDEO_DOWNLOAD_BYTES] {
            descriptor.expected_bytes = Some(size as u64);
            let mut requests = 0;
            let error = client()
                .download_with_transport(
                    &descriptor,
                    |_, _| Ok(()),
                    |path, fields| {
                        requests += 1;
                        assert_eq!(path, "/download");
                        assert_eq!(integer(fields, "filetype")?, 4);
                        assert_eq!(integer(fields, "rangeend")?, 512 * 1024 - 1);
                        bail!("synthetic transport stop")
                    },
                )
                .err()
                .expect("download must stop");
            assert_eq!(error.to_string(), "synthetic transport stop");
            assert_eq!(requests, 1);
        }
    }
}
