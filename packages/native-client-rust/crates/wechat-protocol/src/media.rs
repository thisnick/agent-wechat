//! Bounded native CDN transport. This uses the observed HTTP carrier on port 443,
//! not HTTPS. Media encryption and advertised size/digests are checked separately.
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
    ensure!(code == 0, "CDN bootstrap returned {code}");
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
        && matches!(parts[4], "5" | "7")
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
        let plaintext_limit = if large {
            MAX_LARGE_FILE_BYTES
        } else {
            MAX_CDN_TRANSFER_BYTES
        };
        let chunk_bytes = if large { 512 * 1024 } else { 65536 };
        let ciphertext_limit = (plaintext_limit / 16 + 1) * 16;
        let expected_ciphertext = if large {
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
            None
        };
        let mut position = 0usize;
        let mut total = 0usize;
        let mut encrypted = vec![];
        let mut substituted_filetype = None;
        let mut picture_format = None;
        let mut server_md5 = None;
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
            ensure!(code == 0, "CDN download returned {code}");
            if large || reply.contains_key("seq") {
                ensure!(
                    integer(&reply, "seq")? == i64::from(sequence),
                    "CDN download reply sequence mismatch"
                );
            }
            if large {
                validate_large_download_reply(&reply, descriptor, &mut server_md5)?;
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
                    && (!large || data.len() == (size - position).min(chunk_bytes))
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
        ensure!(
            !(large || descriptor.strict_size) || size_matches != Some(false),
            "Advertised media size mismatch"
        );
        let digest = hex::encode(md5(&bytes));
        if let Some(expected) = &descriptor.expected_md5 {
            ensure!(
                expected.len() == 32
                    && expected.bytes().all(|b| b.is_ascii_hexdigit())
                    && digest.eq_ignore_ascii_case(expected),
                "Advertised media checksum mismatch"
            );
        }
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
    ensure!(
        reply.keys().all(|key| FIELDS.contains(&key.as_str())),
        "Unsupported large-file download response field"
    );
    for (key, expected) in [
        ("ver", 0),
        ("retrysec", 0),
        ("isretry", 0),
        ("isoverload", 0),
        ("isgetcdn", 0),
        ("istrans", 999),
        ("backsrcnum", 0),
        ("substituteftype", 7),
        ("rsppicformat", 0),
        ("videoformat", 0),
    ] {
        ensure!(
            integer(reply, key)? == expected,
            "Unsupported large-file download control"
        );
    }
    ensure!(
        u64::try_from(integer(reply, "rawfilesize")?)? == descriptor.expected_bytes.unwrap_or(0),
        "Large-file download source size mismatch"
    );
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
    ensure!(code == 0, "CDN upload returned {code}");
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
        let (width, height) = jpeg_dimensions(original)?;
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
        let (width, height) = jpeg_dimensions(original)?;
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
            jpeg_dimensions(&raw)? == (self.width, self.height),
            "Prepared original dimensions differ"
        );
        self.standard.validate(&self.original.aes_key)?;
        self.thumbnail.validate(&self.original.aes_key)?;
        ensure!(
            self.original.plaintext_bytes <= 65520
                && self.standard.width <= self.width
                && self.standard.height <= self.height
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
        p.validate()?;
        let end = p.original.encrypted.len() - 1;
        let mut f = upload_fields(&self.config, &p.original, 1, 0, end)?;
        f.insert("filedata".into(), p.original.encrypted.clone());
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
        let reply = self.post("/uploadv3", &f)?;
        let mut file_id = None;
        validate_upload_reply(&reply, 1, 0, end, &mut file_id)?;
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
        mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedVideo> {
        p.video.validate()?;
        ensure!(p.video.kind == MediaQuality::Video, "Invalid video kind");
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
            let reply = self.post("/uploadv3", &fields)?;
            validate_upload_reply(&reply, sequence as u32 + 1, start, end, &mut file_id)?;
            progress((end + 1) as u64, Some(p.video.encrypted.len() as u64))?;
        }
        let id =
            file_id.ok_or_else(|| anyhow::anyhow!("Video upload returned no file identifier"))?;
        Ok(UploadedVideo {
            video: uploaded_primary(&p.video, &id, None, None),
            thumbnail: uploaded_variant(&p.video, &p.thumbnail, &id, MediaQuality::Thumbnail),
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
        assert_eq!(downloaded.substituted_filetype, Some(7));
        assert_eq!(downloaded.advertised_size_matches, Some(true));
    }
    #[test]
    fn large_download_rejects_correlation_routing_unknown_fields_and_ranges() {
        let (descriptor, _, ciphertext) =
            download_fixture(512 * 1024 + 17, MediaQuality::LargeFile);
        for (key, value) in [
            ("seq", 2),
            ("ver", 1),
            ("retcode", -1),
            ("isretry", 1),
            ("retrysec", 1),
            ("isoverload", 1),
            ("isgetcdn", 1),
            ("istrans", 0),
            ("backsrcnum", 1),
            ("substituteftype", 5),
            ("rsppicformat", 1),
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
        for missing in ["seq", "isgetcdn", "rawfilemd5", "filedata"] {
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
                |_, fields| Ok(download_reply(fields, &descriptor, &invalid_padding))
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
                        let mut reply = download_reply(fields, &descriptor, &ciphertext);
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
                |_, fields| Ok(download_reply(fields, &descriptor, &wrong_source))
            )
            .is_err());
        let mut wrong_size = descriptor.clone();
        wrong_size.expected_bytes = Some(descriptor.expected_bytes.unwrap() + 1);
        wrong_size.strict_size = false;
        assert!(client()
            .download_with_transport(
                &wrong_size,
                |_, _| Ok(()),
                |_, fields| Ok(download_reply(fields, &wrong_size, &ciphertext))
            )
            .is_err());
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
            for kind in [MediaQuality::Original, MediaQuality::Video] {
                assert_eq!(
                    size <= upload_plaintext_limit(kind),
                    size <= MAX_NATIVE_UPLOAD_BYTES
                );
            }
        }
        assert_eq!(MAX_LARGE_FILE_BYTES, 536_870_912);
        assert_eq!(MAX_NATIVE_UPLOAD_BYTES, 26_214_383);
        assert_eq!(MAX_CDN_TRANSFER_BYTES, 26_214_400);
    }
    #[test]
    fn multiphase_paths_bind_account_sequence_type_and_phase() {
        for filetype in [5, 7] {
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
            "/cdn/12345/3/4/apply",
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
}
