//! Bounded native CDN transport. This uses the observed HTTP carrier on port 443,
//! not HTTPS. Media encryption and advertised size/digests are checked separately.
use crate::{
    crypto::{md5, random, sha256},
    protobuf as p, AccountState, MAX_MESSAGE_BYTES,
};
use anyhow::{bail, ensure, Result};
use openssl::symm::Cipher;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, io::Read, net::Ipv4Addr, time::Duration};
use zeroize::Zeroize;
pub type CdnFields = BTreeMap<String, Vec<u8>>;
const MAX_CDN_BODY: usize = 2 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 65536;

#[derive(Clone, Serialize, Deserialize)]
pub struct CdnConfig {
    pub version: u32,
    pub uin: u64,
    pub client_version: u32,
    pub client_os_type: String,
    pub auth_key: Vec<u8>,
    pub server_ips: Vec<Ipv4Addr>,
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
    ensure!(fields.len() <= 256, "Too many CDN fields");
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
            && encrypted.len() <= MAX_MESSAGE_BYTES + 16,
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
}
impl MediaQuality {
    pub fn filetype(self) -> u32 {
        match self {
            Self::Original => 1,
            Self::Standard => 2,
            Self::Thumbnail => 3,
            Self::Video => 4,
            Self::File => 5,
        }
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
    /// Prepare and persist this complete result before the first upload request.
    /// The small-object uploader deliberately caps plaintext at 1 MiB.
    pub fn prepare(
        raw: &[u8],
        kind: MediaQuality,
        file_name: &str,
        destination: &str,
    ) -> Result<Self> {
        ensure!(
            !raw.is_empty()
                && raw.len() <= 1024 * 1024
                && [
                    MediaQuality::File,
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
        encrypt_recipient(destination)?;
        let aes_key = random::<16>()?.to_vec();
        let encrypted = ecb_encrypt(raw, &aes_key)?;
        Ok(Self {
            kind,
            aes_key,
            file_key: hex::encode(random::<16>()?),
            encrypted,
            plaintext_bytes: raw.len() as u32,
            md5: hex::encode(md5(raw)),
            sha256: hex::encode(sha256(raw)),
            file_name: file_name.to_string(),
            destination: destination.to_string(),
        })
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.aes_key.len() == 16
                && self.file_key.len() == 32
                && self.file_key.bytes().all(|b| b.is_ascii_hexdigit())
                && self.plaintext_bytes > 0
                && self.plaintext_bytes <= 1024 * 1024
                && self.encrypted.len() == ((self.plaintext_bytes as usize / 16) + 1) * 16,
            "Invalid prepared upload"
        );
        let raw = decrypt_cdn(&self.encrypted, &self.aes_key)?;
        ensure!(
            raw.len() == self.plaintext_bytes as usize
                && hex::encode(md5(&raw)) == self.md5
                && hex::encode(sha256(&raw)) == self.sha256,
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
        ensure!(
            ["/download", "/uploadv3"].contains(&path),
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
        ensure!(
            !descriptor.file_id.is_empty()
                && descriptor.file_id.len() <= 8192
                && !descriptor.file_id.chars().any(|c| c.is_control())
                && descriptor.aes_key.len() == 16,
            "Invalid download descriptor"
        );
        if let Some(expiry) = descriptor.expires_at {
            ensure!(
                expiry > crate::wire::unix_time()? as u64,
                "Explicit media expiry has passed"
            );
        }
        let mut position = 0usize;
        let mut total = 0usize;
        let mut encrypted = vec![];
        let mut substituted_filetype = None;
        let mut picture_format = None;
        for sequence in 1..=257u32 {
            let mut fields = shared(&self.config, sequence, descriptor.quality.filetype());
            for (k, v) in [
                ("wxautostart", 0),
                ("downpicformat", 1),
                ("rangestart", position as u64),
                (
                    "rangeend",
                    if total == 0 {
                        65535
                    } else {
                        (total - 1).min(position + 65535)
                    } as u64,
                ),
            ] {
                put(&mut fields, k, v);
            }
            fields.insert("wxmsgflag".into(), vec![]);
            fields.insert("fileid".into(), descriptor.file_id.as_bytes().to_vec());
            let reply = self.post("/download", &fields)?;
            let code = integer(&reply, "retcode")?;
            ensure!(code == 0, "CDN download returned {code}");
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
                    && size <= MAX_MESSAGE_BYTES + 16
                    && size >= position
                    && data.len() <= size - position
                    && (total == 0 || total == size)
                    && end == start + data.len() - 1,
                "Inconsistent CDN range response"
            );
            total = size;
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
            if position == total {
                break;
            }
        }
        ensure!(
            total > 0 && position == total,
            "CDN transfer did not finish within bound"
        );
        let bytes = decrypt_cdn(&encrypted, &descriptor.aes_key)?;
        ensure!(
            bytes.len() <= MAX_MESSAGE_BYTES,
            "Decoded CDN body too large"
        );
        let size_matches = descriptor.expected_bytes.map(|n| n == bytes.len() as u64);
        ensure!(
            !descriptor.strict_size || size_matches != Some(false),
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
        prepared.validate()?;
        let mut file_id = None;
        for (sequence, chunk) in prepared.encrypted.chunks(UPLOAD_CHUNK).enumerate() {
            let start = sequence * UPLOAD_CHUNK;
            let end = start + chunk.len() - 1;
            let mut fields =
                upload_fields(&self.config, prepared, sequence as u32 + 1, start, end)?;
            fields.insert("filedata".into(), chunk.to_vec());
            let reply = self.post("/uploadv3", &fields)?;
            validate_upload_reply(&reply, sequence as u32 + 1, start, end, &mut file_id)?;
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
    pub fn prepare(
        video: &[u8],
        thumbnail: &[u8],
        file_name: &str,
        destination: &str,
    ) -> Result<Self> {
        ensure!(
            video.len() >= 12 && video.len() <= 65520 && video[4..8] == *b"ftyp",
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
        p.video.validate()?;
        ensure!(
            p.video.kind == MediaQuality::Video && p.video.plaintext_bytes <= 65520,
            "Video outside small-object profile"
        );
        p.thumbnail.validate(&p.video.aes_key)?;
        ensure!(
            p.thumbnail.width <= 320 && p.thumbnail.height <= 320,
            "Video thumbnail outside limit"
        );
        let end = p.video.encrypted.len() - 1;
        let mut f = upload_fields(&self.config, &p.video, 1, 0, end)?;
        f.insert("filedata".into(), p.video.encrypted.clone());
        add_thumbnail(&mut f, &p.thumbnail);
        put(&mut f, "smallvideoflag", 0);
        let reply = self.post("/uploadv3", &f)?;
        let mut file_id = None;
        validate_upload_reply(&reply, 1, 0, end, &mut file_id)?;
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
