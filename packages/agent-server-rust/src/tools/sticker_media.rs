//! Server-owned sticker retrieval. No WeChat UI, native queue, or cache writes.
use super::media_expiry::{expiry_timestamp, finish_expired};
use crate::ia::types::MediaResult;
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use image::{AnimationDecoder, ImageDecoder, ImageFormat};
use md5::{Digest, Md5};
use reqwest::Url;
use sha2::Sha256;
use std::{
    collections::HashMap,
    io::{Cursor, Write},
    net::{IpAddr, SocketAddr},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::{watch, Mutex, Semaphore};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const CACHE_BUDGET: u64 = 512 * 1024 * 1024; // Per account; evict least recently read.
const MAX_PIXELS: u64 = 4_000_000;
const MAX_DECODED_PIXELS: u64 = 100_000_000;
const MAX_FRAMES: usize = 500;
const JOB_TIMEOUT: Duration = Duration::from_secs(20);
const FAILURE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct Failure {
    reason: &'static str,
    retryable: bool,
}

impl Failure {
    pub fn terminal(reason: &'static str) -> Self {
        Self {
            reason,
            retryable: false,
        }
    }
    fn transient(reason: &'static str) -> Self {
        Self {
            reason,
            retryable: true,
        }
    }
    pub fn result(&self) -> MediaResult {
        MediaResult {
            media_type: if self.retryable { "pending" } else { "emoji" }.into(),
            reason: Some(self.reason.into()),
            retryable: Some(self.retryable),
            ..Default::default()
        }
    }
}

#[derive(Clone)]
pub struct Sticker {
    md5: String,
    bytes: Option<usize>,
    url: Option<String>,
    expires_at: Option<i64>,
}

impl Sticker {
    pub fn parse(content: &str, created_at: i64) -> Result<Self, Failure> {
        let invalid = || Failure::terminal("sticker_metadata_invalid");
        if content.len() > 1024 * 1024 {
            return Err(invalid());
        }
        let document = roxmltree::Document::parse_with_options(
            content,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 4096,
                ..Default::default()
            },
        )
        .map_err(|_| invalid())?;
        let root = document.root_element();
        if !root.has_tag_name("msg") || root.tag_name().namespace().is_some() {
            return Err(invalid());
        }
        let mut nodes = root.children().filter(|n| n.has_tag_name("emoji"));
        let node = nodes.next().ok_or_else(invalid)?;
        if nodes.next().is_some() || node.tag_name().namespace().is_some() {
            return Err(invalid());
        }
        let md5 = node.attribute("md5").unwrap_or("");
        if !valid_hash(md5) {
            return Err(invalid());
        }
        let bytes = node
            .attribute("len")
            .filter(|n| !n.is_empty())
            .map(|n| n.parse::<usize>().map_err(|_| invalid()))
            .transpose()?
            .filter(|n| *n > 0);
        if bytes.is_some_and(|n| n > MAX_BYTES) {
            return Err(Failure::terminal("sticker_too_large"));
        }
        // Only the original resource is validated. Do not substitute a thumbnail
        // or assume that encrypted/alternate URL bytes use the photo .dat codec.
        Ok(Self {
            md5: md5.to_ascii_lowercase(),
            bytes,
            url: node
                .attribute("cdnurl")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            expires_at: expiry_timestamp(content, created_at),
        })
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn source_url(value: &str) -> Result<Url, Failure> {
    let invalid = || Failure::terminal("sticker_source_not_allowed");
    if value.len() > 8192 {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    // Extend this list only with separately verified original sticker sources.
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != Some("snsvideo.c2c.wechat.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url
            .port()
            .is_some_and(|port| port != if url.scheme() == "https" { 443 } else { 80 })
    {
        return Err(invalid());
    }
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 192 && b == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let words = ip.segments();
            // Only global unicast, excluding documentation and special-purpose
            // 2001::/23 addresses (including Teredo/private IPv4 embeddings).
            (words[0] & 0xe000) == 0x2000
                && !(words[0] == 0x2001 && (words[1] < 0x200 || words[1] == 0xdb8))
                && !(words[0] == 0x2002) // 6to4 can embed non-public IPv4.
                && !(words[0] == 0x3fff && words[1] < 0x1000)
        }
    }
}

#[async_trait]
trait Fetcher: Send + Sync {
    async fn fetch(&self, url: Url) -> Result<Vec<u8>, Failure>;
}

struct CdnFetcher;

#[async_trait]
impl Fetcher for CdnFetcher {
    async fn fetch(&self, url: Url) -> Result<Vec<u8>, Failure> {
        let host = url.host_str().unwrap();
        let addresses: Vec<SocketAddr> =
            tokio::net::lookup_host((host, url.port_or_known_default().unwrap()))
                .await
                .map_err(|_| Failure::transient("sticker_dns_failed"))?
                .collect();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(Failure::terminal("sticker_source_not_allowed"));
        }
        // Pin checked DNS answers, prohibit redirects, and avoid ambient HTTP
        // proxies changing the destination. Container transparent PROXY routing
        // still applies. Never log reqwest errors: they contain signed URLs.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| Failure::transient("sticker_fetch_failed"))?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|_| Failure::transient("sticker_fetch_failed"))?;
        read_response(response).await
    }
}

async fn read_response(response: reqwest::Response) -> Result<Vec<u8>, Failure> {
    let status = response.status();
    if !status.is_success() {
        return Err(if status.is_server_error() || status.as_u16() == 429 {
            Failure::transient("sticker_cdn_retryable_error")
        } else {
            // A 403/404 is not proof of message/CDN expiry.
            Failure::terminal("sticker_cdn_unavailable")
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(Failure::terminal("sticker_too_large"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Failure::transient("sticker_fetch_failed"))?;
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err(Failure::terminal("sticker_too_large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Clone)]
struct Validated {
    bytes: Arc<Vec<u8>>,
    format: &'static str,
}

fn validate(bytes: Vec<u8>, sticker: &Sticker) -> Result<Validated, Failure> {
    let invalid = || Failure::terminal("sticker_integrity_failed");
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    if sticker.bytes.is_some_and(|n| n != bytes.len())
        || format!("{:x}", Md5::digest(&bytes)) != sticker.md5
    {
        return Err(invalid());
    }
    let format = image::guess_format(&bytes).map_err(|_| invalid())?;
    let label = match format {
        ImageFormat::Gif => "gif",
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::WebP => "webp",
        _ => return Err(Failure::terminal("sticker_format_unsupported")),
    };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    // PNG needs limits before constructing its decoder, including metadata
    // decompression. Applying only ImageDecoder::set_limits is too late.
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|_| invalid())?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_PIXELS {
        return Err(Failure::terminal("sticker_decode_limit"));
    }
    decoder.set_limits(limits.clone()).map_err(|_| invalid())?;
    // Decode all frames from seekable input; preserving just the first frame
    // would hide truncation and destroy the animation delivered to callers.
    if format == ImageFormat::Gif {
        drop(decoder);
        if !bytes.ends_with(b";") {
            return Err(invalid());
        }
        let mut gif =
            image::codecs::gif::GifDecoder::new(Cursor::new(&bytes)).map_err(|_| invalid())?;
        gif.set_limits(limits).map_err(|_| invalid())?;
        validate_frames(gif.into_frames(), pixels)?;
    } else if format == ImageFormat::WebP {
        drop(decoder);
        let mut webp =
            image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes)).map_err(|_| invalid())?;
        webp.set_limits(limits).map_err(|_| invalid())?;
        if webp.has_animation() {
            validate_frames(webp.into_frames(), pixels)?;
        } else {
            image::DynamicImage::from_decoder(webp).map_err(|_| invalid())?;
        }
    } else {
        image::DynamicImage::from_decoder(decoder).map_err(|_| invalid())?;
    }
    Ok(Validated {
        bytes: Arc::new(bytes),
        format: label,
    })
}

fn validate_frames(frames: image::Frames<'_>, pixels: u64) -> Result<(), Failure> {
    let mut count = 0;
    for frame in frames {
        count += 1;
        if count > MAX_FRAMES || count as u64 * pixels > MAX_DECODED_PIXELS {
            return Err(Failure::terminal("sticker_decode_limit"));
        }
        frame.map_err(|_| Failure::terminal("sticker_integrity_failed"))?;
    }
    if count == 0 {
        return Err(Failure::terminal("sticker_integrity_failed"));
    }
    Ok(())
}

fn account_hash(account: &str) -> String {
    format!("{:x}", Sha256::digest(account.as_bytes()))
}

fn cached(root: &Path, account: &str, sticker: &Sticker) -> Option<Validated> {
    let path = root
        .join(account_hash(account))
        .join(format!("{}.blob", sticker.md5));
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return None;
    }
    let value = validate(std::fs::read(&path).ok()?, sticker).ok()?;
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()));
    }
    Some(value)
}

fn cache_write(
    root: &Path,
    account: &str,
    sticker: &Sticker,
    data: &[u8],
    budget: u64,
) -> std::io::Result<()> {
    let dir = root.join(account_hash(account));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let temporary = dir.join(format!("{}.part", uuid::Uuid::new_v4()));
    let target = dir.join(format!("{}.blob", sticker.md5));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    let mut files = Vec::new();
    for item in std::fs::read_dir(&dir)?.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        if !name.strip_suffix(".blob").is_some_and(valid_hash) {
            continue;
        }
        let metadata = item.path().symlink_metadata()?;
        if metadata.is_file() {
            files.push((metadata.modified()?, metadata.len(), item.path()));
        }
    }
    files.sort_by_key(|item| item.0);
    let mut total: u64 = files.iter().map(|item| item.1).sum();
    for (_, size, path) in files {
        if total <= budget {
            break;
        }
        if path != target && std::fs::remove_file(path).is_ok() {
            total -= size;
        }
    }
    Ok(())
}

type Outcome = Result<Validated, Failure>;
struct Job {
    started: Instant,
    receiver: watch::Receiver<Option<Outcome>>,
}
struct Service {
    root: PathBuf,
    fetcher: Arc<dyn Fetcher>,
    slots: Arc<Semaphore>,
    jobs: Mutex<HashMap<String, Job>>,
    timeout: Duration,
}

impl Service {
    fn new(root: PathBuf, fetcher: Arc<dyn Fetcher>) -> Arc<Self> {
        Arc::new(Self {
            root,
            fetcher,
            slots: Arc::new(Semaphore::new(4)),
            jobs: Mutex::new(HashMap::new()),
            timeout: JOB_TIMEOUT,
        })
    }

    async fn get(self: &Arc<Self>, account: &str, sticker: Sticker) -> MediaResult {
        tokio::time::timeout(self.timeout, self.get_inner(account, sticker))
            .await
            .unwrap_or_else(|_| Failure::transient("sticker_download_timeout").result())
    }

    async fn get_inner(self: &Arc<Self>, account: &str, sticker: Sticker) -> MediaResult {
        let service = self.clone();
        let a = account.to_owned();
        let s = sticker.clone();
        let permit = match self.slots.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return Failure::transient("sticker_queue_full").result(),
        };
        if let Ok(Some(value)) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            cached(&service.root, &a, &s)
        })
        .await
        {
            return ready(&sticker, value);
        }
        let mut result = MediaResult {
            media_type: "emoji".into(),
            filename: format!("emoji_{}", sticker.md5),
            ..Default::default()
        };
        if finish_expired(
            &mut result,
            sticker.expires_at,
            chrono::Utc::now().timestamp(),
        ) {
            return result;
        }
        let url = match sticker.url.as_deref().map(source_url) {
            Some(Ok(url)) => url,
            Some(Err(error)) => return error.result(),
            None => return Failure::terminal("sticker_source_unavailable").result(),
        };
        let key = format!("{}:{}", account_hash(account), sticker.md5);
        let mut receiver = {
            let mut jobs = self.jobs.lock().await;
            jobs.retain(|_, job| match job.receiver.borrow().as_ref() {
                None => job.receiver.has_changed().is_ok(),
                Some(Ok(_)) => false,
                Some(Err(_)) => job.started.elapsed() < FAILURE_TTL,
            });
            if let Some(job) = jobs.get(&key) {
                job.receiver.clone()
            } else {
                if jobs.len() >= 128 {
                    return Failure::transient("sticker_queue_full").result();
                }
                let (sender, receiver) = watch::channel(None);
                jobs.insert(
                    key.clone(),
                    Job {
                        started: Instant::now(),
                        receiver: receiver.clone(),
                    },
                );
                let service = self.clone();
                let account = account.to_owned();
                let sticker = sticker.clone();
                let own_receiver = receiver.clone();
                // The server owns the job: disconnecting an HTTP caller must not
                // cancel a validated transfer or cause duplicate submissions.
                tokio::spawn(async move {
                    let outcome = tokio::time::timeout(
                        service.timeout,
                        service.download(&account, &sticker, url),
                    )
                    .await
                    .unwrap_or_else(|_| Err(Failure::transient("sticker_download_timeout")));
                    let succeeded = outcome.is_ok();
                    let _ = sender.send(Some(outcome));
                    // Subscribers retain their outcome until their HTTP reply.
                    // Never retain successful payloads indefinitely in the job map.
                    if succeeded {
                        let mut jobs = service.jobs.lock().await;
                        if jobs
                            .get(&key)
                            .is_some_and(|job| job.receiver.same_channel(&own_receiver))
                        {
                            jobs.remove(&key);
                        }
                    }
                });
                receiver
            }
        };
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return match outcome {
                    Ok(value) => ready(&sticker, value),
                    Err(error) => error.result(),
                };
            }
            if receiver.changed().await.is_err() {
                return Failure::transient("sticker_fetch_failed").result();
            }
        }
    }

    async fn download(&self, account: &str, sticker: &Sticker, url: Url) -> Outcome {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Failure::transient("sticker_queue_full"))?;
        let root = self.root.clone();
        let a = account.to_owned();
        let s = sticker.clone();
        // Keep the concurrency permit in the blocking task even if the job's
        // deadline expires while cache decoding is still running.
        let (value, permit) = tokio::task::spawn_blocking(move || (cached(&root, &a, &s), permit))
            .await
            .map_err(|_| Failure::transient("sticker_fetch_failed"))?;
        if let Some(value) = value {
            return Ok(value);
        }
        let bytes = self.fetcher.fetch(url).await?;
        let root = self.root.clone();
        let account = account.to_owned();
        let sticker = sticker.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let value = validate(bytes, &sticker)?;
            if cache_write(&root, &account, &sticker, &value.bytes, CACHE_BUDGET).is_err() {
                tracing::warn!("[stickers] Validated bytes could not be cached");
            }
            Ok(value)
        })
        .await
        .map_err(|_| Failure::transient("sticker_fetch_failed"))?
    }
}

fn ready(sticker: &Sticker, value: Validated) -> MediaResult {
    if sticker.bytes.is_some_and(|n| n != value.bytes.len()) {
        return Failure::terminal("sticker_integrity_failed").result();
    }
    MediaResult {
        media_type: "emoji".into(),
        data: Some(STANDARD.encode(&*value.bytes)),
        format: value.format.into(),
        filename: format!("emoji_{}.{}", sticker.md5, value.format),
        ..Default::default()
    }
}

pub async fn retrieve(account: &str, sticker: Sticker) -> MediaResult {
    static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
    let service = SERVICE.get_or_init(|| {
        let db = PathBuf::from(
            std::env::var("AGENT_DB_PATH").unwrap_or_else(|_| "/data/agent.db".into()),
        );
        Service::new(
            db.parent()
                .unwrap_or(Path::new("/data"))
                .join("media-cache/stickers"),
            Arc::new(CdnFetcher),
        )
    });
    service.get(account, sticker).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, RgbaImage};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn animated() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
                encoder
                    .encode_frame(Frame::from_parts(
                        RgbaImage::from_pixel(2, 2, image::Rgba(color)),
                        0,
                        0,
                        Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        bytes
    }

    fn sticker(bytes: &[u8]) -> Sticker {
        Sticker {
            md5: format!("{:x}", Md5::digest(bytes)),
            bytes: Some(bytes.len()),
            url: Some("http://snsvideo.c2c.wechat.com/resource?signature=private".into()),
            expires_at: None,
        }
    }

    struct FakeFetcher {
        calls: AtomicUsize,
        outcome: Result<Vec<u8>, Failure>,
        delay: Duration,
    }
    #[async_trait]
    impl Fetcher for FakeFetcher {
        async fn fetch(&self, _: Url) -> Result<Vec<u8>, Failure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.outcome.clone()
        }
    }
    fn fake(outcome: Result<Vec<u8>, Failure>) -> Arc<FakeFetcher> {
        Arc::new(FakeFetcher {
            calls: AtomicUsize::new(0),
            outcome,
            delay: Duration::from_millis(20),
        })
    }

    #[test]
    fn metadata_parsing_is_exact_and_decodes_xml_entities() {
        let h = "0123456789abcdef0123456789abcdef";
        let xml = format!("<msg><emoji md5='{h}' len='12' cdnurl='http://snsvideo.c2c.wechat.com/x?a=1&amp;b=2'/></msg>");
        let s = Sticker::parse(&xml, 1).unwrap();
        assert_eq!(s.bytes, Some(12));
        assert!(s.url.unwrap().ends_with("?a=1&b=2"));
        for xml in [
            "<msg><emoji md5='../etc/passwd'/></msg>".into(),
            format!("<msg><emoji md5='{h}'/><emoji md5='{h}'/></msg>"),
            format!("<msg><quote><emoji md5='{h}'/></quote></msg>"),
            format!("<msg xmlns='evil'><emoji md5='{h}'/></msg>"),
            format!("<msg><emoji md5='{h}' len='oops'/></msg>"),
            format!("<!DOCTYPE msg [<!ENTITY md '{h}'>]><msg><emoji md5='&md;'/></msg>"),
        ] {
            assert!(Sticker::parse(&xml, 1).is_err());
        }
        let xml = format!("<msg><emoji md5='{h}' len='{}'/></msg>", MAX_BYTES + 1);
        assert_eq!(
            Sticker::parse(&xml, 1).err().unwrap().reason,
            "sticker_too_large"
        );
    }

    #[test]
    fn prevents_untrusted_urls_and_non_public_dns_answers() {
        for url in [
            "file:///etc/passwd",
            "http://127.0.0.1/x",
            "http://snsvideo.c2c.wechat.com.evil/x",
            "http://user:password@snsvideo.c2c.wechat.com/x",
            "http://snsvideo.c2c.wechat.com:6174/x",
            "http://snsvideo.c2c.wechat.com/x#fragment",
        ] {
            assert!(source_url(url).is_err());
        }
        assert!(source_url("http://snsvideo.c2c.wechat.com/x?a=1&b=2").is_ok());
        assert!(source_url("https://snsvideo.c2c.wechat.com/x").is_ok());
        for ip in [
            "0.0.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["43.175.129.36", "240d:c010:12b:1::2d"] {
            assert!(public_ip(ip.parse().unwrap()));
        }
    }

    #[test]
    fn animation_is_decoded_but_original_bytes_are_preserved() {
        let bytes = animated();
        let s = sticker(&bytes);
        let v = validate(bytes.clone(), &s).unwrap();
        assert_eq!(v.format, "gif");
        assert_eq!(*v.bytes, bytes);
        let frames = image::codecs::gif::GifDecoder::new(Cursor::new(&*v.bytes))
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_ne!(frames[0].buffer(), frames[1].buffer());
        let result = ready(&s, v);
        assert_eq!(result.media_type, "emoji");
        assert!(result.url.is_none());
        assert_eq!(STANDARD.decode(result.data.unwrap()).unwrap(), bytes);
    }

    #[test]
    fn validates_static_formats_and_rejects_bad_or_truncated_payloads() {
        for (format, label) in [
            (ImageFormat::Png, "png"),
            (ImageFormat::Jpeg, "jpeg"),
            (ImageFormat::WebP, "webp"),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(3, 2)
                .write_to(&mut bytes, format)
                .unwrap();
            let bytes = bytes.into_inner();
            assert_eq!(
                validate(bytes.clone(), &sticker(&bytes)).unwrap().format,
                label
            );
        }
        let bytes = animated();
        let mut bad = sticker(&bytes);
        bad.bytes = Some(bytes.len() + 1);
        assert!(validate(bytes.clone(), &bad).is_err());
        bad = sticker(&bytes);
        bad.md5 = "0".repeat(32);
        assert!(validate(bytes.clone(), &bad).is_err());
        let mut truncated = bytes;
        truncated.pop();
        assert!(validate(truncated.clone(), &sticker(&truncated)).is_err());
        let html = b"<html>CDN error</html>".to_vec();
        assert!(validate(html.clone(), &sticker(&html)).is_err());
        // Hash/length checks alone do not prove that an image decodes.
        let malformed = b"GIF89a\x01\0\x01\0\0\0\0;".to_vec();
        assert!(validate(malformed.clone(), &sticker(&malformed)).is_err());
    }

    #[test]
    fn image_dimensions_and_animation_work_are_bounded() {
        let mut huge = animated();
        huge[6..8].copy_from_slice(&2001u16.to_le_bytes());
        huge[8..10].copy_from_slice(&2000u16.to_le_bytes());
        assert_eq!(
            validate(huge.clone(), &sticker(&huge))
                .err()
                .unwrap()
                .reason,
            "sticker_decode_limit"
        );

        let frame = || {
            Ok(Frame::from_parts(
                RgbaImage::new(1, 1),
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            ))
        };
        let frames = image::Frames::new(Box::new((0..=MAX_FRAMES).map(|_| frame())));
        assert_eq!(
            validate_frames(frames, 1).err().unwrap().reason,
            "sticker_decode_limit"
        );
        let frames = image::Frames::new(Box::new((0..26).map(|_| frame())));
        assert_eq!(
            validate_frames(frames, MAX_PIXELS).err().unwrap().reason,
            "sticker_decode_limit"
        );
    }

    #[tokio::test]
    async fn concurrent_requests_download_once_and_cache_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let s = sticker(&bytes);
        let fetcher = fake(Ok(bytes.clone()));
        let service = Service::new(dir.path().into(), fetcher.clone());
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let service = service.clone();
            let s = s.clone();
            tasks.push(tokio::spawn(async move { service.get("account", s).await }));
        }
        for task in tasks {
            assert_eq!(
                STANDARD.decode(task.await.unwrap().data.unwrap()).unwrap(),
                bytes
            );
        }
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
        assert!(service.jobs.lock().await.is_empty());
        let unused = fake(Err(Failure::terminal("must_not_download")));
        let restarted = Service::new(dir.path().into(), unused.clone());
        assert!(restarted.get("account", s.clone()).await.data.is_some());
        assert_eq!(unused.calls.load(Ordering::SeqCst), 0);
        // Another account cannot consume this account's cache.
        assert!(service.get("other", s.clone()).await.data.is_some());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
        let path = dir
            .path()
            .join(account_hash("account"))
            .join(format!("{}.blob", s.md5));
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn cache_precedes_expiry_or_missing_url_and_corruption_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let mut s = sticker(&bytes);
        cache_write(dir.path(), "account", &s, &bytes, CACHE_BUDGET).unwrap();
        let fetcher = fake(Ok(bytes.clone()));
        let service = Service::new(dir.path().into(), fetcher.clone());
        s.url = None;
        s.expires_at = Some(1);
        assert!(service.get("account", s.clone()).await.data.is_some());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
        let absent = service.get("other", s.clone()).await;
        assert_eq!(absent.media_type, "expired");
        assert_eq!(absent.retryable, Some(false));
        s = sticker(&bytes);
        let path = dir
            .path()
            .join(account_hash("account"))
            .join(format!("{}.blob", s.md5));
        std::fs::write(&path, b"truncated").unwrap();
        assert!(service.get("account", s).await.data.is_some());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelled_http_waiter_does_not_cancel_the_server_job() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let s = sticker(&bytes);
        let fetcher = fake(Ok(bytes));
        let service = Service::new(dir.path().into(), fetcher.clone());
        let worker = service.clone();
        let input = s.clone();
        let request = tokio::spawn(async move { worker.get("account", input).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while fetcher.calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        request.abort();
        assert!(service.get("account", s).await.data.is_some());
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_are_bounded_coalesced_and_never_invent_expiry() {
        for error in [
            Failure::terminal("sticker_cdn_unavailable"),
            Failure::transient("sticker_fetch_failed"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let s = sticker(&animated());
            let fetcher = fake(Err(error.clone()));
            let service = Service::new(dir.path().into(), fetcher.clone());
            for _ in 0..2 {
                let result = service.get("account", s.clone()).await;
                assert_eq!(result.retryable, Some(error.retryable));
                assert_eq!(result.reason.as_deref(), Some(error.reason));
                assert_ne!(result.media_type, "expired");
                assert!(result.data.is_none());
                assert!(!serde_json::to_string(&result)
                    .unwrap()
                    .contains("signature"));
            }
            assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn deadline_and_bad_sources_return_explicit_status_without_cache_writes() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let fetcher = Arc::new(FakeFetcher {
            calls: AtomicUsize::new(0),
            outcome: Ok(bytes.clone()),
            delay: Duration::from_secs(1),
        });
        let mut service = Service::new(dir.path().into(), fetcher.clone());
        Arc::get_mut(&mut service).unwrap().timeout = Duration::from_millis(50);
        let result = service.get("account", sticker(&bytes)).await;
        assert_eq!(result.media_type, "pending");
        assert_eq!(result.reason.as_deref(), Some("sticker_download_timeout"));
        assert_eq!(result.retryable, Some(true));
        assert!(cached(dir.path(), "account", &sticker(&bytes)).is_none());
        let calls = fetcher.calls.load(Ordering::SeqCst);
        let mut s = sticker(&bytes);
        s.url = Some("http://127.0.0.1:6174/private".into());
        assert_eq!(
            service.get("other", s.clone()).await.reason.as_deref(),
            Some("sticker_source_not_allowed")
        );
        s.url = None;
        assert_eq!(
            service.get("other", s.clone()).await.reason.as_deref(),
            Some("sticker_source_unavailable")
        );
        s.expires_at = Some(1);
        assert_eq!(service.get("other", s).await.media_type, "expired");
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), calls);
    }

    #[tokio::test]
    async fn queue_wait_counts_towards_the_request_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let fetcher = fake(Ok(bytes.clone()));
        let mut service = Service::new(dir.path().into(), fetcher.clone());
        Arc::get_mut(&mut service).unwrap().timeout = Duration::from_millis(50);
        let _busy = service.slots.clone().acquire_many_owned(4).await.unwrap();
        let started = Instant::now();
        let result = service.get("account", sticker(&bytes)).await;
        assert_eq!(result.media_type, "pending");
        assert_eq!(result.retryable, Some(true));
        assert_eq!(result.reason.as_deref(), Some("sticker_download_timeout"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    }

    async fn response(raw: Vec<u8>) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(&raw).await;
        });
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(format!("http://{address}/sticker"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn http_ignores_wrong_mime_prohibits_redirects_and_does_not_infer_expiry() {
        let bytes = animated();
        let mut raw = format!("HTTP/1.1 200 OK\r\nContent-Type: image/jpg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).into_bytes();
        raw.extend_from_slice(&bytes);
        let downloaded = read_response(response(raw).await).await.unwrap();
        assert_eq!(
            validate(downloaded, &sticker(&bytes)).unwrap().format,
            "gif"
        );
        for (code, retryable) in [
            (302, false),
            (403, false),
            (404, false),
            (429, true),
            (500, true),
        ] {
            let raw = format!("HTTP/1.1 {code} Status\r\nContent-Length: 0\r\nLocation: http://127.0.0.1/private\r\nConnection: close\r\n\r\n").into_bytes();
            let error = read_response(response(raw).await).await.err().unwrap();
            assert_eq!(error.retryable, retryable);
            assert_ne!(error.result().media_type, "expired");
        }
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_BYTES + 1
        )
        .into_bytes();
        assert_eq!(
            read_response(response(raw).await)
                .await
                .err()
                .unwrap()
                .reason,
            "sticker_too_large"
        );
        // Missing Content-Length must not permit an unbounded streamed body.
        let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        raw.resize(raw.len() + MAX_BYTES + 1, 0);
        assert_eq!(
            read_response(response(raw).await)
                .await
                .err()
                .unwrap()
                .reason,
            "sticker_too_large"
        );
    }

    #[test]
    fn cache_quota_evicts_only_owned_blobs_and_leaves_no_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = animated();
        let s = sticker(&bytes);
        cache_write(dir.path(), "account", &s, &bytes, CACHE_BUDGET).unwrap();
        let account = dir.path().join(account_hash("account"));
        let old = account.join(format!("{}.blob", "1".repeat(32)));
        std::fs::write(&old, &bytes).unwrap();
        let untouched = account.join("keep.txt");
        std::fs::write(&untouched, b"unrelated").unwrap();
        cache_write(dir.path(), "account", &s, &bytes, bytes.len() as u64).unwrap();
        assert!(!old.exists());
        assert!(untouched.exists());
        assert!(std::fs::read_dir(account).unwrap().all(|f| !f
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
    }
}
