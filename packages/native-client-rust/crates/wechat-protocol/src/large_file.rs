use super::*;
use std::sync::atomic::{AtomicU32, AtomicU64};

const MAX_UPLOAD_TOKEN_BYTES: usize = 65536;
const BLOCKS_PER_GROUP: usize = 128;
const MAX_BLOCKS: usize = (MAX_LARGE_FILE_BYTES + 16).div_ceil(512 * 1024);

// The reference sends the current 128-block group and at most its two preceding
// groups. This uploader is sequential: only blocks before `next_block` have a
// verified acknowledgement. Offsets and alreadyupsize remain file-relative.
fn acknowledged_bitmap(next_block: usize) -> Vec<u8> {
    let group = next_block / BLOCKS_PER_GROUP;
    let first_group = group.saturating_sub(2);
    let mut bitmap = vec![0; (group - first_group + 1) * 16];
    for bit in 0..next_block - first_group * BLOCKS_PER_GROUP {
        bitmap[bit / 8] |= 1 << (bit % 8);
    }
    bitmap
}

// Legacy blocking large-file upload methods kept for 3.x native compatibility (#701).
// Prefer `upload_large_file*_async` and `upload_video_original*_async` for new code.
#[cfg(not(target_arch = "wasm32"))]
impl CdnClient {
    /// Upload one ordinary file using previously persisted source/key material.
    /// Object readback and business-message submission remain separate operations.
    #[deprecated(
        since = "3.2.0",
        note = "use `CdnClient::upload_large_file_with_progress_async`; blocking APIs are removed in 4.0 (#701)"
    )]
    pub fn upload_large_file_with_progress(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        ensure!(
            matches!(prepared.kind, MediaQuality::File | MediaQuality::LargeFile),
            "Expected an ordinary file upload"
        );
        let mut session = self.session();
        upload_large_file(
            &self.config,
            UploadRef::Memory(prepared),
            task_id,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            progress,
        )
    }

    /// [`Self::upload_large_file_with_progress`] for an upload read from a
    /// file in steps. The requests are the same; the block that ends the
    /// object is sent only after the whole file matched its digests.
    #[deprecated(
        since = "3.2.0",
        note = "use `CdnClient::upload_large_file_from_source_with_progress_async`; blocking APIs are removed in 4.0 (#701)"
    )]
    pub fn upload_large_file_from_file_with_progress(
        &self,
        prepared: &PreparedFileUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        ensure!(
            matches!(prepared.kind, MediaQuality::File | MediaQuality::LargeFile),
            "Expected an ordinary file upload"
        );
        let mut session = self.session();
        upload_large_file(
            &self.config,
            UploadRef::File(prepared),
            task_id,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            progress,
        )
    }

    /// Upload the unmodified original as a separate type-4 object. The normal
    /// rendition and thumbnail use their own paired upload and final metadata.
    /// Persist the task ID and prepared material before calling this method.
    #[deprecated(
        since = "3.2.0",
        note = "use `CdnClient::upload_video_original_with_progress_async`; blocking APIs are removed in 4.0 (#701)"
    )]
    pub fn upload_video_original_with_progress(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        let mut session = self.session();
        upload_video_original(
            self,
            UploadRef::Memory(prepared),
            task_id,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            progress,
        )
    }

    /// [`Self::upload_video_original_with_progress`] for an original read from
    /// a file in steps.
    #[deprecated(
        since = "3.2.0",
        note = "use `CdnClient::upload_video_original_from_source_with_progress_async`; blocking APIs are removed in 4.0 (#701)"
    )]
    pub fn upload_video_original_from_file_with_progress(
        &self,
        prepared: &PreparedFileUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        let mut session = self.session();
        upload_video_original(
            self,
            UploadRef::File(prepared),
            task_id,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            progress,
        )
    }
}

impl CdnClient {
    /// Asynchronously upload one large ordinary file using previously persisted
    /// source/key material and a pluggable [`HttpTransport`] and [`Clock`].
    pub async fn upload_large_file_with_progress_async(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        http: &dyn HttpTransport,
        clock: &dyn Clock,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        ensure!(
            matches!(prepared.kind, MediaQuality::File | MediaQuality::LargeFile),
            "Expected an ordinary file upload"
        );
        let mut session = self.async_session(http, clock);
        upload_large_file_async(
            &self.config,
            UploadRef::Memory(prepared),
            task_id,
            &mut session,
            progress,
        )
        .await
    }

    /// Asynchronously upload one large ordinary file streamed from a
    /// [`PreparedAsyncUpload`] without buffering the full ciphertext in memory.
    pub async fn upload_large_file_from_source_with_progress_async(
        &self,
        prepared: &PreparedAsyncUpload,
        task_id: u64,
        http: &dyn HttpTransport,
        clock: &dyn Clock,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        ensure!(
            matches!(prepared.kind, MediaQuality::File | MediaQuality::LargeFile),
            "Expected an ordinary file upload"
        );
        let mut session = self.async_session(http, clock);
        upload_large_file_async(
            &self.config,
            UploadRef::Async(prepared),
            task_id,
            &mut session,
            progress,
        )
        .await
    }

    /// Asynchronously upload an unmodified original video as a separate type-4
    /// object using a pluggable [`HttpTransport`] and [`Clock`].
    pub async fn upload_video_original_with_progress_async(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        http: &dyn HttpTransport,
        clock: &dyn Clock,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        let mut session = self.async_session(http, clock);
        upload_video_original_async(
            self,
            UploadRef::Memory(prepared),
            task_id,
            &mut session,
            progress,
        )
        .await
    }

    /// Asynchronously upload an unmodified original video streamed from a
    /// [`PreparedAsyncUpload`].
    pub async fn upload_video_original_from_source_with_progress_async(
        &self,
        prepared: &PreparedAsyncUpload,
        task_id: u64,
        http: &dyn HttpTransport,
        clock: &dyn Clock,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        let mut session = self.async_session(http, clock);
        upload_video_original_async(
            self,
            UploadRef::Async(prepared),
            task_id,
            &mut session,
            progress,
        )
        .await
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn upload_video_original(
    client: &CdnClient,
    prepared: UploadRef<'_>,
    task_id: u64,
    mut post: impl FnMut(&str, &CdnFields, &mut dyn FnMut() -> Result<()>, bool) -> CountedPost,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    ensure!(
        prepared.kind() == MediaQuality::Video && task_id != 0,
        "Expected an original video upload and task ID"
    );
    prepared.validate()?;
    if prepared.encrypted_len() < MAX_CDN_TRANSFER_BYTES {
        client.upload_with_transport(prepared, progress, |path, fields, fence| {
            post(path, fields, fence, false)
                .map(|(reply, _)| reply)
                .map_err(|failure| failure.error)
        })
    } else {
        upload_large_file(&client.config, prepared, task_id, post, progress)
    }
}

async fn upload_video_original_async(
    client: &CdnClient,
    prepared: UploadRef<'_>,
    task_id: u64,
    session: &mut AsyncCdnSession<'_>,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    ensure!(
        prepared.kind() == MediaQuality::Video && task_id != 0,
        "Expected an original video upload and task ID"
    );
    prepared.validate()?;
    if prepared.encrypted_len() < MAX_CDN_TRANSFER_BYTES {
        client
            .upload_with_transport_async(prepared, progress, session)
            .await
    } else {
        upload_large_file_async(&client.config, prepared, task_id, session, progress).await
    }
}

fn common_fields(
    conf: &CdnConfig,
    prepared: UploadRef<'_>,
    task_id: u64,
    sequence: u32,
) -> CdnFields {
    let mut fields = CdnFields::new();
    for (key, value) in [
        ("ver", conf.version as u64),
        ("seq", sequence as u64),
        ("weixinnum", conf.uin),
        ("nettype", 1),
        ("ipseq", 0),
        ("retrycnt", 0),
        ("lastretcode", 0),
        ("waitsec", 0),
        ("clientversion", conf.client_version as u64),
        ("taskid", task_id),
        ("from", 0),
        ("bizid", 1),
        ("appid", 0),
        ("filetype", u64::from(prepared.kind().filetype())),
        ("optype", 0),
        ("expectbizid", 0),
        ("expectappid", 0),
        ("expectfiletype", u64::from(prepared.kind().filetype())),
        ("expectoptype", 0),
        ("frontid", conf.front_id as u64),
    ] {
        put(&mut fields, key, value);
    }
    fields.insert("authkey".into(), conf.auth_key.clone());
    fields.insert(
        "clientostype".into(),
        conf.client_os_type.as_bytes().to_vec(),
    );
    fields.insert("filekey".into(), prepared.file_key().as_bytes().to_vec());
    if let Some(location) = conf.location {
        for (key, value) in [
            ("province", location.province),
            ("city", location.city),
            ("isp", location.isp),
        ] {
            put(&mut fields, key, value);
        }
    }
    fields
}

/// Server codes the official client retries when a reply carries no `isretry`:
/// its retryable list without the download and redirect codes.
const RETRYABLE_SERVER_CODES: [i32; 8] = [
    -5_103_179, // DataIDChanged
    -5_103_202, // SendDataOnInvalidConn
    -5_103_205, // InvalidOffset
    -5_103_211, // FileMd5NotEqual
    -5_103_215, // DataChecksumNotEqual
    -5_103_216, // AckRetransNoCompleteFile
    -5_103_217, // ResumeTooOldData
    -5_202_163,
];
/// Of those, the codes the official client retries at once.
const FAST_RETRY_CODES: [i32; 7] = [
    -5_103_179, -5_103_202, -5_103_205, -5_103_211, -5_103_215, -5_103_216, -5_103_217,
];
/// Server errors retried during one upload, as in the official client.
const MAX_SERVER_RETRIES: u32 = 8;
/// Waits, in seconds, before retry 1 to 8 of other codes when the reply gives no
/// usable `retrysec`: the start of the official client's backoff table.
const SERVER_RETRY_BACKOFF_SECS: [u64; MAX_SERVER_RETRIES as usize] = [0, 1, 1, 1, 3, 3, 3, 6];
/// Most time one upload waits between server retries in total: the official
/// client's limit for a whole large-file transfer.
const MAX_SERVER_RETRY_WAIT: Duration = Duration::from_secs(900);

/// Whether and when to retry a large-file request the CDN rejected, following
/// the official client: `isretry` decides when present, otherwise the code does.
/// A `retrysec` below 900 sets the wait; otherwise some codes are retried at once
/// and the rest back off. `retry` counts the server retries already made.
fn server_retry_delay(reply: &CdnFields, code: i32, retry: u32) -> Option<Duration> {
    let retryable = match reply.get("isretry") {
        Some(_) => integer(reply, "isretry").ok()? != 0,
        None => RETRYABLE_SERVER_CODES.contains(&code),
    };
    if !retryable {
        return None;
    }
    if let Ok(seconds) = integer(reply, "retrysec") {
        if (0..900).contains(&seconds) {
            return Some(Duration::from_secs(seconds as u64));
        }
    }
    if FAST_RETRY_CODES.contains(&code) {
        return Some(Duration::ZERO);
    }
    let index = (retry as usize).min(SERVER_RETRY_BACKOFF_SECS.len() - 1);
    Some(Duration::from_secs(SERVER_RETRY_BACKOFF_SECS[index]))
}

/// The server-error retries of one upload, bounded like the official client's.
/// A retry resends the rejected request with a new sequence number on the next
/// CDN address and is otherwise unchanged.
#[derive(Default)]
struct ServerRetries {
    count: AtomicU32,
    waited_ms: AtomicU64,
}

impl ServerRetries {
    /// The wait before retrying a request the CDN rejected, or `None` to stop.
    fn next(&self, reply: &CdnFields, error: &anyhow::Error) -> Option<Duration> {
        let code = ServiceRejected::find(error)?.code;
        let count = self.count();
        let delay = server_retry_delay(reply, code, count)?;
        let waited_ms = self.waited_ms.load(Ordering::Relaxed) + delay.as_millis() as u64;
        if count >= MAX_SERVER_RETRIES || waited_ms > MAX_SERVER_RETRY_WAIT.as_millis() as u64 {
            return None;
        }
        self.count.store(count + 1, Ordering::Relaxed);
        self.waited_ms.store(waited_ms, Ordering::Relaxed);
        Some(delay)
    }

    /// Server errors retried so far.
    fn count(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }
}

/// A large-file request's reply and the HTTP attempts it took, or why it failed.
#[cfg(not(target_arch = "wasm32"))]
type CountedPost = std::result::Result<(CdnFields, u32), CdnPostFailure>;

/// One request of a large-file upload, used to say where the upload stopped.
#[derive(Clone, Copy)]
struct Step<'r> {
    phase: &'static str,
    block: Option<usize>,
    retries: &'r ServerRetries,
}

impl<'r> Step<'r> {
    fn new(phase: &'static str, block: Option<usize>, retries: &'r ServerRetries) -> Self {
        Self {
            phase,
            block,
            retries,
        }
    }

    /// Attach where and why the upload stopped. An error that already says where
    /// it stopped keeps that.
    fn stopped(
        self,
        error: anyhow::Error,
        attempts: u32,
        cause: LargeFileUploadCause,
    ) -> anyhow::Error {
        if LargeFileUploadFailure::find(&error).is_some() {
            return error;
        }
        let cause = if ServiceRejected::find(&error).is_some() {
            LargeFileUploadCause::ServiceRejected
        } else {
            cause
        };
        error.context(LargeFileUploadFailure {
            phase: self.phase,
            block: self.block.and_then(|block| u32::try_from(block).ok()),
            attempts,
            server_retries: self.retries.count(),
            cause,
        })
    }

    /// Stopped outside a request: cancellation, logout, the source or progress.
    fn local(self, error: anyhow::Error) -> anyhow::Error {
        self.stopped(error, 0, LargeFileUploadCause::Local)
    }

    /// The session gave up on the request.
    fn post(self, failure: CdnPostFailure) -> anyhow::Error {
        self.stopped(failure.error, failure.attempts, failure.cause)
    }

    /// A reply arrived but was rejected or did not match the request.
    fn reply(self, error: anyhow::Error, attempts: u32) -> anyhow::Error {
        self.stopped(error, attempts, LargeFileUploadCause::ReplyInvalid)
    }
}

fn validate_reply(reply: &CdnFields, sequence: u32, version: u32) -> Result<()> {
    ensure!(
        integer(reply, "seq")? == i64::from(sequence),
        "Large-file reply sequence mismatch"
    );
    let code = integer(reply, "retcode")?;
    ServiceRejected::check_wide(Service::LargeFileUpload, code)?;
    // Like the official client, a successful reply's isretry, retrysec,
    // isoverload and isgetcdn are not acted on.
    ensure!(
        integer(reply, "ver")? == i64::from(version),
        "Large-file reply version mismatch"
    );
    Ok(())
}

fn opaque_field(reply: &CdnFields, key: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let value = reply
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("Missing large-file {key}"))?;
    ensure!(
        !value.is_empty() && value.len() <= max_bytes,
        "Invalid large-file {key} length"
    );
    Ok(value.clone())
}

fn validate_large_file_inputs(
    conf: &CdnConfig,
    prepared: UploadRef<'_>,
    task_id: u64,
) -> Result<u64> {
    ensure!(
        ((prepared.kind() == MediaQuality::File
            && prepared.plaintext_bytes() as usize > MAX_NATIVE_UPLOAD_BYTES
            && prepared.plaintext_bytes() as usize <= MAX_CDN_TRANSFER_BYTES)
            || (prepared.kind() == MediaQuality::LargeFile
                && prepared.plaintext_bytes() as usize > MAX_CDN_TRANSFER_BYTES
                && prepared.plaintext_bytes() as usize <= MAX_LARGE_FILE_BYTES)
            || (prepared.kind() == MediaQuality::Video
                && prepared.plaintext_bytes() as usize > MAX_NATIVE_UPLOAD_BYTES
                && prepared.plaintext_bytes() as usize <= wechat_media::MAX_VIDEO_SOURCE_BYTES))
            && prepared.encrypted_len() >= MAX_CDN_TRANSFER_BYTES
            && task_id != 0,
        "Unsupported large-file upload profile"
    );
    conf.validate()?;
    ensure!(
        conf.location.is_some(),
        "Missing authenticated CDN location"
    );
    prepared.validate()?;
    Ok(prepared.encrypted_len() as u64)
}

fn apply_fields(prepared: UploadRef<'_>, total: u64) -> Result<CdnFields> {
    let mut apply = CdnFields::new();
    for (key, value) in [
        ("apptype", 1),
        ("chatnum", 1),
        ("reqtype", 0),
        ("type", u64::from(prepared.kind().filetype())),
        ("prepare", 0),
        ("num", 1),
        ("type_0", u64::from(prepared.kind().filetype())),
        ("cpr_0", 0),
        ("size_0", total),
        ("rawsize_0", prepared.plaintext_bytes() as u64),
        ("format_0", 0),
    ] {
        put(&mut apply, key, value);
    }
    apply.insert(
        "chatid".into(),
        encrypt_recipient(prepared.destination())?.into_bytes(),
    );
    apply.insert("rawfilemd5_0".into(), prepared.md5().as_bytes().to_vec());
    if let Some((_, extension)) = prepared.file_name().rsplit_once('.') {
        if !extension.is_empty() {
            apply.insert("ext_0".into(), extension.as_bytes().to_vec());
        }
    }
    Ok(apply)
}

fn parse_apply_reply(reply: &CdnFields, prepared: UploadRef<'_>) -> Result<(usize, Vec<u8>)> {
    ensure!(
        integer(reply, "hit")? == 0,
        "Large-file cache hits require unsupported key recovery"
    );
    ensure!(
        integer(reply, "type")? == i64::from(prepared.kind().filetype())
            && integer(reply, "num")? == 1
            && integer(reply, "type_0")? == i64::from(prepared.kind().filetype())
            && integer(reply, "fin_0")? == 0,
        "Unsupported large-file apply result"
    );
    let block_size = usize::try_from(integer(reply, "blocksize_0")?)?;
    ensure!(
        [512 * 1024, 1024 * 1024].contains(&block_size)
            && prepared.encrypted_len().div_ceil(block_size) <= MAX_BLOCKS,
        "Unsupported large-file block size"
    );
    let upinfo = opaque_field(reply, "upinfo_0", MAX_UPLOAD_TOKEN_BYTES)?;
    Ok((block_size, upinfo))
}

fn upload_block_fields(acknowledged: u64, index: usize, upinfo: &[u8], chunk: &[u8]) -> CdnFields {
    let mut fields = CdnFields::new();
    put(&mut fields, "alreadyupsize", acknowledged);
    fields.insert("alreadyupbit".into(), acknowledged_bitmap(index));
    fields.insert("upinfo".into(), upinfo.to_vec());
    put(&mut fields, "offset", acknowledged);
    fields.insert("blockmd5".into(), hex::encode(md5(chunk)).into_bytes());
    fields.insert("blockdata".into(), chunk.to_vec());
    fields
}

fn parse_block_reply(reply: &CdnFields, acknowledged: u64) -> Result<Vec<u8>> {
    ensure!(
        u64::try_from(integer(reply, "offset")?)? == acknowledged && integer(reply, "fin")? == 0,
        "Large-file block acknowledgement mismatch"
    );
    opaque_field(reply, "etag", 8192)
}

fn finish_fields(upinfo: Vec<u8>, etags: Vec<Vec<u8>>) -> CdnFields {
    let mut finish = CdnFields::new();
    finish.insert("upinfo".into(), upinfo);
    put(&mut finish, "blocksum", etags.len());
    for (index, etag) in etags.into_iter().enumerate() {
        finish.insert(format!("etag_{index}"), etag);
    }
    finish
}

fn complete_fields(prepared: UploadRef<'_>, finished_upinfo: Vec<u8>) -> CdnFields {
    let mut complete = CdnFields::new();
    put(&mut complete, "type", prepared.kind().filetype());
    put(&mut complete, "num", 1);
    complete.insert("upinfo_0".into(), finished_upinfo);
    complete
}

fn parse_complete_reply(reply: &CdnFields) -> Result<String> {
    let file_id = String::from_utf8(opaque_field(reply, "fileid", 8192)?)?;
    ensure!(
        !file_id.chars().any(|c| c.is_control()),
        "Invalid large-file identifier"
    );
    Ok(file_id)
}

#[cfg(not(target_arch = "wasm32"))]
fn upload_large_file(
    conf: &CdnConfig,
    prepared: UploadRef<'_>,
    task_id: u64,
    mut post: impl FnMut(&str, &CdnFields, &mut dyn FnMut() -> Result<()>, bool) -> CountedPost,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    let total = validate_large_file_inputs(conf, prepared, task_id)?;
    let mut sequence = 0u32;
    let retries = ServerRetries::default();
    // The session retries transient HTTP/TCP failures and unreadable replies by
    // resending the same bytes, which the CDN answers with the same reply, even
    // for a block it already accepted; changing `retrycnt` on such a resend is
    // rejected (-5103015). A rejected reply is retried only as `ServerRetries`
    // allows. A failed upload leaves the caller's persisted source/key material
    // intact and never submits a business message.
    let mut exchange =
        |step: Step,
         extra: CdnFields,
         acknowledged: u64,
         progress: &mut dyn FnMut(u64, Option<u64>) -> Result<()>| {
            progress(acknowledged, Some(total)).map_err(|error| step.local(error))?;
            let mut fields = common_fields(conf, prepared, task_id, sequence + 1);
            fields.extend(extra);
            let mut switch_endpoint = false;
            loop {
                sequence += 1;
                put(&mut fields, "seq", sequence);
                let path = format!(
                    "/cdn/{}/{sequence}/{}/{}",
                    conf.uin,
                    prepared.kind().filetype(),
                    step.phase
                );
                let mut fence = || progress(acknowledged, Some(total));
                let (reply, attempts) = post(&path, &fields, &mut fence, switch_endpoint)
                    .map_err(|failure| step.post(failure))?;
                let Err(error) = validate_reply(&reply, sequence, conf.version) else {
                    return Ok::<_, anyhow::Error>((reply, attempts));
                };
                let Some(delay) = retries.next(&reply, &error) else {
                    return Err(step.reply(error, attempts));
                };
                sleep_with_fence(delay, CDN_RETRY_SLEEP_SLICE, &mut fence)
                    .map_err(|error| step.local(error))?;
                switch_endpoint = true;
            }
        };

    let step = Step::new("apply", None, &retries);
    let apply = apply_fields(prepared, total)?;
    let (reply, attempts) = exchange(step, apply, 0, &mut progress)?;
    let (block_size, upinfo) =
        parse_apply_reply(&reply, prepared).map_err(|error| step.reply(error, attempts))?;
    progress(0, Some(total)).map_err(|error| step.local(error))?;

    let mut acknowledged = 0u64;
    let mut etags = Vec::new();
    // Each block is produced just before it is sent. For a file, the block
    // that ends the object is produced only once the whole source matched its
    // prepared digests, so a changed source never reaches `fin`.
    let mut chunks = prepared.chunks();
    let mut index = 0usize;
    while (chunks.position() as u64) < total {
        let step = Step::new("uploadblock", Some(index), &retries);
        let chunk = chunks.next(block_size).map_err(|error| step.local(error))?;
        let fields = upload_block_fields(acknowledged, index, &upinfo, &chunk);
        let (reply, attempts) = exchange(step, fields, acknowledged, &mut progress)?;
        etags.push(
            parse_block_reply(&reply, acknowledged).map_err(|error| step.reply(error, attempts))?,
        );
        acknowledged += chunk.len() as u64;
        index += 1;
        progress(acknowledged, Some(total)).map_err(|error| step.local(error))?;
    }
    if acknowledged != total {
        let step = Step::new("uploadblock", Some(index), &retries);
        return Err(step.local(anyhow::anyhow!("Incomplete large-file upload")));
    }

    let step = Step::new("fin", None, &retries);
    let (reply, attempts) = exchange(
        step,
        finish_fields(upinfo, etags),
        acknowledged,
        &mut progress,
    )?;
    let finished_upinfo = opaque_field(&reply, "upinfo", MAX_UPLOAD_TOKEN_BYTES)
        .map_err(|error| step.reply(error, attempts))?;
    progress(acknowledged, Some(total)).map_err(|error| step.local(error))?;
    let step = Step::new("complete", None, &retries);
    let (reply, attempts) = exchange(
        step,
        complete_fields(prepared, finished_upinfo),
        acknowledged,
        &mut progress,
    )?;
    let file_id = parse_complete_reply(&reply).map_err(|error| step.reply(error, attempts))?;
    // Return the correlated completion even if cancellation arrived while this
    // request was in flight. The caller persists it before checking its fence.
    Ok(prepared.uploaded(file_id, None, None))
}

struct LargeFileExchange<'a, 's, 't> {
    conf: &'a CdnConfig,
    prepared: UploadRef<'a>,
    task_id: u64,
    total: u64,
    sequence: u32,
    retries: &'a ServerRetries,
    session: &'s mut AsyncCdnSession<'t>,
}

impl LargeFileExchange<'_, '_, '_> {
    async fn send(
        &mut self,
        step: Step<'_>,
        extra: CdnFields,
        acknowledged: u64,
        progress: &mut impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<(CdnFields, u32)> {
        progress(acknowledged, Some(self.total)).map_err(|error| step.local(error))?;
        let total = self.total;
        let mut fields = common_fields(self.conf, self.prepared, self.task_id, self.sequence + 1);
        fields.extend(extra);
        loop {
            self.sequence += 1;
            let seq = self.sequence;
            put(&mut fields, "seq", seq);
            let path = format!(
                "/cdn/{}/{seq}/{}/{}",
                self.conf.uin,
                self.prepared.kind().filetype(),
                step.phase
            );
            let (reply, attempts) = self
                .session
                .post_counted(&path, &fields, || progress(acknowledged, Some(total)))
                .await
                .map_err(|failure| step.post(failure))?;
            let Err(error) = validate_reply(&reply, seq, self.conf.version) else {
                return Ok((reply, attempts));
            };
            let Some(delay) = self.retries.next(&reply, &error) else {
                return Err(step.reply(error, attempts));
            };
            self.session
                .wait(delay, &mut || progress(acknowledged, Some(total)))
                .await
                .map_err(|error| step.local(error))?;
            self.session.switch_endpoint();
        }
    }
}

async fn upload_large_file_async(
    conf: &CdnConfig,
    prepared: UploadRef<'_>,
    task_id: u64,
    session: &mut AsyncCdnSession<'_>,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    let total = validate_large_file_inputs(conf, prepared, task_id)?;
    let retries = ServerRetries::default();
    let mut exchange = LargeFileExchange {
        conf,
        prepared,
        task_id,
        total,
        sequence: 0,
        retries: &retries,
        session,
    };

    let step = Step::new("apply", None, &retries);
    let apply = apply_fields(prepared, total)?;
    let (reply, attempts) = exchange.send(step, apply, 0, &mut progress).await?;
    let (block_size, upinfo) =
        parse_apply_reply(&reply, prepared).map_err(|error| step.reply(error, attempts))?;
    progress(0, Some(total)).map_err(|error| step.local(error))?;

    let mut acknowledged = 0u64;
    let mut etags = Vec::new();
    let mut chunks = prepared.chunks();
    let mut index = 0usize;
    while (chunks.position() as u64) < total {
        let step = Step::new("uploadblock", Some(index), &retries);
        let chunk = chunks
            .next_async(block_size)
            .await
            .map_err(|error| step.local(error))?;
        let fields = upload_block_fields(acknowledged, index, &upinfo, &chunk);
        let (reply, attempts) = exchange
            .send(step, fields, acknowledged, &mut progress)
            .await?;
        etags.push(
            parse_block_reply(&reply, acknowledged).map_err(|error| step.reply(error, attempts))?,
        );
        acknowledged += chunk.len() as u64;
        index += 1;
        progress(acknowledged, Some(total)).map_err(|error| step.local(error))?;
    }
    if acknowledged != total {
        let step = Step::new("uploadblock", Some(index), &retries);
        return Err(step.local(anyhow::anyhow!("Incomplete large-file upload")));
    }

    let step = Step::new("fin", None, &retries);
    let (reply, attempts) = exchange
        .send(
            step,
            finish_fields(upinfo, etags),
            acknowledged,
            &mut progress,
        )
        .await?;
    let finished_upinfo = opaque_field(&reply, "upinfo", MAX_UPLOAD_TOKEN_BYTES)
        .map_err(|error| step.reply(error, attempts))?;
    progress(acknowledged, Some(total)).map_err(|error| step.local(error))?;
    let step = Step::new("complete", None, &retries);
    let (reply, attempts) = exchange
        .send(
            step,
            complete_fields(prepared, finished_upinfo),
            acknowledged,
            &mut progress,
        )
        .await?;
    let file_id = parse_complete_reply(&reply).map_err(|error| step.reply(error, attempts))?;
    Ok(prepared.uploaded(file_id, None, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::OnceLock};

    // The fixtures below hold their uploads in memory.
    fn upload_large_file(
        conf: &CdnConfig,
        prepared: &PreparedUpload,
        task_id: u64,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        super::upload_large_file(
            conf,
            UploadRef::Memory(prepared),
            task_id,
            |path, fields, _, _| counted(post(path, fields)),
            progress,
        )
    }
    fn upload_video_original(
        client: &CdnClient,
        prepared: &PreparedUpload,
        task_id: u64,
        mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        super::upload_video_original(
            client,
            UploadRef::Memory(prepared),
            task_id,
            |path, fields, _, _| counted(post(path, fields)),
            progress,
        )
    }

    /// A fixture reply counted as one attempt; a fixture error as a transport failure.
    fn counted(reply: Result<CdnFields>) -> CountedPost {
        reply
            .map(|reply| (reply, 1))
            .map_err(|error| CdnPostFailure {
                error,
                attempts: 1,
                cause: LargeFileUploadCause::Transport,
            })
    }

    #[test]
    fn acknowledgement_windows_cross_multiple_groups_without_wrapping() {
        assert_eq!(acknowledged_bitmap(0), vec![0; 16]);
        let mut before_boundary = vec![255; 16];
        before_boundary[15] = 127;
        assert_eq!(acknowledged_bitmap(127), before_boundary);
        assert_eq!(
            acknowledged_bitmap(128),
            [vec![255; 16], vec![0; 16]].concat()
        );
        assert_eq!(
            acknowledged_bitmap(256),
            [vec![255; 32], vec![0; 16]].concat()
        );
        for index in [384, 512, 1024] {
            assert_eq!(
                acknowledged_bitmap(index),
                [vec![255; 32], vec![0; 16]].concat()
            );
        }
        let mut after_boundary = [vec![255; 32], vec![0; 16]].concat();
        after_boundary[32] = 1;
        assert_eq!(acknowledged_bitmap(513), after_boundary);
        assert_eq!(MAX_BLOCKS, 1025);
    }

    #[test]
    fn final_request_serializes_all_maximum_file_etags_in_order() {
        let mut fields = CdnFields::new();
        put(&mut fields, "blocksum", MAX_BLOCKS);
        fields.insert("upinfo".into(), b"synthetic-finish-context".to_vec());
        for index in 0..MAX_BLOCKS {
            fields.insert(
                format!("etag_{index}"),
                format!("synthetic-{index}").into_bytes(),
            );
        }
        let wire = pack_cdn(&fields).unwrap();
        let mut cursor = crate::mmtls::Cursor::new(&wire);
        let mut decoded = CdnFields::new();
        for _ in 0..MAX_BLOCKS + 2 {
            let key = std::str::from_utf8(cursor.sized(4).unwrap())
                .unwrap()
                .to_owned();
            decoded.insert(key, cursor.sized(4).unwrap().to_vec());
        }
        cursor.end().unwrap();
        assert_eq!(integer(&decoded, "blocksum").unwrap(), 1025);
        for index in 0..MAX_BLOCKS {
            assert_eq!(
                decoded[&format!("etag_{index}")],
                format!("synthetic-{index}").as_bytes()
            );
        }
        fields.extend((MAX_BLOCKS..2048).map(|i| (format!("etag_{i}"), vec![1])));
        assert!(pack_cdn(&fields).is_err());
    }

    fn config() -> CdnConfig {
        CdnConfig {
            version: 1,
            uin: 12345,
            client_version: 456,
            client_os_type: "test-client".into(),
            auth_key: b"test-credential".to_vec(),
            server_ips: vec![Ipv4Addr::new(1, 1, 1, 1)],
            front_id: 789,
            location: Some(CdnLocation {
                province: 1234,
                city: 56,
                isp: 78,
            }),
        }
    }

    fn prepared() -> &'static PreparedUpload {
        static VALUE: OnceLock<PreparedUpload> = OnceLock::new();
        VALUE.get_or_init(|| {
            PreparedUpload::with_material(
                &vec![0x5a; MAX_CDN_TRANSFER_BYTES + 17],
                MediaQuality::LargeFile,
                "boundary.fixture.bin",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap()
        })
    }

    // This peer accepts only the captured four-phase wire contract. It checks
    // offsets, encrypted block hashes, prior acknowledgements and both tokens.
    struct Peer<'a> {
        prepared: &'a PreparedUpload,
        block_size: usize,
        requests: usize,
        /// Requests answered by [`Peer::reject`]; each used a sequence number.
        rejected: usize,
    }

    impl<'a> Peer<'a> {
        fn new(prepared: &'a PreparedUpload, block_size: usize) -> Self {
            Self {
                prepared,
                block_size,
                requests: 0,
                rejected: 0,
            }
        }

        /// Answer the next request with a server error, as the CDN does: `ver` 0,
        /// `offset` 0 and the request's own sequence number.
        fn reject(&mut self, fields: &CdnFields, code: i64, isretry: Option<i64>) -> CdnFields {
            self.rejected += 1;
            let sequence = integer(fields, "seq").unwrap();
            assert_eq!(sequence as usize, self.requests + self.rejected);
            let mut reply = CdnFields::new();
            for (key, value) in [
                ("ver", 0),
                ("seq", sequence),
                ("retcode", code),
                ("retrysec", 0),
                ("isoverload", 0),
                ("isgetcdn", 0),
                ("offset", 0),
                ("fin", 0),
            ] {
                put(&mut reply, key, value);
            }
            if let Some(isretry) = isretry {
                put(&mut reply, "isretry", isretry);
            }
            reply
        }

        fn blocks(&self) -> usize {
            self.prepared.encrypted.len().div_ceil(self.block_size)
        }

        fn respond(&mut self, path: &str, fields: &CdnFields) -> Result<CdnFields> {
            self.requests += 1;
            let step = self.requests;
            let sequence = step + self.rejected;
            let kind = self.prepared.kind.filetype();
            let phase = match step {
                1 => "apply",
                n if n <= self.blocks() + 1 => "uploadblock",
                n if n == self.blocks() + 2 => "fin",
                n if n == self.blocks() + 3 => "complete",
                _ => panic!("Request after completion"),
            };
            assert_eq!(path, format!("/cdn/12345/{sequence}/{kind}/{phase}"));
            assert_eq!(integer(fields, "seq")?, sequence as i64);
            assert_eq!(integer(fields, "taskid")?, 987654321);
            assert_eq!(integer(fields, "filetype")?, i64::from(kind));
            assert_eq!(integer(fields, "expectfiletype")?, i64::from(kind));
            assert_eq!(integer(fields, "frontid")?, 789);
            for key in ["ipseq", "retrycnt", "lastretcode", "waitsec"] {
                assert_eq!(integer(fields, key)?, 0);
            }
            assert_eq!(integer(fields, "province")?, 1234);
            assert_eq!(integer(fields, "city")?, 56);
            assert_eq!(integer(fields, "isp")?, 78);
            assert_eq!(fields["authkey"], b"test-credential");
            assert_eq!(fields["filekey"], self.prepared.file_key.as_bytes());
            assert!(!fields.contains_key("aeskey"));
            assert!(!fields.contains_key("signature"));
            let mut reply = CdnFields::new();
            for (key, value) in [
                ("ver", 1),
                ("seq", sequence),
                ("retcode", 0),
                ("isretry", 0),
                ("retrysec", 0),
                ("isoverload", 0),
                ("isgetcdn", 0),
            ] {
                put(&mut reply, key, value);
            }
            match phase {
                "apply" => {
                    assert_eq!(integer(fields, "type")?, i64::from(kind));
                    assert_eq!(integer(fields, "type_0")?, i64::from(kind));
                    assert_eq!(integer(fields, "num")?, 1);
                    assert_eq!(integer(fields, "apptype")?, 1);
                    assert_eq!(integer(fields, "chatnum")?, 1);
                    assert_eq!(
                        integer(fields, "size_0")?,
                        self.prepared.encrypted.len() as i64
                    );
                    assert_eq!(
                        integer(fields, "rawsize_0")?,
                        i64::from(self.prepared.plaintext_bytes)
                    );
                    assert_eq!(fields["rawfilemd5_0"], self.prepared.md5.as_bytes());
                    assert_eq!(
                        fields["ext_0"],
                        self.prepared
                            .file_name
                            .rsplit('.')
                            .next()
                            .unwrap()
                            .as_bytes()
                    );
                    assert_eq!(
                        fields["chatid"],
                        encrypt_recipient("filehelper")?.as_bytes()
                    );
                    for (key, value) in [
                        ("hit", 0),
                        ("type", kind as usize),
                        ("num", 1),
                        ("type_0", kind as usize),
                        ("fin_0", 0),
                        ("blocksize_0", self.block_size),
                    ] {
                        put(&mut reply, key, value);
                    }
                    reply.insert("upinfo_0".into(), b"apply-token".to_vec());
                }
                "uploadblock" => {
                    let index = step - 2;
                    let offset = index * self.block_size;
                    let end = (offset + self.block_size).min(self.prepared.encrypted.len());
                    let expected = &self.prepared.encrypted[offset..end];
                    assert_eq!(integer(fields, "offset")?, offset as i64);
                    assert_eq!(integer(fields, "alreadyupsize")?, offset as i64);
                    assert_eq!(fields["upinfo"], b"apply-token");
                    assert_eq!(fields["blockdata"], expected);
                    assert_eq!(fields["blockmd5"], hex::encode(md5(expected)).as_bytes());
                    let bitmap: Vec<u8> = (0..128)
                        .collect::<Vec<_>>()
                        .chunks(8)
                        .map(|bits| {
                            bits.iter().fold(0, |value, bit| {
                                value | (u8::from(*bit < index) << (bit % 8))
                            })
                        })
                        .collect();
                    assert_eq!(fields["alreadyupbit"], bitmap);
                    put(&mut reply, "offset", offset);
                    put(&mut reply, "fin", 0);
                    reply.insert("etag".into(), format!("etag-{index}").into_bytes());
                }
                "fin" => {
                    assert_eq!(fields["upinfo"], b"apply-token");
                    assert_eq!(integer(fields, "blocksum")?, self.blocks() as i64);
                    for index in 0..self.blocks() {
                        assert_eq!(
                            fields[&format!("etag_{index}")],
                            format!("etag-{index}").as_bytes()
                        );
                    }
                    reply.insert("upinfo".into(), b"finish-token".to_vec());
                }
                "complete" => {
                    assert_eq!(integer(fields, "type")?, i64::from(kind));
                    assert_eq!(integer(fields, "num")?, 1);
                    assert_eq!(fields["upinfo_0"], b"finish-token");
                    reply.insert("fileid".into(), b"completed-object".to_vec());
                }
                _ => unreachable!(),
            }
            Ok(reply)
        }
    }

    #[test]
    fn a_file_source_sends_the_same_blocks_and_a_changed_file_never_finishes() {
        let memory = prepared();
        let raw = vec![0x5a; MAX_CDN_TRANSFER_BYTES + 17];
        let directory = wechat_platform::private_tempdir("wechat-large-file-test-").unwrap();
        let path = directory.path().join("source");
        std::fs::write(&path, &raw).unwrap();
        let open = || {
            PreparedFileUpload::new(
                std::fs::File::open(&path).unwrap(),
                raw.len() as u64,
                &memory.md5,
                &memory.sha256,
                memory.kind,
                &memory.file_name,
                &memory.destination,
                &memory.aes_key,
                &memory.file_key,
            )
            .unwrap()
        };
        // The peer checks every block against the in-memory ciphertext.
        let file = open();
        let mut peer = Peer::new(memory, 512 * 1024);
        let result = super::upload_large_file(
            &config(),
            UploadRef::File(&file),
            987654321,
            |path, fields, _, _| counted(peer.respond(path, fields)),
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(peer.requests, peer.blocks() + 3);
        assert_eq!(result.descriptor.file_id, "completed-object");
        assert_eq!(result.sha256, memory.sha256);
        // Changed after preparation: every block but the last is sent, then the
        // transfer stops before `fin`.
        let mut changed = raw.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        std::fs::write(&path, &changed).unwrap();
        let file = open();
        let mut peer = Peer::new(memory, 512 * 1024);
        let mut phases = vec![];
        let error = super::upload_large_file(
            &config(),
            UploadRef::File(&file),
            987654321,
            |path, fields, _, _| {
                phases.push(path.rsplit('/').next().unwrap().to_string());
                counted(peer.respond(path, fields))
            },
            |_, _| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.root_cause().to_string(),
            "Prepared upload digest mismatch"
        );
        let failure = LargeFileUploadFailure::find(&error).unwrap();
        assert_eq!(
            (failure.phase, failure.attempts, failure.cause),
            ("uploadblock", 0, LargeFileUploadCause::Local)
        );
        assert!(!phases
            .iter()
            .any(|phase| phase == "fin" || phase == "complete"));
        assert_eq!(
            phases
                .iter()
                .filter(|phase| *phase == "uploadblock")
                .count(),
            peer.blocks() - 1
        );
    }

    #[test]
    fn transfers_ordered_blocks_and_uses_finish_token() {
        let prepared = prepared();
        for block_size in [512 * 1024, 1024 * 1024] {
            let mut peer = Peer::new(prepared, block_size);
            let events = RefCell::new(Vec::new());
            let progress = RefCell::new(Vec::new());
            let result = upload_large_file(
                &config(),
                prepared,
                987654321,
                |path, fields| {
                    events.borrow_mut().push("request");
                    peer.respond(path, fields)
                },
                |done, total| {
                    events.borrow_mut().push("progress");
                    progress.borrow_mut().push(done);
                    assert_eq!(total, Some(prepared.encrypted.len() as u64));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(peer.requests, peer.blocks() + 3);
            let events = events.borrow();
            for event in events[..events.len() - 2].chunks(3) {
                assert_eq!(event, ["progress", "request", "progress"]);
            }
            assert_eq!(&events[events.len() - 2..], ["progress", "request"]);
            let positions = progress.borrow();
            assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(
                positions.last().copied(),
                Some(prepared.encrypted.len() as u64)
            );
            assert_eq!(result.descriptor.file_id, "completed-object");
            assert_eq!(result.descriptor.quality, MediaQuality::LargeFile);
            assert_eq!(
                result.descriptor.expected_bytes,
                Some(prepared.plaintext_bytes as u64)
            );
            assert_eq!(result.descriptor.aes_key, prepared.aes_key);
            assert_eq!(result.md5, prepared.md5);
            assert_eq!(result.sha256, prepared.sha256);
        }
    }

    #[test]
    fn standalone_original_video_uses_type_four_without_thumbnail() {
        let mut raw = prepared().clone();
        raw.kind = MediaQuality::Video;
        raw.file_name = "original.mp4".into();
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        let cancelled = std::cell::Cell::new(false);
        let mut peer = Peer::new(&raw, 1024 * 1024);
        let result = upload_video_original(
            &cdn,
            &raw,
            987654321,
            |path, fields| {
                assert!(!fields.contains_key("thumbdata"));
                assert!(!fields.contains_key("type_1"));
                let response = peer.respond(path, fields)?;
                if path.ends_with("/complete") {
                    cancelled.set(true);
                }
                Ok(response)
            },
            |_, _| {
                ensure!(!cancelled.get(), "cancelled");
                Ok(())
            },
        )
        .unwrap();
        assert!(cancelled.get());
        assert_eq!(result.descriptor.quality, MediaQuality::Video);
        assert_eq!(
            result.descriptor.expected_bytes,
            Some(u64::from(raw.plaintext_bytes))
        );
        assert_eq!(result.descriptor.aes_key, raw.aes_key);
        assert_eq!(result.md5, raw.md5);
        assert_eq!(result.sha256, raw.sha256);
        assert_eq!(peer.requests, peer.blocks() + 3);
        assert!(cdn
            .upload_large_file_with_progress(&raw, 987654321, |_, _| {
                panic!("Video admitted by ordinary-file interface")
            })
            .is_err());
    }

    #[test]
    fn small_original_video_uses_standalone_legacy_and_rejects_bad_material_locally() {
        let raw = PreparedUpload::with_material(
            b"\0\0\0\x0cftypisom",
            MediaQuality::Video,
            "original.mp4",
            "filehelper",
            &[0x31; 16],
            "00112233445566778899aabbccddeeff",
        )
        .unwrap();
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        let mut calls = 0;
        let result = upload_video_original(
            &cdn,
            &raw,
            987654321,
            |path, fields| {
                calls += 1;
                assert_eq!(path, "/uploadv3");
                assert_eq!(integer(fields, "filetype")?, 4);
                assert!(!fields.contains_key("thumbdata"));
                assert_eq!(integer(fields, "hasthumb")?, 0);
                let mut reply = CdnFields::new();
                put(&mut reply, "retcode", 0);
                for key in ["seq", "rangestart", "rangeend"] {
                    reply.insert(key.into(), fields[key].clone());
                }
                reply.insert("fileid".into(), b"small-original".to_vec());
                Ok(reply)
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.descriptor.quality, MediaQuality::Video);
        let mut over = raw.clone();
        over.plaintext_bytes = wechat_media::MAX_VIDEO_SOURCE_BYTES as u32 + 1;
        let mut wrong = raw.clone();
        wrong.kind = MediaQuality::File;
        let mut corrupt = raw.clone();
        corrupt.encrypted[0] ^= 1;
        for (value, task) in [
            (&over, 987654321),
            (&wrong, 987654321),
            (&corrupt, 987654321),
            (&raw, 0),
        ] {
            assert!(upload_video_original(
                &cdn,
                value,
                task,
                |_, _| { panic!("Invalid original video reached transport") },
                |_, _| Ok(())
            )
            .is_err());
        }
    }

    #[test]
    fn original_video_padding_boundary_selects_one_carrier_without_fallback() {
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        for (size, expected_path) in [
            (MAX_NATIVE_UPLOAD_BYTES, "/uploadv3"),
            (MAX_NATIVE_UPLOAD_BYTES + 1, "/cdn/12345/1/4/apply"),
        ] {
            let raw = PreparedUpload::with_material(
                &vec![0x5a; size],
                MediaQuality::Video,
                "original.mp4",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap();
            let mut calls = 0;
            let error = upload_video_original(
                &cdn,
                &raw,
                987654321,
                |path, fields| {
                    calls += 1;
                    assert_eq!(path, expected_path);
                    assert_eq!(integer(fields, "filetype")?, 4);
                    bail!("isolated transport failure")
                },
                |_, _| Ok(()),
            )
            .err()
            .unwrap();
            assert_eq!(error.root_cause().to_string(), "isolated transport failure");
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn padding_band_and_exact_threshold_keep_filetype_five() {
        for (plaintext, ciphertext, requests) in [
            (MAX_CDN_TRANSFER_BYTES - 16, MAX_CDN_TRANSFER_BYTES, 28),
            (MAX_CDN_TRANSFER_BYTES - 1, MAX_CDN_TRANSFER_BYTES, 28),
            (MAX_CDN_TRANSFER_BYTES, MAX_CDN_TRANSFER_BYTES + 16, 29),
        ] {
            let prepared = PreparedUpload::with_material(
                &vec![0x5a; plaintext],
                MediaQuality::File,
                "boundary.fixture.bin",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap();
            assert_eq!(prepared.encrypted.len(), ciphertext);
            let legacy = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
            let rejected = legacy.upload_with_progress(&prepared, |_, _| {
                panic!("Legacy transfer started for a multiphase file")
            });
            assert_eq!(
                rejected.err().unwrap().to_string(),
                "File requires multiphase upload"
            );
            let mut peer = Peer::new(&prepared, 1024 * 1024);
            let result = upload_large_file(
                &config(),
                &prepared,
                987654321,
                |path, fields| peer.respond(path, fields),
                |_, _| Ok(()),
            )
            .unwrap();
            assert_eq!(result.descriptor.quality, MediaQuality::File);
            assert_eq!(peer.requests, requests);
        }
    }

    #[test]
    fn completed_object_receipt_survives_late_cancellation() {
        let cancelled = std::cell::Cell::new(false);
        let mut peer = Peer::new(prepared(), 1024 * 1024);
        let uploaded = upload_large_file(
            &config(),
            prepared(),
            987654321,
            |path, fields| {
                let reply = peer.respond(path, fields)?;
                if path.ends_with("/complete") {
                    cancelled.set(true);
                }
                Ok(reply)
            },
            |_, _| {
                ensure!(!cancelled.get(), "cancelled");
                Ok(())
            },
        )
        .unwrap();
        assert!(cancelled.get());
        assert_eq!(uploaded.descriptor.file_id, "completed-object");
        assert_eq!(peer.requests, peer.blocks() + 3);
    }

    #[test]
    fn rejects_apply_changes_and_cache_hits_without_sending_blocks() {
        for (key, value) in [
            ("seq", 2),
            ("ver", 2),
            // A rejection whose isretry is 0 is not retried.
            ("retcode", -1),
            ("hit", 1),
            ("type_0", 5),
            ("num", 2),
            ("fin_0", 1),
            ("blocksize_0", 2 * 1024 * 1024),
            ("blocksize_0", -1),
        ] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    put(&mut reply, key, value);
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err(), "Accepted invalid {key}");
            assert_eq!(peer.requests, 1);
        }
    }

    #[test]
    fn rejected_reply_preserves_server_code_without_trusting_wrong_sequence() {
        let mut reply = CdnFields::new();
        put(&mut reply, "seq", 1);
        put(&mut reply, "ver", 0);
        put(&mut reply, "retcode", -5103015);
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file upload returned -5103015"
        );
        put(&mut reply, "seq", 2);
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file reply sequence mismatch"
        );
        put(&mut reply, "seq", 1);
        put(&mut reply, "retcode", 0);
        for key in ["isretry", "retrysec", "isoverload", "isgetcdn"] {
            put(&mut reply, key, 0);
        }
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file reply version mismatch"
        );
        put(&mut reply, "ver", 1);
        validate_reply(&reply, 1, 1).unwrap();
        // A successful reply's controls are not acted on, as in the official client.
        for key in ["isretry", "isoverload", "isgetcdn"] {
            put(&mut reply, key, 1);
        }
        validate_reply(&reply, 1, 1).unwrap();
    }

    #[test]
    fn server_errors_are_retried_as_the_official_client_does() {
        // isretry=1 on a rejected block: retried with a new sequence number on the
        // next address, the block itself unchanged.
        let memory = prepared();
        let mut peer = Peer::new(memory, 1024 * 1024);
        let mut sent = vec![];
        let result = super::upload_large_file(
            &config(),
            UploadRef::Memory(memory),
            987654321,
            |path, fields, _, switch| {
                let block = path
                    .ends_with("/uploadblock")
                    .then(|| integer(fields, "offset").unwrap());
                sent.push((block, switch, fields.get("blockdata").cloned()));
                if block == Some(2 * 1024 * 1024) && peer.rejected == 0 {
                    return counted(Ok(peer.reject(fields, -5103202, Some(1))));
                }
                counted(peer.respond(path, fields))
            },
            |_, _| Ok(()),
        );
        if let Err(error) = result {
            panic!("{error:#}");
        }
        let tries: Vec<_> = sent
            .iter()
            .filter(|(block, _, _)| *block == Some(2 * 1024 * 1024))
            .collect();
        assert_eq!(tries.len(), 2);
        assert_eq!(tries[0].2, tries[1].2, "the block is resent unchanged");
        assert_eq!(
            (tries[0].1, tries[1].1),
            (false, true),
            "the retry moves address"
        );
        assert_eq!(peer.requests, peer.blocks() + 3);

        // Without isretry the code decides: retried codes continue, others stop.
        for (code, retried) in [(-5103215, true), (-5103015, false)] {
            let mut peer = Peer::new(memory, 1024 * 1024);
            let result = super::upload_large_file(
                &config(),
                UploadRef::Memory(memory),
                987654321,
                |path, fields, _, _| {
                    if path.ends_with("/fin") && peer.rejected == 0 {
                        return counted(Ok(peer.reject(fields, code, None)));
                    }
                    counted(peer.respond(path, fields))
                },
                |_, _| Ok(()),
            );
            assert_eq!(result.is_ok(), retried, "{code}");
        }

        // isretry=0 stops even on a code that would otherwise be retried.
        let mut peer = Peer::new(memory, 1024 * 1024);
        let error = super::upload_large_file(
            &config(),
            UploadRef::Memory(memory),
            987654321,
            |path, fields, _, _| {
                if path.ends_with("/apply") {
                    return counted(Ok(peer.reject(fields, -5103202, Some(0))));
                }
                counted(peer.respond(path, fields))
            },
            |_, _| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(ServiceRejected::find(&error).unwrap().code, -5103202);
        assert_eq!(peer.rejected, 1);
    }

    #[test]
    fn server_retries_stop_after_eight_and_keep_the_last_code() {
        let memory = prepared();
        let mut peer = Peer::new(memory, 1024 * 1024);
        let error = super::upload_large_file(
            &config(),
            UploadRef::Memory(memory),
            987654321,
            |path, fields, _, _| {
                if path.ends_with("/uploadblock") && integer(fields, "offset").unwrap() == 0 {
                    return counted(Ok(peer.reject(fields, -5103216, Some(1))));
                }
                counted(peer.respond(path, fields))
            },
            |_, _| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(peer.rejected, MAX_SERVER_RETRIES as usize + 1);
        assert_eq!(ServiceRejected::find(&error).unwrap().code, -5103216);
        assert_eq!(
            LargeFileUploadFailure::find(&error),
            Some(&LargeFileUploadFailure {
                phase: "uploadblock",
                block: Some(0),
                attempts: 1,
                server_retries: MAX_SERVER_RETRIES,
                cause: LargeFileUploadCause::ServiceRejected,
            })
        );
    }

    #[test]
    fn a_server_retry_wait_honours_retrysec_and_cancellation() {
        let memory = prepared();
        let mut reply = CdnFields::new();
        put(&mut reply, "isretry", 1);
        put(&mut reply, "retrysec", 7);
        assert_eq!(
            server_retry_delay(&reply, -5103015, 0),
            Some(Duration::from_secs(7))
        );
        // Without a usable retrysec: fast codes at once, others back off.
        put(&mut reply, "retrysec", 900);
        assert_eq!(
            server_retry_delay(&reply, -5103202, 3),
            Some(Duration::ZERO)
        );
        assert_eq!(
            server_retry_delay(&reply, -5202163, 0),
            Some(Duration::ZERO)
        );
        assert_eq!(
            server_retry_delay(&reply, -5202163, 4),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            server_retry_delay(&reply, -5103015, 20),
            Some(Duration::from_secs(6))
        );
        put(&mut reply, "isretry", 0);
        assert_eq!(server_retry_delay(&reply, -5103202, 0), None);

        // The waits of one upload stay within the official transfer limit.
        let rejected = anyhow::Error::new(ServiceRejected::new(Service::LargeFileUpload, -5103202));
        put(&mut reply, "isretry", 1);
        put(&mut reply, "retrysec", 600);
        let retries = ServerRetries::default();
        assert_eq!(
            retries.next(&reply, &rejected),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            retries.next(&reply, &rejected),
            None,
            "1200 s would exceed 900 s"
        );
        assert_eq!(retries.count(), 1);

        // A cancellation during the wait stops at once, without another request.
        let mut peer = Peer::new(memory, 1024 * 1024);
        let mut cancelled = false;
        let started = Instant::now();
        let error = super::upload_large_file(
            &config(),
            UploadRef::Memory(memory),
            987654321,
            |path, fields, _, _| {
                if path.ends_with("/apply") {
                    let mut reply = peer.reject(fields, -5103202, Some(1));
                    put(&mut reply, "retrysec", 60);
                    return counted(Ok(reply));
                }
                counted(peer.respond(path, fields))
            },
            |_, _| {
                if std::mem::replace(&mut cancelled, true) {
                    bail!("media_cancelled");
                }
                Ok(())
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.root_cause().to_string(), "media_cancelled");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!((peer.rejected, peer.requests), (1, 0));
    }

    #[test]
    fn missing_authenticated_location_stops_before_any_exchange() {
        let mut conf = config();
        conf.location = None;
        let mut requests = 0;
        let result = upload_large_file(
            &conf,
            prepared(),
            987654321,
            |_, _| {
                requests += 1;
                bail!("unexpected request")
            },
            |_, _| Ok(()),
        );
        assert_eq!(
            result.err().unwrap().to_string(),
            "Missing authenticated CDN location"
        );
        assert_eq!(requests, 0);
    }

    #[test]
    fn rejects_conflicting_or_incomplete_block_acknowledgement() {
        for bad in ["offset", "fin", "etag", "seq"] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    if path.ends_with("/uploadblock") {
                        if bad == "etag" {
                            reply.remove("etag");
                        } else {
                            put(&mut reply, bad, 1);
                        }
                    }
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            let error = result
                .err()
                .unwrap_or_else(|| panic!("Accepted invalid {bad}"));
            assert_eq!(peer.requests, 2);
            assert_eq!(
                LargeFileUploadFailure::find(&error),
                Some(&LargeFileUploadFailure {
                    phase: "uploadblock",
                    block: Some(0),
                    attempts: 1,
                    server_retries: 0,
                    cause: LargeFileUploadCause::ReplyInvalid,
                }),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_stopped_upload_names_its_request_block_and_cause() {
        // Server rejection of the third block keeps its code and names the block.
        let mut peer = Peer::new(prepared(), 1024 * 1024);
        let error = upload_large_file(
            &config(),
            prepared(),
            987654321,
            |path, fields| {
                let mut reply = peer.respond(path, fields)?;
                if path.ends_with("/uploadblock") && integer(fields, "offset")? == 2 * 1024 * 1024 {
                    put(&mut reply, "retcode", -5103015);
                }
                Ok(reply)
            },
            |_, _| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(ServiceRejected::find(&error).unwrap().code, -5103015);
        assert_eq!(
            LargeFileUploadFailure::find(&error),
            Some(&LargeFileUploadFailure {
                phase: "uploadblock",
                block: Some(2),
                attempts: 1,
                server_retries: 0,
                cause: LargeFileUploadCause::ServiceRejected,
            })
        );
        assert_eq!(
            error.to_string(),
            "Large-file upload stopped at uploadblock block 2"
        );

        // Like the official client, a successful reply's controls are not acted on.
        let mut peer = Peer::new(prepared(), 1024 * 1024);
        let uploaded = upload_large_file(
            &config(),
            prepared(),
            987654321,
            |path, fields| {
                let mut reply = peer.respond(path, fields)?;
                if path.ends_with("/fin") {
                    for key in ["isoverload", "isgetcdn", "isretry"] {
                        put(&mut reply, key, 1);
                    }
                    put(&mut reply, "retrysec", 30);
                }
                Ok(reply)
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(uploaded.descriptor.file_id, "completed-object");

        // Cancellation reported by progress after the second block is local.
        let mut peer = Peer::new(prepared(), 1024 * 1024);
        let error = upload_large_file(
            &config(),
            prepared(),
            987654321,
            |path, fields| peer.respond(path, fields),
            |done, _| {
                ensure!(done < 2 * 1024 * 1024, "media_cancelled");
                Ok(())
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.root_cause().to_string(), "media_cancelled");
        let failure = LargeFileUploadFailure::find(&error).unwrap();
        assert_eq!(
            (
                failure.phase,
                failure.block,
                failure.attempts,
                failure.cause
            ),
            ("uploadblock", Some(1), 0, LargeFileUploadCause::Local)
        );
    }

    #[test]
    fn cancellation_before_request_or_after_ack_never_continues() {
        for cancel_at in [1, 2, 3, 4, 5, 6] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let mut callbacks = 0;
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| peer.respond(path, fields),
                |_, _| {
                    callbacks += 1;
                    ensure!(callbacks != cancel_at, "cancelled");
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(callbacks, cancel_at);
            assert_eq!(peer.requests, cancel_at / 2);
        }
    }

    #[test]
    fn transport_failure_is_not_retried_and_missing_completion_is_not_success() {
        let mut requests = 0;
        assert!(upload_large_file(
            &config(),
            prepared(),
            987654321,
            |_, _| {
                requests += 1;
                bail!("transport failed")
            },
            |_, _| Ok(())
        )
        .is_err());
        assert_eq!(requests, 1);
        for phase in ["fin", "complete"] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    if path.ends_with(&format!("/{phase}")) {
                        reply.remove(if phase == "fin" { "upinfo" } else { "fileid" });
                    }
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err());
            assert_eq!(
                peer.requests,
                peer.blocks() + if phase == "fin" { 2 } else { 3 }
            );
        }
    }

    #[test]
    fn invalid_persisted_material_and_task_are_rejected_before_network() {
        let mut altered = prepared().clone();
        altered.encrypted[0] ^= 1;
        let small = PreparedUpload::with_material(
            b"small control",
            MediaQuality::File,
            "small.bin",
            "filehelper",
            &[0x31; 16],
            "00112233445566778899aabbccddeeff",
        )
        .unwrap();
        let mut requests = 0;
        for (prepared, task) in [(&altered, 987654321), (&small, 987654321), (prepared(), 0)] {
            assert!(upload_large_file(
                &config(),
                prepared,
                task,
                |_, _| {
                    requests += 1;
                    bail!("must not reach transport")
                },
                |_, _| Ok(())
            )
            .is_err());
        }
        assert_eq!(requests, 0);
    }

    #[test]
    fn large_file_session_reuses_keepalive_and_recovers_from_uploadblock_drop() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        let memory = prepared();
        let peer = Arc::new(Mutex::new(Peer::new(memory, 1024 * 1024)));
        let dropped = Arc::new(AtomicBool::new(false));
        let peer_ref = peer.clone();
        let drop_once = dropped.clone();
        let server = super::super::tests::MockCdnServer::start(move |_req, path, fields| {
            if path.ends_with("/uploadblock")
                && integer(fields, "offset").unwrap() == 1024 * 1024
                && !drop_once.swap(true, Ordering::SeqCst)
            {
                return super::super::tests::MockCdnAction::DropBeforeResponse;
            }
            let reply = peer_ref.lock().unwrap().respond(path, fields).unwrap();
            super::super::tests::MockCdnAction::Ok {
                reply,
                close: false,
            }
        });
        let conf = config();
        let cdn = CdnClient::new(conf.clone(), Duration::from_secs(5)).unwrap();
        let mut session = CdnSession::new(
            &cdn,
            vec![server.addr],
            super::super::tests::FAST_RETRY_POLICY,
        );
        let result = super::upload_large_file(
            &conf,
            UploadRef::Memory(memory),
            987654321,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(result.descriptor.file_id, "completed-object");
        assert_eq!(result.sha256, memory.sha256);
        let expected_blocks = memory.encrypted.len().div_ceil(1024 * 1024);
        assert_eq!(server.requests(), expected_blocks + 4);
        assert_eq!(
            server.connections(),
            2,
            "initial keep-alive connection + 1 fresh socket after drop"
        );
    }

    #[test]
    fn unreadable_block_replies_are_resent_unchanged_and_the_upload_completes() {
        // The CDN answers a repeated block, even one it already accepted, with the
        // same reply. An empty or unparseable 200 is therefore resent byte for byte.
        use super::super::tests::{MockCdnAction, MockCdnServer, FAST_RETRY_POLICY};
        use std::sync::{Arc, Mutex};
        let memory = prepared();
        let peer = Arc::new(Mutex::new(Peer::new(memory, 1024 * 1024)));
        let seen = Arc::new(Mutex::new(Vec::<(String, CdnFields)>::new()));
        let (peer_ref, seen_ref) = (peer.clone(), seen.clone());
        let server = MockCdnServer::start(move |_req, path, fields| {
            let mut seen = seen_ref.lock().unwrap();
            seen.push((path.to_string(), fields.clone()));
            let first_try = seen.iter().filter(|(seen, _)| seen == path).count() == 1;
            if path.ends_with("/uploadblock") && first_try {
                match integer(fields, "offset").unwrap() {
                    0x10_0000 => return MockCdnAction::Raw(vec![]),
                    0x20_0000 => return MockCdnAction::Raw(b"not a cdn reply".to_vec()),
                    _ => {}
                }
            }
            MockCdnAction::Ok {
                reply: peer_ref.lock().unwrap().respond(path, fields).unwrap(),
                close: false,
            }
        });
        let conf = config();
        let cdn = CdnClient::new(conf.clone(), Duration::from_secs(5)).unwrap();
        let mut session = CdnSession::new(&cdn, vec![server.addr], FAST_RETRY_POLICY);
        let result = super::upload_large_file(
            &conf,
            UploadRef::Memory(memory),
            987654321,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(result.descriptor.file_id, "completed-object");
        let blocks = memory.encrypted.len().div_ceil(1024 * 1024);
        // apply, fin and complete, every block once, and the two resends.
        assert_eq!(server.requests(), blocks + 3 + 2);
        let seen = seen.lock().unwrap();
        for offset in [0x10_0000, 0x20_0000] {
            let tries: Vec<_> = seen
                .iter()
                .filter(|(path, fields)| {
                    path.ends_with("/uploadblock") && integer(fields, "offset").unwrap() == offset
                })
                .collect();
            assert_eq!(tries.len(), 2, "block at {offset} is sent twice");
            assert_eq!(tries[0], tries[1], "the resend repeats the request exactly");
        }
    }

    #[test]
    fn an_http_error_status_on_a_block_is_reported_as_http_status() {
        // 403 is fatal at once; 503 is retried until the session gives up. Both are
        // HttpStatus, which `CdnPostFailure::attempt` picks from the status message.
        use super::super::tests::{MockCdnAction, MockCdnServer, FAST_RETRY_POLICY, ZERO_BACKOFFS};
        use std::sync::{Arc, Mutex};
        for (status, attempts) in [(403, 1), (503, ZERO_BACKOFFS.len() as u32 + 1)] {
            let memory = prepared();
            let peer = Arc::new(Mutex::new(Peer::new(memory, 1024 * 1024)));
            let peer_ref = peer.clone();
            let server = MockCdnServer::start(move |_req, path, fields| {
                if path.ends_with("/uploadblock") && integer(fields, "offset").unwrap() == 0x10_0000
                {
                    return MockCdnAction::Status(status);
                }
                MockCdnAction::Ok {
                    reply: peer_ref.lock().unwrap().respond(path, fields).unwrap(),
                    close: false,
                }
            });
            let conf = config();
            let cdn = CdnClient::new(conf.clone(), Duration::from_secs(5)).unwrap();
            let mut session = CdnSession::new(&cdn, vec![server.addr], FAST_RETRY_POLICY);
            let error = super::upload_large_file(
                &conf,
                UploadRef::Memory(memory),
                987654321,
                |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
                |_, _| Ok(()),
            )
            .err()
            .unwrap();
            assert_eq!(
                error.root_cause().to_string(),
                format!("CDN HTTP status {status}")
            );
            assert_eq!(
                LargeFileUploadFailure::find(&error),
                Some(&LargeFileUploadFailure {
                    phase: "uploadblock",
                    block: Some(1),
                    attempts,
                    server_retries: 0,
                    cause: LargeFileUploadCause::HttpStatus,
                }),
                "{status}"
            );
        }
    }

    #[test]
    fn a_block_whose_replies_stay_unreadable_says_where_the_upload_stopped() {
        use super::super::tests::{MockCdnAction, MockCdnServer, FAST_RETRY_POLICY, ZERO_BACKOFFS};
        use std::sync::{Arc, Mutex};
        let memory = prepared();
        let peer = Arc::new(Mutex::new(Peer::new(memory, 1024 * 1024)));
        let peer_ref = peer.clone();
        let server = MockCdnServer::start(move |_req, path, fields| {
            if path.ends_with("/uploadblock") && integer(fields, "offset").unwrap() == 0x10_0000 {
                return MockCdnAction::Raw(vec![]);
            }
            MockCdnAction::Ok {
                reply: peer_ref.lock().unwrap().respond(path, fields).unwrap(),
                close: false,
            }
        });
        let conf = config();
        let cdn = CdnClient::new(conf.clone(), Duration::from_secs(5)).unwrap();
        let mut session = CdnSession::new(&cdn, vec![server.addr], FAST_RETRY_POLICY);
        let error = super::upload_large_file(
            &conf,
            UploadRef::Memory(memory),
            987654321,
            |path, fields, fence, switch| session.post_large(path, fields, fence, switch),
            |_, _| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(error.root_cause().to_string(), "Empty CDN reply");
        assert_eq!(
            LargeFileUploadFailure::find(&error),
            Some(&LargeFileUploadFailure {
                phase: "uploadblock",
                block: Some(1),
                attempts: ZERO_BACKOFFS.len() as u32 + 1,
                server_retries: 0,
                cause: LargeFileUploadCause::ReplyUnreadable,
            })
        );
        assert_eq!(server.requests(), 2 + ZERO_BACKOFFS.len() + 1);
    }

    #[tokio::test]
    async fn async_large_file_upload_supports_memory_and_streaming_source() {
        use std::sync::{Arc, Mutex};

        struct InstantClock;
        impl Clock for InstantClock {
            fn monotonic_ms(&self) -> u64 {
                1000
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(1_700_000_000_000)
            }
        }

        struct PeerHttp(Mutex<Peer<'static>>);
        impl HttpTransport for PeerHttp {
            fn send<'a>(
                &'a self,
                request: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                let outcome = (|| {
                    let fields = unpack_cdn(&request.body)?;
                    let reply = self
                        .0
                        .lock()
                        .unwrap()
                        .respond(request.url.path(), &fields)?;
                    Ok(crate::transport::HttpResponse {
                        status: 200,
                        headers: vec![],
                        body: pack_cdn(&reply)?,
                    })
                })();
                Box::pin(async move { outcome })
            }
        }

        struct VecSource(Arc<Vec<u8>>);
        impl AsyncUploadSource for VecSource {
            fn read_at<'a>(
                &'a self,
                offset: u64,
                buf: &'a mut [u8],
            ) -> crate::transport::TransportFuture<'a, Result<()>> {
                let outcome = (|| {
                    let start = usize::try_from(offset)?;
                    let end = start + buf.len();
                    ensure!(end <= self.0.len(), "Upload source changed");
                    buf.copy_from_slice(&self.0[start..end]);
                    Ok(())
                })();
                Box::pin(async move { outcome })
            }
        }

        let memory = prepared();
        let cdn = CdnClient::new(config(), Duration::from_secs(5)).unwrap();

        let http_mem = PeerHttp(Mutex::new(Peer::new(memory, 1024 * 1024)));
        let uploaded_mem = cdn
            .upload_large_file_with_progress_async(
                memory,
                987654321,
                &http_mem,
                &InstantClock,
                |_, _| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(uploaded_mem.descriptor.file_id, "completed-object");
        assert_eq!(uploaded_mem.sha256, memory.sha256);

        let raw = Arc::new(vec![0x5a; MAX_CDN_TRANSFER_BYTES + 17]);
        let async_source = PreparedAsyncUpload::new(
            Arc::new(VecSource(raw)),
            (MAX_CDN_TRANSFER_BYTES + 17) as u64,
            &memory.md5,
            &memory.sha256,
            memory.kind,
            &memory.file_name,
            &memory.destination,
            &memory.aes_key,
            &memory.file_key,
        )
        .unwrap();
        let http_src = PeerHttp(Mutex::new(Peer::new(memory, 1024 * 1024)));
        let uploaded_src = cdn
            .upload_large_file_from_source_with_progress_async(
                &async_source,
                987654321,
                &http_src,
                &InstantClock,
                |_, _| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(uploaded_src.descriptor.file_id, "completed-object");
        assert_eq!(uploaded_src.sha256, memory.sha256);
    }
    #[tokio::test]
    async fn async_upload_resends_unreadable_replies_and_retries_server_errors() {
        use std::sync::Mutex;

        struct InstantClock;
        impl Clock for InstantClock {
            fn monotonic_ms(&self) -> u64 {
                1000
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(1_700_000_000_000)
            }
        }

        // Block 1 first gets an empty 200 (resent with the same sequence number);
        // block 2 is first rejected with isretry=1 (retried with a new one).
        struct FlakyHttp(Mutex<(Peer<'static>, Vec<(i64, i64)>)>);
        impl HttpTransport for FlakyHttp {
            fn send<'a>(
                &'a self,
                request: HttpRequest,
            ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>>
            {
                let outcome = (|| {
                    let fields = unpack_cdn(&request.body)?;
                    let mut state = self.0.lock().unwrap();
                    let (peer, sent) = &mut *state;
                    let offset = if request.url.path().ends_with("/uploadblock") {
                        integer(&fields, "offset")?
                    } else {
                        -1
                    };
                    let seen = sent.iter().filter(|(o, _)| *o == offset).count();
                    sent.push((offset, integer(&fields, "seq")?));
                    let body = match (offset, seen) {
                        (0x10_0000, 0) => vec![],
                        (0x20_0000, 0) => pack_cdn(&peer.reject(&fields, -5103202, Some(1)))?,
                        _ => pack_cdn(&peer.respond(request.url.path(), &fields)?)?,
                    };
                    Ok(crate::transport::HttpResponse {
                        status: 200,
                        headers: vec![],
                        body,
                    })
                })();
                Box::pin(async move { outcome })
            }
        }

        let memory = prepared();
        let cdn = CdnClient::new(config(), Duration::from_secs(5)).unwrap();
        let http = FlakyHttp(Mutex::new((Peer::new(memory, 1024 * 1024), vec![])));
        let uploaded = cdn
            .upload_large_file_with_progress_async(
                memory,
                987654321,
                &http,
                &InstantClock,
                |_, _| Ok(()),
            )
            .await
            .unwrap();
        assert_eq!(uploaded.descriptor.file_id, "completed-object");
        let state = http.0.lock().unwrap();
        let seqs = |offset| -> Vec<i64> {
            state
                .1
                .iter()
                .filter(|(o, _)| *o == offset)
                .map(|(_, seq)| *seq)
                .collect()
        };
        let [first, second] = seqs(0x10_0000)[..] else {
            panic!("block 1 is sent twice")
        };
        assert_eq!(first, second, "an unreadable reply is resent unchanged");
        let [first, second] = seqs(0x20_0000)[..] else {
            panic!("block 2 is sent twice")
        };
        assert_eq!(
            second,
            first + 1,
            "a server retry takes the next sequence number"
        );
    }
}
