//! Server-owned media transfers and one-shot outgoing media jobs.
use crate::media_types::*;
use crate::{source_message_id, Core};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use wechat_protocol::{
    media::{
        CdnClient, MediaDescriptor, MediaQuality, PreparedImageUpload, PreparedUpload,
        PreparedVideoUpload, StickerDescriptor, UploadedImage, UploadedMedia, UploadedVideo,
    },
    NativeClient, NativeOptions,
};
use wechat_store::{MediaAssetRecord, MediaJobRecord, OwnedMediaSource};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
enum DownloadPlan {
    Cdn {
        descriptor: MediaDescriptor,
    },
    Sticker {
        descriptor: StickerDescriptor,
    },
    Voice {
        server_id: u64,
        length: u32,
        inline: Option<Vec<u8>>,
        as_wav: bool,
    },
    CachedOnly,
}
#[derive(Clone, Serialize, Deserialize)]
struct VoicePart {
    client_id: String,
    encoded: Vec<u8>,
    duration_ms: u32,
    status: String,
    server_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct UploadMaterial {
    key: Vec<u8>,
    file_key: String,
}
impl Drop for UploadMaterial {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
        self.file_key.zeroize();
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct SendPlan {
    request: MediaSendRequest,
    client_id: String,
    created_at: u32,
    material: Option<UploadMaterial>,
    uploaded_file: Option<UploadedMedia>,
    uploaded_image: Option<UploadedImage>,
    uploaded_video: Option<UploadedVideo>,
    #[serde(default)]
    prepared_video_thumbnail_source_id: Option<String>,
    voice: Vec<VoicePart>,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn wxgf_preview_source_id(source_sha256: &str) -> String {
    format!("wxgf-preview-v1_{source_sha256}")
}
fn media_request_hash(
    request: &MediaSendRequest,
    primary: &str,
    standard: Option<&str>,
    thumbnail: Option<&str>,
    filename: Option<&str>,
) -> Result<String> {
    let normalized = json!({"threadId":request.thread_id,"kind":request.kind,"playSeconds":request.play_seconds,"durationMs":request.duration_ms,"filename":filename,"sourceHashes":{"primary":primary,"standard":standard,"thumbnail":thumbnail}});
    Ok(digest(&serde_json::to_vec(&normalized)?))
}
fn public_job(account: &str, row: &MediaJobRecord) -> MediaJob {
    MediaJob {
        id: row.id.clone(),
        account_id: account.into(),
        kind: row.kind.clone(),
        state: row.state.clone(),
        phase: row.phase.clone(),
        attempts: row.attempts,
        cancel_requested: row.cancel_requested,
        retryable: row.retryable,
        reason: row.error_code.clone(),
        asset_id: row.data["assetId"].as_str().map(str::to_string),
        source_id: row.data["sourceId"].as_str().map(str::to_string),
        thread_id: row.data["threadId"].as_str().map(str::to_string),
        server_message_id: row.data["serverMessageId"].as_str().map(str::to_string),
        completed_bytes: row.data["completedBytes"].as_u64().unwrap_or(0),
        total_bytes: row.data["totalBytes"].as_u64(),
        result: row.data["result"].clone(),
        created_at: row.created_at.clone(),
        updated_at: row.updated_at.clone(),
    }
}
impl Core {
    pub fn upload_source(
        &self,
        account: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<OwnedMediaSource> {
        self.store.account(account)?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_SOURCE_BYTES,
            "source_size_limit"
        );
        ensure!(
            !name.is_empty()
                && name.len() <= 255
                && name != "."
                && name != ".."
                && !name.contains(['/', '\\'])
                && !name.chars().any(|c| c.is_control()),
            "invalid_upload_filename"
        );
        ensure!(
            mime.len() <= 128 && !mime.chars().any(|c| c.is_control()),
            "invalid_content_type"
        );
        let id = format!("upload_{}", uuid::Uuid::new_v4());
        let sealed = self.vault.seal(account, &format!("source:{id}"), bytes)?;
        self.store.create_media_source(
            account,
            &id,
            name,
            mime,
            bytes.len() as u64,
            &digest(bytes),
            &sealed,
        )
    }
    fn source_bytes(&self, account: &str, id: &str) -> Result<(OwnedMediaSource, Vec<u8>)> {
        let (meta, sealed) = self.store.media_source(account, id)?;
        let bytes = self
            .vault
            .open_record(account, &format!("source:{id}"), &sealed)?
            .to_vec();
        ensure!(
            bytes.len() as u64 == meta.byte_length && digest(&bytes) == meta.sha256,
            "source_integrity_failed"
        );
        Ok((meta, bytes))
    }
    pub fn media_asset(&self, account: &str, id: &str) -> Result<MediaAsset> {
        let row = self.store.media_asset(account, id)?;
        Ok(serde_json::from_value(row.metadata)?)
    }
    pub fn media_content(&self, account: &str, id: &str) -> Result<MediaContent> {
        let row = self.store.media_asset(account, id)?;
        let asset: MediaAsset = serde_json::from_value(row.metadata)?;
        ensure!(asset.state == "available", "media_not_available");
        let sealed = row.payload.context("media_not_cached")?;
        let bytes = self
            .vault
            .open_record(account, &format!("asset:{id}"), &sealed)?
            .to_vec();
        ensure!(
            asset.byte_length == Some(bytes.len() as u64)
                && asset.sha256.as_deref() == Some(&digest(&bytes)),
            "media_integrity_failed"
        );
        Ok(MediaContent {
            bytes,
            content_type: asset
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            sha256: asset.sha256.context("media_integrity_failed")?,
            filename: asset.filename,
            source_sha256: None,
        })
    }
    pub fn media_preview_content(&self, account: &str, id: &str) -> Result<MediaContent> {
        // Verify the retained source as well as the separately encrypted preview.
        let original = self.media_content(account, id)?;
        let asset = self.media_asset(account, id)?;
        let preview = asset.preview.context("media_preview_unavailable")?;
        ensure!(
            preview.source_sha256 == original.sha256,
            "media_integrity_failed"
        );
        let (source, bytes) =
            self.source_bytes(account, &wxgf_preview_source_id(&original.sha256))?;
        ensure!(
            source.sha256 == preview.sha256
                && source.byte_length == preview.byte_length
                && source.media_type == preview.content_type,
            "media_integrity_failed"
        );
        Ok(MediaContent {
            bytes,
            content_type: preview.content_type,
            sha256: preview.sha256,
            filename: Some(preview.filename),
            source_sha256: Some(original.sha256),
        })
    }
    pub fn media_job(&self, account: &str, id: &str) -> Result<MediaJob> {
        Ok(public_job(account, &self.store.media_job(account, id)?))
    }
    pub fn media_job_by_key(&self, account: &str, key: &str) -> Result<MediaJob> {
        Ok(public_job(
            account,
            &self
                .store
                .media_job_by_key(account, &format!("send:{key}"))?,
        ))
    }
    pub fn cancel_media_job(&self, account: &str, id: &str) -> Result<MediaJob> {
        let row = self.store.cancel_media_job(account, id)?;
        Ok(public_job(account, &row))
    }
    pub fn retry_media_job(self: &Arc<Self>, account: &str, id: &str) -> Result<MediaJob> {
        let row = self.store.retry_media_job(account, id)?;
        self.spawn_media_worker(account, id);
        Ok(public_job(account, &row))
    }
    pub fn resume_media_jobs(self: &Arc<Self>) -> Result<usize> {
        let mut n = 0;
        for account in self.store.accounts()? {
            for id in self.store.pending_media_projections(&account.id)? {
                let core = self.clone();
                let account = account.id.clone();
                tokio::spawn(async move {
                    if let Ok(_guard) = core.lock_account(&account).await {
                        let local = core.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            local.project_media_send_receipts(&account, &id)
                        })
                        .await;
                    }
                });
            }
            if self.media_network_suspended(&account.id)? {
                continue;
            }
            for id in self.store.queued_media_jobs(&account.id)? {
                self.spawn_media_worker(&account.id, &id);
                n += 1;
            }
        }
        Ok(n)
    }
    fn spawn_media_worker(self: &Arc<Self>, account: &str, id: &str) {
        let core = self.clone();
        let account = account.to_string();
        let id = id.to_string();
        tokio::spawn(async move {
            let result = core.clone().run_media_worker(&account, &id).await;
            if result.is_err() {
                let _ = core.fail_media_job(&account, &id, "worker_interrupted");
            }
        });
    }
    pub fn media_assets(&self, account: &str, message_id: &str) -> Result<Vec<MediaAsset>> {
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        let msg = reader.message(&source_message_id(message_id)?, own.as_deref())?;
        if !matches!(msg.message_type, 3 | 34 | 43 | 47 | 49) {
            return Ok(vec![]);
        }
        let content = msg
            .content
            .text
            .as_deref()
            .context("media_metadata_unavailable")?;
        let quoted_image = crate::quotes::quoted_image_content(&msg);
        let mut quoted = msg.clone();
        let mut plans = if let Some(content) = &quoted_image {
            quoted.message_type = 3;
            discover(&quoted, content)?
        } else {
            discover(&msg, content)?
        };
        if msg.message_type == 34 {
            if let Some(server) = &msg.server_message_id {
                let cached = msg.peer.as_ref().and_then(|chat| {
                    let shards = reader
                        .schemas()
                        .iter()
                        .filter(|schema| schema.database.rsplit('/').next() == Some("media_0.db"))
                        .collect::<Vec<_>>();
                    if shards.len() == 1 {
                        reader
                            .cached_voice(&shards[0].database, chat, &msg.id.local_id, Some(server))
                            .ok()
                            .map(|cached| cached.bytes)
                    } else {
                        None
                    }
                });
                let inline = match cached {
                    Some(bytes) => Some(bytes),
                    None => self.native_inline(account, &msg)?,
                };
                for (_, _, plan, _) in &mut plans {
                    if let DownloadPlan::Voice {
                        server_id,
                        inline: target,
                        ..
                    } = plan
                    {
                        *server_id = server.parse()?;
                        *target = inline.clone();
                    }
                }
            }
        }
        let mut out = vec![];
        for (kind, variant, plan, filename) in plans {
            let identity = digest(&serde_json::to_vec(&(
                account, message_id, variant, content,
            ))?);
            let id = format!("asset_{}", &identity[..40]);
            let expires = match &plan {
                DownloadPlan::Cdn { descriptor } => descriptor.expires_at,
                DownloadPlan::Sticker { descriptor } => descriptor.expires_at,
                _ => None,
            };
            let mut asset = if let Ok(existing) = self.store.media_asset(account, &id) {
                serde_json::from_value(existing.metadata)?
            } else {
                let asset = MediaAsset {
                    id: id.clone(),
                    account_id: account.into(),
                    message_id: Some(message_id.into()),
                    kind,
                    variant,
                    actual_variant: None,
                    state: "pending".into(),
                    content_type: None,
                    byte_length: None,
                    sha256: None,
                    filename,
                    expires_at: expires,
                    reason: None,
                    retryable: true,
                    job_id: None,
                    content_url: None,
                    animation_frames: None,
                    preview: None,
                    official_writeback: if quoted_image.is_none()
                        && matches!(
                            kind,
                            MediaKind::Voice
                                | MediaKind::Image
                                | MediaKind::File
                                | MediaKind::Video
                        ) {
                        "pending"
                    } else {
                        "unsupported"
                    }
                    .into(),
                };
                let sealed =
                    self.vault
                        .seal(account, &format!("plan:{id}"), &serde_json::to_vec(&plan)?)?;
                self.store.put_media_asset(
                    account,
                    &MediaAssetRecord {
                        id: id.clone(),
                        message_id: Some(message_id.into()),
                        variant: serde_json::to_value(variant)?.as_str().unwrap().into(),
                        kind: serde_json::to_value(kind)?.as_str().unwrap().into(),
                        metadata: serde_json::to_value(&asset)?,
                        plan: sealed,
                        payload: None,
                    },
                )?;
                asset
            };
            if asset.state != "available" && kind == MediaKind::Image && quoted_image.is_none() {
                let q = match variant {
                    MediaVariant::Original => wechat_profile::ImageQuality::Original,
                    MediaVariant::Thumbnail => wechat_profile::ImageQuality::Thumbnail,
                    _ => wechat_profile::ImageQuality::Standard,
                };
                if let Ok(cached) = reader.cached_image(&msg.id, q) {
                    asset = self.cache_asset(
                        account,
                        &id,
                        cached.media.bytes,
                        &cached.media.media_type,
                        match cached.quality {
                            wechat_profile::VerifiedImageQuality::Original => {
                                Some(MediaVariant::Original)
                            }
                            wechat_profile::VerifiedImageQuality::Standard => {
                                Some(MediaVariant::Standard)
                            }
                            wechat_profile::VerifiedImageQuality::Thumbnail => {
                                Some(MediaVariant::Thumbnail)
                            }
                            wechat_profile::VerifiedImageQuality::Unknown => None,
                        },
                        None,
                    )?;
                }
            }
            if asset.state != "available" && matches!(kind, MediaKind::File | MediaKind::Video) {
                if let DownloadPlan::Cdn { descriptor } = &plan {
                    if let Some(md5) = descriptor.expected_md5.as_deref() {
                        let cache_kind = if kind == MediaKind::File {
                            wechat_profile::HardlinkKind::File
                        } else {
                            wechat_profile::HardlinkKind::Video
                        };
                        if let Ok(cached) = reader.cached_hardlink_file(cache_kind, md5) {
                            if let Ok((mime, frames)) =
                                validate_asset_bytes(kind, variant, &cached.bytes)
                            {
                                asset = self.cache_asset(
                                    account,
                                    &id,
                                    cached.bytes,
                                    &mime,
                                    Some(variant),
                                    frames,
                                )?;
                            }
                        }
                    }
                }
            }
            if asset.state != "available"
                && expires.is_some_and(|t| {
                    t <= wechat_protocol::wire::unix_time().unwrap_or(u32::MAX) as u64
                })
            {
                asset.state = "expired".into();
                asset.reason = Some("media_expired".into());
                asset.retryable = false;
                self.store
                    .save_media_asset(account, &id, &serde_json::to_value(&asset)?, None)?;
            }
            if asset.state == "available" && asset.official_writeback != "complete" {
                asset = self.repair_cached_asset(account, asset)?;
            }
            out.push(asset);
        }
        Ok(out)
    }
    fn native_inline(
        &self,
        account: &str,
        msg: &wechat_profile::Message,
    ) -> Result<Option<Vec<u8>>> {
        let Some(server) = &msg.server_message_id else {
            return Ok(None);
        };
        let Some(own) = self.profile_username(account)? else {
            return Ok(None);
        };
        for (id, sealed) in self.store.media_sync_receipts(account)? {
            let plain = self
                .vault
                .open_record(account, &format!("apply:{id}"), &sealed)?;
            let page: wechat_protocol::SyncBatch = serde_json::from_slice(&plain)?;
            for command in page.commands {
                if command.id != 5 {
                    continue;
                }
                let Ok(m) = wechat_protocol::wire::decode_add_message(&command.payload, &own)
                else {
                    continue;
                };
                if m.server_message_id.as_ref() == Some(server)
                    && Some(&m.peer) == msg.peer.as_ref()
                    && m.message_type as i64 == msg.message_type
                    && m.created_at as i64 == msg.create_time
                    && !m.inline_data.is_empty()
                {
                    return Ok(Some(m.inline_data));
                }
            }
        }
        Ok(None)
    }
    pub fn start_media_transfer(
        self: &Arc<Self>,
        account: &str,
        request: MediaTransferRequest,
    ) -> Result<MediaJob> {
        let assets = self.media_assets(account, &request.message_id)?;
        let asset = if let Some(id) = &request.asset_id {
            assets
                .into_iter()
                .find(|a| &a.id == id)
                .context("media_asset_not_found")?
        } else {
            select_asset(assets, request.quality)?
        };
        ensure!(
            request.quality == MediaVariant::Best
                || (asset.variant == request.quality
                    && (asset.state != "available"
                        || asset.actual_variant == Some(request.quality))),
            "unsupported_media_variant"
        );
        if matches!(asset.state.as_str(), "expired" | "unsupported") {
            bail!(
                "{}",
                asset.reason.unwrap_or_else(|| "unsupported_media".into())
            );
        }
        let key = format!("download:{}", asset.id);
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let hash = digest(asset.id.as_bytes());
        let (row, created) = self.store.create_media_job(
            account,
            &id,
            &key,
            &hash,
            "download",
            &json!({"assetId":asset.id,"completedBytes":0}),
            b"",
        )?;
        if created && asset.state == "available" {
            let mut row = row;
            row.state = "available".into();
            row.phase = "completed".into();
            row.data["completedBytes"] = asset.byte_length.into();
            let saved = self.store.save_media_job(account, &row)?;
            return Ok(public_job(account, &saved));
        }
        if created {
            let mut metadata = asset;
            metadata.job_id = Some(row.id.clone());
            self.store.save_media_asset(
                account,
                &metadata.id,
                &serde_json::to_value(&metadata)?,
                None,
            )?;
            self.spawn_media_worker(account, &row.id);
        }
        Ok(public_job(account, &row))
    }
    pub fn send_media(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        request: MediaSendRequest,
    ) -> Result<MediaJob> {
        self.account_state(account)?;
        if self.store.send_by_key(account, key).is_ok() {
            bail!("idempotency_conflict");
        }
        ensure!(
            !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control),
            "invalid_media_idempotency_key"
        );
        ensure!(
            request.play_seconds.is_none_or(|n| n > 0 && n <= 3600),
            "invalid_video_duration"
        );
        ensure!(
            request.kind == MediaKind::Image || request.standard_source_id.is_none(),
            "unsupported_standard_variant"
        );
        ensure!(
            matches!(request.kind, MediaKind::Image | MediaKind::Video)
                || request.thumbnail_source_id.is_none(),
            "unsupported_thumbnail_variant"
        );
        validate_destination(&request.thread_id)?;
        ensure!(
            request.kind != MediaKind::Sticker,
            "sticker_send_unsupported"
        );
        let (source, _) = self.source_bytes(account, &request.source_id)?;
        let source_hash = |id: &Option<String>| -> Result<Option<String>> {
            id.as_ref()
                .map(|id| Ok(self.store.media_source(account, id)?.0.sha256))
                .transpose()
        };
        let hash = media_request_hash(
            &request,
            &source.sha256,
            source_hash(&request.standard_source_id)?.as_deref(),
            source_hash(&request.thumbnail_source_id)?.as_deref(),
            (request.kind == MediaKind::File).then_some(source.name.as_str()),
        )?;
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let plan = SendPlan {
            request: request.clone(),
            client_id: format!("aw_{}", uuid::Uuid::new_v4().simple()),
            created_at: wechat_protocol::wire::unix_time()?,
            material: None,
            uploaded_file: None,
            uploaded_image: None,
            uploaded_video: None,
            prepared_video_thumbnail_source_id: None,
            voice: vec![],
        };
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&plan)?,
        )?;
        let (row, new) = self.store.create_media_job(
            account,
            &id,
            &format!("send:{key}"),
            &hash,
            "send",
            &json!({"threadId":request.thread_id,"sourceId":request.source_id,"completedBytes":0}),
            &payload,
        )?;
        if new {
            self.spawn_media_worker(account, &row.id);
        }
        Ok(public_job(account, &row))
    }
    fn cache_asset(
        &self,
        account: &str,
        id: &str,
        bytes: Vec<u8>,
        mime: &str,
        actual: Option<MediaVariant>,
        frames: Option<u32>,
    ) -> Result<MediaAsset> {
        let mut asset = self.media_asset(account, id)?;
        asset.preview = if mime == "image/x-wxgf" {
            ensure!(
                matches!(asset.kind, MediaKind::Image | MediaKind::Sticker),
                "unsupported_media"
            );
            Some(self.cache_wxgf_preview(account, id, &bytes)?)
        } else {
            None
        };
        asset.state = "available".into();
        asset.content_type = Some(mime.into());
        asset.byte_length = Some(bytes.len() as u64);
        asset.sha256 = Some(digest(&bytes));
        asset.actual_variant = actual;
        asset.retryable = false;
        asset.reason = None;
        asset.content_url = Some(format!("/v1/accounts/{account}/media/{id}/content"));
        asset.animation_frames = if mime == "image/x-wxgf" { None } else { frames };
        if mime == "image/x-wxgf" {
            asset.filename = Some(format!("{id}.wxgf"));
        }
        let sealed = self.vault.seal(account, &format!("asset:{id}"), &bytes)?;
        self.store
            .save_media_asset(account, id, &serde_json::to_value(&asset)?, Some(&sealed))?;
        Ok(asset)
    }
    fn cache_wxgf_preview(
        &self,
        account: &str,
        asset_id: &str,
        bytes: &[u8],
    ) -> Result<MediaImagePreview> {
        let source_sha256 = digest(bytes);
        let id = wxgf_preview_source_id(&source_sha256);
        let source = if self.store.media_source(account, &id).is_ok() {
            self.source_bytes(account, &id)?.0
        } else {
            let decoded = wechat_media::wxgf_preview(bytes)?;
            ensure!(
                ["image/jpeg", "image/png"].contains(&decoded.inspection.content_type.as_str())
                    && !decoded.bytes.is_empty()
                    && decoded.bytes.len() <= 16 * 1024 * 1024,
                "image_decode_failed"
            );
            let extension = if decoded.inspection.content_type == "image/png" {
                "png"
            } else {
                "jpg"
            };
            let name = format!("{asset_id}.still.{extension}");
            let hash = digest(&decoded.bytes);
            let sealed = self
                .vault
                .seal(account, &format!("source:{id}"), &decoded.bytes)?;
            match self.store.create_media_source(
                account,
                &id,
                &name,
                &decoded.inspection.content_type,
                decoded.bytes.len() as u64,
                &hash,
                &sealed,
            ) {
                Ok(source) => source,
                // A concurrent discovery may already have published this exact
                // source's preview. Reuse only its authenticated owned bytes.
                Err(error) => self.source_bytes(account, &id).map_err(|_| error)?.0,
            }
        };
        ensure!(
            ["image/jpeg", "image/png"].contains(&source.media_type.as_str())
                && source.byte_length > 0
                && source.byte_length <= 16 * 1024 * 1024,
            "media_integrity_failed"
        );
        Ok(MediaImagePreview {
            source_sha256,
            content_type: source.media_type,
            byte_length: source.byte_length,
            sha256: source.sha256,
            filename: source.name,
            content_url: format!(
                "/v1/accounts/{account}/media/{asset_id}/content?representation=preview"
            ),
        })
    }
    fn media_network_suspended(&self, account: &str) -> Result<bool> {
        let disconnected =
            self.store.runtime(account, "desired")?.as_deref() == Some(b"disconnected");
        let auto_disabled = self.store.runtime(account, "auto-connect")?.as_deref() == Some(b"0")
            && self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                != Some(self.ownership_generation.as_bytes());
        Ok(disconnected || auto_disabled)
    }
    async fn run_media_worker(self: Arc<Self>, account: &str, id: &str) -> Result<()> {
        let _guard = self.lock_account(account).await?;
        if self.media_network_suspended(account)? {
            return Ok(());
        }
        let core = self.clone();
        let account = account.to_string();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            if !core.store.claim_media_job(&account, &id)? {
                return Ok(());
            }
            let row = core.store.media_job(&account, &id)?;
            let outcome = if row.kind == "download" {
                core.perform_download(&account, &id)
            } else {
                core.perform_media_send(&account, &id)
            };
            if let Err(error) = outcome {
                let mut row = core.store.media_job(&account, &id)?;
                if let Some((stage, code)) = service_diagnostic(&error) {
                    row.data["result"]["serviceStage"] = stage.into();
                    row.data["result"]["serviceCode"] = code.into();
                    core.store.save_media_job(&account, &row)?;
                }
                core.fail_media_job(&account, &id, &classify_error(&error))?;
            }
            Ok(())
        })
        .await
        .context("media_worker_stopped")?
    }
    fn progress_media(&self, account: &str, id: &str, done: u64, total: Option<u64>) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        ensure!(!row.cancel_requested, "media_cancelled");
        ensure!(
            self.store.runtime(account, "logged-out")?.is_none(),
            "auth_required"
        );
        ensure!(
            !self.media_network_suspended(account)?,
            "media_disconnected"
        );
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "media_worker_stopping"
        );
        row.data["completedBytes"] = done.into();
        row.data["totalBytes"] = total.into();
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    fn fail_media_job(&self, account: &str, id: &str, code: &str) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        if matches!(
            row.state.as_str(),
            "available" | "accepted" | "needs_review"
        ) {
            return Ok(());
        }
        let uncertain =
            row.kind == "send" && matches!(row.phase.as_str(), "uploading" | "submitting");
        row.state = if uncertain {
            "needs_review"
        } else if code == "media_cancelled" {
            "cancelled"
        } else if code == "media_expired" {
            "expired"
        } else if code.starts_with("unsupported") || code.ends_with("_unsupported") {
            "unsupported"
        } else {
            "failed"
        }
        .into();
        row.retryable = !uncertain
            && row.attempts < 3
            && matches!(
                code,
                "transport_unavailable"
                    | "media_worker_stopping"
                    | "worker_interrupted"
                    | "media_cancelled"
                    | "media_disconnected"
                    | "auth_required"
            );
        row.error_code = Some(code.into());
        self.store.save_media_job(account, &row)?;
        if row.kind == "send" {
            let _ = self.project_media_send_receipts(account, id);
        }
        Ok(())
    }
    fn phase_media(&self, account: &str, id: &str, phase: &str) -> Result<MediaJobRecord> {
        let mut row = self.store.media_job(account, id)?;
        ensure!(!row.cancel_requested, "media_cancelled");
        ensure!(
            self.store.runtime(account, "logged-out")?.is_none(),
            "auth_required"
        );
        ensure!(
            !self.media_network_suspended(account)?,
            "media_disconnected"
        );
        ensure!(
            !self.stopping.load(Ordering::Acquire),
            "media_worker_stopping"
        );
        row.phase = phase.into();
        self.store.save_media_job(account, &row)
    }
    fn perform_download(&self, account: &str, id: &str) -> Result<()> {
        let row = self.phase_media(account, id, "downloading")?;
        let asset_id = row.data["assetId"]
            .as_str()
            .context("media_asset_not_found")?;
        let stored = self.store.media_asset(account, asset_id)?;
        let asset: MediaAsset = serde_json::from_value(stored.metadata)?;
        if asset.state == "available" {
            return self.complete_download(account, id, &asset);
        }
        let plan: DownloadPlan = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("plan:{asset_id}"),
            &stored.plan,
        )?)?;
        let progress = |n, t| self.progress_media(account, id, n, t);
        let mut official_writeback = None;
        let (bytes, mime, actual, frames) = match plan {
            DownloadPlan::Cdn { descriptor } => {
                let mut state = self.account_state(account)?;
                let native = NativeClient::new(NativeOptions::default())?;
                let conf = match native.cdn_config(&mut state) {
                    Ok(conf) => conf,
                    Err(error) if is_media_auth_rejected(&error) => {
                        state = native.renew_session(&state, "agent-wechat media")?;
                        native.cdn_config(&mut state)?
                    }
                    Err(error) => return Err(error),
                };
                self.save_state(account, &state)?;
                let cdn = CdnClient::new(conf, Duration::from_secs(25))?;
                let media = cdn.download_with_progress(&descriptor, progress)?;
                let (mime, frames) = validate_asset_bytes(asset.kind, asset.variant, &media.bytes)?;
                let actual = if media
                    .substituted_filetype
                    .is_none_or(|v| v == descriptor.quality.filetype())
                {
                    Some(asset.variant)
                } else {
                    None
                };
                ensure!(actual == Some(asset.variant), "unsupported_media_variant");
                official_writeback = if asset.official_writeback == "unsupported" {
                    None
                } else {
                    match asset.kind {
                        MediaKind::Image => {
                            Some(self.project_received_image(account, id, &asset, &media.bytes))
                        }
                        MediaKind::File | MediaKind::Video => {
                            Some(self.project_received_hardlink(account, id, &asset, &media.bytes))
                        }
                        _ => None,
                    }
                };
                (media.bytes, mime, actual, frames)
            }
            DownloadPlan::Sticker { descriptor } => {
                let (bytes, inspection) =
                    wechat_protocol::media::fetch_sticker(&descriptor, progress)?;
                (
                    bytes,
                    inspection.content_type,
                    Some(MediaVariant::Original),
                    Some(inspection.frames),
                )
            }
            DownloadPlan::Voice {
                server_id,
                length,
                inline,
                as_wav,
            } => {
                let mut bytes = inline
                    .filter(|v| v.len() == length as usize)
                    .unwrap_or_default();
                if bytes.is_empty() {
                    let mut state = self.account_state(account)?;
                    let native = NativeClient::new(NativeOptions::default())?;
                    let mut renewed = false;
                    while bytes.len() < length as usize {
                        progress(bytes.len() as u64, Some(length as u64))?;
                        let reply = native.download_voice_chunk(
                            &mut state,
                            server_id,
                            length,
                            bytes.len() as u32,
                        );
                        self.save_state(account, &state)?;
                        let chunk = match reply {
                            Ok(chunk) => chunk,
                            Err(error) if !renewed && is_media_auth_rejected(&error) => {
                                state = native.renew_session(&state, "agent-wechat media")?;
                                self.save_state(account, &state)?;
                                renewed = true;
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                        ensure!(chunk.offset_matches, "media_integrity_failed");
                        bytes.extend(chunk.bytes);
                    }
                    self.save_state(account, &state)?;
                }
                let pcm = wechat_protocol::silk::decode(&bytes)?;
                official_writeback = Some(self.project_received_voice(account, id, &asset, &bytes));
                if as_wav {
                    (
                        wechat_protocol::silk::wav(&pcm)?,
                        "audio/wav".into(),
                        Some(MediaVariant::Normal),
                        None,
                    )
                } else {
                    (
                        bytes,
                        "audio/silk".into(),
                        Some(MediaVariant::Encoded),
                        None,
                    )
                }
            }
            DownloadPlan::CachedOnly => bail!("unsupported_media_transport"),
        };
        self.progress_media(account, id, bytes.len() as u64, Some(bytes.len() as u64))?;
        let mut asset = self.cache_asset(account, asset_id, bytes, &mime, actual, frames)?;
        if let Some(status) = official_writeback {
            asset.official_writeback = status.into();
            self.store
                .save_media_asset(account, asset_id, &serde_json::to_value(&asset)?, None)?;
        }
        self.complete_download(account, id, &asset)
    }
    /// Repair only bytes and receipts already owned by this account. This method
    /// has no transport path and is safe to call after explicit logout. The
    /// caller holds the account mutation guard for this bounded batch.
    pub fn repair_media_writeback(&self, account: &str, limit: usize) -> Result<serde_json::Value> {
        ensure!((1..=100).contains(&limit), "invalid_limit");
        let mut candidates = self.store.media_writeback_candidates(account, limit + 1)?;
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        let mut repaired = 0;
        let mut blocked = 0;
        for (family, id) in &candidates {
            let result = if family == "asset" {
                self.media_asset(account, id)
                    .and_then(|asset| self.repair_cached_asset(account, asset))
                    .map(|asset| asset.official_writeback == "complete")
            } else {
                self.project_media_send_receipts(account, id)
                    .and_then(|()| self.store.media_job(account, id))
                    .map(|job| job.data["result"]["officialMediaWriteback"] == "complete")
            };
            let complete = result.unwrap_or(false);
            self.store
                .record_media_writeback_scan(account, family, id, complete)?;
            if complete {
                repaired += 1;
            } else {
                blocked += 1;
            }
        }
        Ok(
            json!({"scanned":candidates.len(),"repaired":repaired,"blocked":blocked,"hasMore":has_more}),
        )
    }
    fn repair_cached_asset(&self, account: &str, mut asset: MediaAsset) -> Result<MediaAsset> {
        if asset.state != "available"
            || ["complete", "unsupported"].contains(&asset.official_writeback.as_str())
        {
            return Ok(asset);
        }
        if asset.actual_variant != Some(asset.variant) {
            return Ok(asset);
        }
        let record = self.store.media_asset(account, &asset.id)?;
        let Some(sealed) = record.payload else {
            return Ok(asset);
        };
        let bytes = self
            .vault
            .open_record(account, &format!("asset:{}", asset.id), &sealed)?;
        ensure!(
            asset.byte_length == Some(bytes.len() as u64)
                && asset.sha256.as_deref() == Some(digest(&bytes).as_str()),
            "media_integrity_failed"
        );
        let receipt = format!("media-cache-repair:{}", asset.id);
        let status = match asset.kind {
            MediaKind::Image => self.project_received_image(account, &receipt, &asset, &bytes),
            MediaKind::File | MediaKind::Video => {
                self.project_received_hardlink(account, &receipt, &asset, &bytes)
            }
            MediaKind::Voice if asset.variant == MediaVariant::Encoded => {
                self.project_received_voice(account, &receipt, &asset, &bytes)
            }
            MediaKind::Voice if asset.variant == MediaVariant::Normal => {
                let encoded = (|| -> Result<Vec<u8>> {
                    let source = source_message_id(
                        asset
                            .message_id
                            .as_deref()
                            .context("media_message_missing")?,
                    )?;
                    let reader = self.profile_reader(account)?;
                    let own = self.profile_username(account)?;
                    let message = reader.message(&source, own.as_deref())?;
                    let chat = message.peer.as_deref().context("media_message_missing")?;
                    let server = message
                        .server_message_id
                        .as_deref()
                        .context("media_message_missing")?;
                    let shards = reader
                        .schemas()
                        .iter()
                        .filter(|schema| schema.database.rsplit('/').next() == Some("media_0.db"))
                        .collect::<Vec<_>>();
                    ensure!(shards.len() == 1, "unsupported_media_profile");
                    let cached = reader
                        .cached_voice(&shards[0].database, chat, &source.local_id, Some(server))
                        .ok()
                        .map(|cached| cached.bytes);
                    let encoded = match cached {
                        Some(encoded) => encoded,
                        None => self
                            .native_inline(account, &message)?
                            .context("media_encoded_voice_missing")?,
                    };
                    let wav =
                        wechat_protocol::silk::wav(&wechat_protocol::silk::decode(&encoded)?)?;
                    ensure!(wav.as_slice() == bytes.as_slice(), "media_integrity_failed");
                    Ok(encoded)
                })();
                match encoded {
                    Ok(encoded) => self.project_received_voice(account, &receipt, &asset, &encoded),
                    Err(_) => "blocked",
                }
            }
            _ => return Ok(asset),
        };
        asset.official_writeback = status.into();
        self.store
            .save_media_asset(account, &asset.id, &serde_json::to_value(&asset)?, None)?;
        Ok(asset)
    }
    fn project_received_hardlink(
        &self,
        account: &str,
        job: &str,
        asset: &MediaAsset,
        bytes: &[u8],
    ) -> &'static str {
        let attempt = || -> Result<()> {
            let source = source_message_id(
                asset
                    .message_id
                    .as_deref()
                    .context("media_message_missing")?,
            )?;
            let reader = self.profile_reader(account)?;
            let own = self.profile_username(account)?;
            let message = reader.message(&source, own.as_deref())?;
            let chat = message.peer.context("media_message_missing")?;
            let options = self.write_options(account, job)?;
            let (location, keys) = self.profile_location_keys(account)?;
            match asset.kind {
                MediaKind::File => {
                    wechat_profile::cache_file_projection(
                        &location, &keys, &source, &chat, bytes, &options,
                    )?;
                }
                MediaKind::Video => {
                    let quality = match asset.variant {
                        MediaVariant::Normal => wechat_profile::VideoQuality::Normal,
                        MediaVariant::Original => wechat_profile::VideoQuality::Original,
                        MediaVariant::Thumbnail => wechat_profile::VideoQuality::Thumbnail,
                        _ => bail!("unsupported_media_variant"),
                    };
                    wechat_profile::cache_video_projection(
                        &location, &keys, &source, &chat, quality, bytes, &options,
                    )?;
                }
                _ => bail!("unsupported_media_variant"),
            };
            Ok(())
        };
        if attempt().is_ok() {
            "complete"
        } else {
            "blocked"
        }
    }
    fn project_received_image(
        &self,
        account: &str,
        job: &str,
        asset: &MediaAsset,
        bytes: &[u8],
    ) -> &'static str {
        let attempt = || -> Result<()> {
            let source = source_message_id(
                asset
                    .message_id
                    .as_deref()
                    .context("media_message_missing")?,
            )?;
            let reader = self.profile_reader(account)?;
            let own = self.profile_username(account)?;
            let message = reader.message(&source, own.as_deref())?;
            let chat = message.peer.context("media_message_missing")?;
            let quality = match asset.variant {
                MediaVariant::Original => wechat_profile::ImageQuality::Original,
                MediaVariant::Standard => wechat_profile::ImageQuality::Standard,
                MediaVariant::Thumbnail => wechat_profile::ImageQuality::Thumbnail,
                _ => bail!("unsupported_media_variant"),
            };
            let options = self.write_options(account, job)?;
            let (location, keys) = self.profile_location_keys(account)?;
            wechat_profile::cache_image_projection(
                &location, &keys, &source, &chat, quality, bytes, &options,
            )?;
            Ok(())
        };
        if attempt().is_ok() {
            "complete"
        } else {
            "blocked"
        }
    }
    fn project_received_voice(
        &self,
        account: &str,
        job: &str,
        asset: &MediaAsset,
        encoded: &[u8],
    ) -> &'static str {
        let attempt = || -> Result<()> {
            let source = source_message_id(
                asset
                    .message_id
                    .as_deref()
                    .context("media_message_missing")?,
            )?;
            let reader = self.profile_reader(account)?;
            let own = self.profile_username(account)?;
            let message = reader.message(&source, own.as_deref())?;
            let chat = message.peer.context("media_message_missing")?;
            let options = self.write_options(account, job)?;
            let (location, keys) = self.profile_location_keys(account)?;
            wechat_profile::cache_voice_projection(
                &location, &keys, &source, &chat, encoded, &options,
            )?;
            Ok(())
        };
        if attempt().is_ok() {
            "complete"
        } else {
            "blocked"
        }
    }
    fn complete_download(&self, account: &str, id: &str, asset: &MediaAsset) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        row.data["completedBytes"] = asset.byte_length.into();
        row.data["totalBytes"] = asset.byte_length.into();
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
}

// Only a whole numeric suffix on these fixed library messages may cross the
// protected boundary. General error strings can contain URLs or account data.
fn service_diagnostic(error: &anyhow::Error) -> Option<(&'static str, i32)> {
    for cause in error.chain() {
        let message = cause.to_string();
        for (prefix, stage) in [
            ("CDN bootstrap returned ", "bootstrap"),
            ("CDN download returned ", "download"),
            ("CDN upload returned ", "upload"),
            ("CDN HTTP status ", "http"),
            ("sticker_http_status_", "http"),
            ("Account authentication returned ", "authentication"),
            ("Automatic authentication returned ", "authentication"),
            ("Voice service returned ", "voice"),
            ("Native envelope returned ", "native_envelope"),
        ] {
            if let Some(value) = message.strip_prefix(prefix) {
                if let Ok(code) = value.parse::<i32>() {
                    return Some((stage, code));
                }
            }
        }
    }
    None
}
fn is_media_auth_rejected(error: &anyhow::Error) -> bool {
    matches!(
        service_diagnostic(error),
        Some((
            "bootstrap" | "authentication" | "voice" | "native_envelope",
            -13
        ))
    )
}
fn classify_error(error: &anyhow::Error) -> String {
    if let Some((stage, code)) = service_diagnostic(error) {
        if is_media_auth_rejected(error) {
            return "auth_required".into();
        }
        if stage == "http" && (500..600).contains(&code) {
            return "transport_unavailable".into();
        }
        return "media_service_rejected".into();
    }
    for cause in error.chain() {
        let message = cause.to_string();
        if [
            "image_size_limit",
            "image_decode_limit",
            "image_decode_failed",
            "image_no_frames",
            "jpeg_trailer_missing",
            "png_trailer_missing",
            "gif_trailer_missing",
            "webp_length_invalid",
            "wxgf_invalid",
            "wxgf_partition_missing",
            "wxgf_partition_limit",
            "wxgf_decode_failed",
            "wxgf_decoder_unavailable",
            "wxgf_decode_timeout",
            "wxgf_preview_limit",
        ]
        .contains(&message.as_str())
        {
            return message;
        }
    }
    let message = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for code in [
        "media_cancelled",
        "media_disconnected",
        "auth_required",
        "media_expired",
        "media_worker_stopping",
        "media_integrity_failed",
        "source_integrity_failed",
        "unsupported_media_transport",
        "voice_duration_limit",
        "media_encoder_unavailable",
        "sticker_source_not_allowed",
        "sticker_source_dns_not_public",
    ] {
        if message.contains(code) {
            return code.into();
        }
    }
    if message.contains("checksum") || message.contains("length mismatch") {
        return "media_integrity_failed".into();
    }
    if message.contains("unsupported") || message.contains("Unsupported") {
        return "unsupported_media".into();
    }
    if message.contains("timed out")
        || message.contains("connection")
        || message.contains("fetch_failed")
        || message.contains("dns_failed")
        || message.contains("error sending request")
        || message.contains("incomplete message")
        || message.contains("unexpected end of file")
        || message.contains("HTTP status 5")
    {
        return "transport_unavailable".into();
    }
    "media_processing_failed".into()
}
fn validate_destination(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && !value.chars().any(|c| c.is_control() || c.is_whitespace()),
        "invalid_send_destination"
    );
    if let Some(group) = value.strip_suffix("@chatroom") {
        ensure!(
            !group.is_empty() && !group.contains('@'),
            "invalid_send_destination"
        );
    }
    Ok(())
}
fn select_asset(mut assets: Vec<MediaAsset>, quality: MediaVariant) -> Result<MediaAsset> {
    if quality != MediaVariant::Best {
        return assets
            .into_iter()
            .find(|a| a.variant == quality)
            .context("unsupported_media_variant");
    }
    if let Some(i) = assets.iter().position(|a| a.state == "available") {
        return Ok(assets.remove(i));
    }
    for variant in [
        MediaVariant::Original,
        MediaVariant::Normal,
        MediaVariant::Standard,
        MediaVariant::Thumbnail,
        MediaVariant::Encoded,
    ] {
        if let Some(i) = assets.iter().position(|a| a.variant == variant) {
            return Ok(assets.remove(i));
        }
    }
    bail!("unsupported_media")
}
fn validate_asset_bytes(
    kind: MediaKind,
    variant: MediaVariant,
    bytes: &[u8],
) -> Result<(String, Option<u32>)> {
    if kind == MediaKind::Video && variant == MediaVariant::Thumbnail {
        let result = wechat_protocol::media::inspect_image(bytes)?;
        ensure!(
            result.content_type == "image/jpeg",
            "media_integrity_failed"
        );
        return Ok((result.content_type, Some(result.frames)));
    }
    validate_media_bytes(kind, bytes)
}
fn validate_media_bytes(kind: MediaKind, bytes: &[u8]) -> Result<(String, Option<u32>)> {
    match kind {
        MediaKind::Image | MediaKind::Sticker => {
            let result = wechat_protocol::media::inspect_image(bytes)?;
            let frames = (result.content_type != "image/x-wxgf").then_some(result.frames);
            Ok((result.content_type, frames))
        }
        MediaKind::Video => {
            ensure!(
                bytes.len() >= 12 && bytes[4..8] == *b"ftyp",
                "media_integrity_failed"
            );
            Ok(("video/mp4".into(), None))
        }
        MediaKind::File => Ok(("application/octet-stream".into(), None)),
        MediaKind::Voice => {
            wechat_protocol::silk::decode(bytes)?;
            Ok(("audio/silk".into(), None))
        }
    }
}

type DiscoveredMedia = (MediaKind, MediaVariant, DownloadPlan, Option<String>);
fn discover(msg: &wechat_profile::Message, content: &str) -> Result<Vec<DiscoveredMedia>> {
    ensure!(content.len() <= 1024 * 1024, "media_metadata_limit");
    let mut xml = content.trim_start();
    if !xml.starts_with('<') && msg.peer.as_ref().is_some_and(|p| p.ends_with("@chatroom")) {
        if let Some((prefix, rest)) = xml.split_once(":\n") {
            ensure!(
                !prefix.is_empty()
                    && prefix.len() <= 128
                    && prefix
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(&b)),
                "media_metadata_invalid"
            );
            xml = rest;
        }
    }
    ensure!(
        !xml.to_ascii_uppercase().contains("<!DOCTYPE")
            && !xml.to_ascii_uppercase().contains("<!ENTITY"),
        "media_metadata_invalid"
    );
    let doc =
        roxmltree::Document::parse(xml).map_err(|_| anyhow::anyhow!("media_metadata_invalid"))?;
    ensure!(doc.descendants().count() <= 4096, "media_metadata_limit");
    let root = doc.root_element();
    let element = |name: &str| -> Result<roxmltree::Node<'_, '_>> {
        let list = root
            .descendants()
            .filter(|n| {
                n.is_element() && n.tag_name().name() == name && n.tag_name().namespace().is_none()
            })
            .collect::<Vec<_>>();
        ensure!(list.len() == 1, "media_metadata_invalid");
        Ok(list[0])
    };
    let number = |v: Option<&str>| {
        v.filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let key = |v: Option<&str>| -> Result<Vec<u8>> {
        let k = hex_decode(v.context("media_key_missing")?)?;
        ensure!(k.len() == 16, "media_metadata_invalid");
        Ok(k)
    };
    let mut out = vec![];
    match msg.message_type {
        3 => {
            let n = element("img")?;
            let expiry =
                number(n.attribute("expiretime")).or_else(|| number(n.attribute("expire")));
            for (variant, url, aes, size, quality) in [
                (
                    MediaVariant::Original,
                    "cdnbigimgurl",
                    "aeskey",
                    "hdlength",
                    MediaQuality::Original,
                ),
                (
                    MediaVariant::Standard,
                    "cdnmidimgurl",
                    "aeskey",
                    "length",
                    MediaQuality::Standard,
                ),
                (
                    MediaVariant::Thumbnail,
                    "cdnthumburl",
                    "cdnthumbaeskey",
                    "cdnthumblength",
                    MediaQuality::Thumbnail,
                ),
            ] {
                if let Some(file_id) = n.attribute(url).filter(|v| !v.is_empty()) {
                    let bytes = number(n.attribute(size));
                    if bytes == Some(0) {
                        continue;
                    }
                    let md5 = if variant == MediaVariant::Original {
                        n.attribute("md5").map(str::to_string)
                    } else {
                        None
                    };
                    out.push((
                        MediaKind::Image,
                        variant,
                        DownloadPlan::Cdn {
                            descriptor: MediaDescriptor {
                                file_id: file_id.into(),
                                aes_key: key(n.attribute(aes))?,
                                quality,
                                expected_bytes: bytes,
                                expected_md5: md5,
                                strict_size: variant == MediaVariant::Original,
                                expires_at: expiry,
                            },
                        },
                        None,
                    ));
                }
            }
        }
        43 => {
            let n = element("videomsg")?;
            for (variant, url, aes, size, md5, quality) in [
                (
                    MediaVariant::Normal,
                    "cdnvideourl",
                    "aeskey",
                    "length",
                    "md5",
                    MediaQuality::Video,
                ),
                (
                    MediaVariant::Original,
                    "cdnrawvideourl",
                    "cdnrawvideoaeskey",
                    "rawlength",
                    "rawmd5",
                    MediaQuality::Video,
                ),
                (
                    MediaVariant::Thumbnail,
                    "cdnthumburl",
                    "cdnthumbaeskey",
                    "cdnthumblength",
                    "cdnthumbmd5",
                    MediaQuality::Thumbnail,
                ),
            ] {
                if let Some(id) = n.attribute(url).filter(|s| !s.is_empty()) {
                    out.push((
                        MediaKind::Video,
                        variant,
                        DownloadPlan::Cdn {
                            descriptor: MediaDescriptor {
                                file_id: id.into(),
                                aes_key: key(n.attribute(aes))?,
                                quality,
                                expected_bytes: number(n.attribute(size)),
                                expected_md5: n.attribute(md5).map(str::to_string),
                                strict_size: true,
                                expires_at: number(n.attribute("expiretime")),
                            },
                        },
                        Some(
                            if variant == MediaVariant::Original {
                                "original.mp4"
                            } else if variant == MediaVariant::Thumbnail {
                                "video-thumbnail.jpg"
                            } else {
                                "video.mp4"
                            }
                            .into(),
                        ),
                    ));
                }
            }
        }
        49 => {
            let app = element("appmsg")?;
            let child = |name: &str| {
                app.children()
                    .find(|n| n.is_element() && n.tag_name().name() == name)
            };
            ensure!(
                child("type").and_then(|n| n.text()) == Some("6"),
                "unsupported_application_media"
            );
            let attach = child("appattach").context("media_metadata_invalid")?;
            let value = |name: &str| {
                attach
                    .children()
                    .find(|n| n.is_element() && n.tag_name().name() == name)
                    .and_then(|n| n.text())
            };
            let encryption = match attach
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "encryver")
            {
                None => 1,
                Some(version) => number(version.text()).context("media_metadata_invalid")?,
            };
            // Both observed versions use the supplied AES key and file CDN
            // transport. Version 0 files still require full size/digest checks.
            ensure!(
                matches!(encryption, 0 | 1),
                "unsupported_attachment_encryption"
            );
            let filename = child("title")
                .and_then(|n| n.text())
                .filter(|s| {
                    s.len() <= 255 && !s.contains(['/', '\\']) && !s.chars().any(|c| c.is_control())
                })
                .map(str::to_string);
            out.push((
                MediaKind::File,
                MediaVariant::Original,
                DownloadPlan::Cdn {
                    descriptor: MediaDescriptor {
                        file_id: value("cdnattachurl")
                            .context("unsupported_attachment_transport")?
                            .into(),
                        aes_key: key(value("aeskey"))?,
                        quality: MediaQuality::File,
                        expected_bytes: number(value("totallen")),
                        expected_md5: child("md5").and_then(|n| n.text()).map(str::to_string),
                        strict_size: true,
                        expires_at: number(value("expiretime")),
                    },
                },
                filename,
            ));
        }
        34 => {
            let n = element("voicemsg")?;
            let length = u32::try_from(
                number(n.attribute("length")).context("media_voice_length_missing")?,
            )?;
            ensure!(
                length > 0 && length <= 16 * 1024 * 1024,
                "unsupported_voice_size"
            );
            let server_id = msg
                .server_message_id
                .as_ref()
                .context("media_server_id_missing")?
                .parse()?;
            for (variant, as_wav) in [(MediaVariant::Normal, true), (MediaVariant::Encoded, false)]
            {
                out.push((
                    MediaKind::Voice,
                    variant,
                    DownloadPlan::Voice {
                        server_id,
                        length,
                        inline: None,
                        as_wav,
                    },
                    None,
                ));
            }
        }
        47 => {
            let n = element("emoji")?;
            let length = number(n.attribute("len")).context("media_metadata_invalid")?;
            let url = n
                .attribute("cdnurl")
                .context("unsupported_sticker_source")?;
            wechat_protocol::media::validate_sticker_url(url)?;
            out.push((
                MediaKind::Sticker,
                MediaVariant::Original,
                DownloadPlan::Sticker {
                    descriptor: StickerDescriptor {
                        url: url.into(),
                        expected_bytes: length,
                        expected_md5: n.attribute("md5").context("media_metadata_invalid")?.into(),
                        expires_at: number(n.attribute("expiretime")),
                    },
                },
                None,
            ));
        }
        _ => bail!("unsupported_media"),
    };
    ensure!(!out.is_empty(), "unsupported_media_transport");
    Ok(out)
}
fn hex_decode(s: &str) -> Result<Vec<u8>> {
    ensure!(
        s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "media_metadata_invalid"
    );
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}

impl Core {
    fn save_send_plan(&self, account: &str, id: &str, plan: &SendPlan) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(plan)?,
        )?;
        if plan.request.kind == MediaKind::Voice {
            row.data["result"]["chunks"] = json!(plan.voice.iter().enumerate().map(|(i,p)|json!({"index":i,"state":p.status,"durationMs":p.duration_ms,"serverMessageId":p.server_id})).collect::<Vec<_>>());
        }
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    fn perform_media_send(&self, account: &str, id: &str) -> Result<()> {
        let row = self.store.media_job(account, id)?;
        let mut plan: SendPlan = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        self.progress_media(account, id, 0, None)?;
        let (source, bytes) = self.source_bytes(account, &plan.request.source_id)?;
        let native = NativeClient::new(NativeOptions::default())?;
        let mut state = self.account_state(account)?;
        if plan.request.kind == MediaKind::Video && plan.request.play_seconds.is_none() {
            plan.request.play_seconds = Some(video_duration(&bytes, || {
                self.progress_media(account, id, 0, None)
            })?);
            self.save_send_plan(account, id, &plan)?;
        }
        if plan.request.kind == MediaKind::Voice {
            if plan.voice.is_empty() {
                let pcm = audio_pcm(&bytes, || self.progress_media(account, id, 0, None))?;
                ensure!(
                    !pcm.is_empty() && pcm.len() <= 300 * 16000,
                    "voice_duration_limit"
                );
                for (index, chunk) in pcm.chunks(10 * 16000).enumerate() {
                    let encoded = wechat_protocol::silk::encode(chunk)?;
                    ensure!(encoded.len() <= 48000, "unsupported_voice_chunk");
                    plan.voice.push(VoicePart {
                        client_id: format!("{}_{}", plan.client_id, index),
                        encoded,
                        duration_ms: ((chunk.len() as u64 * 1000) / 16000) as u32,
                        status: "prepared".into(),
                        server_id: None,
                    });
                }
                self.save_send_plan(account, id, &plan)?;
            }
            for index in 0..plan.voice.len() {
                if plan.voice[index].status == "accepted" {
                    continue;
                }
                self.progress_media(account, id, index as u64, Some(plan.voice.len() as u64))?;
                self.phase_media(account, id, "submitting")?;
                plan.voice[index].status = "submitting".into();
                self.save_send_plan(account, id, &plan)?;
                let part = &plan.voice[index];
                let result = native.send_voice(
                    &mut state,
                    &plan.request.thread_id,
                    &part.client_id,
                    plan.created_at,
                    part.duration_ms,
                    &part.encoded,
                );
                let receipt = match result {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        plan.voice[index].status = "needs_review".into();
                        self.save_send_plan(account, id, &plan)?;
                        let _ = self.save_state(account, &state);
                        return Err(error);
                    }
                };
                plan.voice[index].server_id = receipt.server_message_id.clone();
                plan.voice[index].status = match receipt.status {
                    wechat_protocol::media_send::SendStatus::ServerAccepted => "accepted",
                    wechat_protocol::media_send::SendStatus::Rejected => "rejected",
                    wechat_protocol::media_send::SendStatus::NeedsReview => "needs_review",
                }
                .into();
                // Preserve a known server result before any optional local state
                // or profile update can fail under a disconnect/logout fence.
                self.save_send_plan(account, id, &plan)?;
                self.save_state(account, &state)?;
                if receipt.status == wechat_protocol::media_send::SendStatus::ServerAccepted {
                    let _ = self.project_media_send_receipts(account, id);
                }
                if receipt.status != wechat_protocol::media_send::SendStatus::ServerAccepted {
                    return self.finish_media_send(account, id, receipt);
                }
                self.phase_media(account, id, "preparing")?;
            }
            let last = plan.voice.last().and_then(|p| p.server_id.clone());
            return self.finish_media_send(
                account,
                id,
                wechat_protocol::media_send::MediaSendResult {
                    status: wechat_protocol::media_send::SendStatus::ServerAccepted,
                    service_code: 0,
                    server_message_id: last,
                    client_id_matches: Some(true),
                },
            );
        }
        if plan.material.is_none() {
            plan.material = Some(UploadMaterial {
                key: wechat_protocol::crypto::random::<16>()?.to_vec(),
                file_key: hex_string(&wechat_protocol::crypto::random::<16>()?),
            });
            self.save_send_plan(account, id, &plan)?;
        }
        let material = plan.material.as_ref().unwrap();
        let mut conf = match native.cdn_config(&mut state) {
            Ok(conf) => conf,
            Err(error) if is_media_auth_rejected(&error) => {
                state = native.renew_session(&state, "agent-wechat media")?;
                native.cdn_config(&mut state)?
            }
            Err(e) => return Err(e),
        };
        self.save_state(account, &state)?;
        let cdn = CdnClient::new(conf.clone(), Duration::from_secs(25))?;
        conf.auth_key.fill(0);
        let callback = |n, t| self.progress_media(account, id, n, t);
        match plan.request.kind {
            MediaKind::File => {
                if plan.uploaded_file.is_none() {
                    let prepared = PreparedUpload::with_material(
                        &bytes,
                        MediaQuality::File,
                        &source.name,
                        &plan.request.thread_id,
                        &material.key,
                        &material.file_key,
                    )?;
                    self.phase_media(account, id, "uploading")?;
                    let uploaded = cdn.upload_with_progress(&prepared, callback)?;
                    plan.uploaded_file = Some(uploaded);
                    self.save_send_plan(account, id, &plan)?;
                }
            }
            MediaKind::Image => {
                if plan.uploaded_image.is_none() {
                    let standard = plan
                        .request
                        .standard_source_id
                        .as_ref()
                        .map(|s| self.source_bytes(account, s))
                        .transpose()?
                        .map(|v| v.1);
                    let thumb = plan
                        .request
                        .thumbnail_source_id
                        .as_ref()
                        .map(|s| self.source_bytes(account, s))
                        .transpose()?
                        .map(|v| v.1);
                    let (original, standard, thumbnail) = image_variants(&bytes, standard, thumb)?;
                    let inspection = wechat_protocol::media::inspect_image(&original)?;
                    let mut row = self.store.media_job(account, id)?;
                    row.data["result"]["imagePreparation"] = json!({"sourceSha256":source.sha256,"sentOriginalSha256":digest(&original),"sourcePreserved":original==bytes,"width":inspection.width,"height":inspection.height});
                    self.store.save_media_job(account, &row)?;
                    let prepared = PreparedImageUpload::with_material(
                        &original,
                        &standard,
                        &thumbnail,
                        &source.name,
                        &plan.request.thread_id,
                        &material.key,
                        &material.file_key,
                    )?;
                    self.phase_media(account, id, "uploading")?;
                    self.progress_media(account, id, 0, Some(original.len() as u64))?;
                    let uploaded = cdn.upload_image(&prepared)?;
                    plan.uploaded_image = Some(uploaded);
                    self.save_send_plan(account, id, &plan)?;
                }
            }
            MediaKind::Video => {
                if plan.uploaded_video.is_none() {
                    ensure!(
                        bytes.len() >= 12 && bytes[4..8] == *b"ftyp",
                        "unsupported_video_container"
                    );
                    let thumb = if let Some(id) = plan
                        .request
                        .thumbnail_source_id
                        .as_ref()
                        .or(plan.prepared_video_thumbnail_source_id.as_ref())
                    {
                        self.source_bytes(account, id)?.1
                    } else {
                        let bytes =
                            video_thumbnail(&bytes, || self.progress_media(account, id, 0, None))?;
                        let source = self.upload_source(
                            account,
                            "video-thumbnail.jpg",
                            "image/jpeg",
                            &bytes,
                        )?;
                        plan.prepared_video_thumbnail_source_id = Some(source.id);
                        // Retain the exact prepared bytes before any upload. Recovery
                        // never depends on regenerating a codec's thumbnail output.
                        self.save_send_plan(account, id, &plan)?;
                        bytes
                    };
                    let prepared = PreparedVideoUpload::with_material(
                        &bytes,
                        &thumb,
                        &source.name,
                        &plan.request.thread_id,
                        &material.key,
                        &material.file_key,
                    )?;
                    self.phase_media(account, id, "uploading")?;
                    let uploaded = cdn.upload_video_with_progress(&prepared, callback)?;
                    plan.uploaded_video = Some(uploaded);
                    self.save_send_plan(account, id, &plan)?;
                }
            }
            _ => bail!("unsupported_media_send"),
        }
        self.phase_media(account, id, "readback")?;
        let mut verified = vec![];
        if let Some(u) = &plan.uploaded_file {
            let downloaded = cdn.download_with_progress(&u.descriptor, callback)?;
            ensure!(downloaded.sha256 == u.sha256, "media_integrity_failed");
            verified.push(downloaded.sha256);
        }
        if let Some(u) = &plan.uploaded_image {
            for variant in [&u.original, &u.standard, &u.thumbnail] {
                let downloaded = cdn.download_with_progress(&variant.descriptor, callback)?;
                ensure!(
                    downloaded.sha256 == variant.sha256,
                    "media_integrity_failed"
                );
                wechat_protocol::media::inspect_image(&downloaded.bytes)?;
                verified.push(downloaded.sha256);
            }
        }
        if let Some(u) = &plan.uploaded_video {
            for variant in [&u.video, &u.thumbnail] {
                let downloaded = cdn.download_with_progress(&variant.descriptor, callback)?;
                ensure!(
                    downloaded.sha256 == variant.sha256,
                    "media_integrity_failed"
                );
                verified.push(downloaded.sha256);
            }
        }
        let mut row = self.store.media_job(account, id)?;
        row.data["result"]["uploadReadbackVerified"] = true.into();
        row.data["result"]["verifiedSha256"] = verified.into();
        self.store.save_media_job(account, &row)?;
        self.phase_media(account, id, "submitting")?;
        let result = match plan.request.kind {
            MediaKind::File => native.send_file(
                &mut state,
                &plan.request.thread_id,
                &plan.client_id,
                plan.created_at,
                plan.uploaded_file.as_ref().unwrap(),
            ),
            MediaKind::Image => native.send_image(
                &mut state,
                &plan.request.thread_id,
                &plan.client_id,
                plan.uploaded_image.as_ref().unwrap(),
            ),
            MediaKind::Video => native.send_video(
                &mut state,
                &plan.request.thread_id,
                &plan.client_id,
                plan.uploaded_video.as_ref().unwrap(),
                plan.request
                    .play_seconds
                    .filter(|s| *s > 0)
                    .context("video_duration_required")?,
            ),
            _ => bail!("unsupported_media_send"),
        };
        match result {
            Ok(receipt) => {
                self.finish_media_send(account, id, receipt)?;
                self.save_state(account, &state)
            }
            Err(error) => {
                let _ = self.save_state(account, &state);
                Err(error)
            }
        }
    }
    fn finish_media_send(
        &self,
        account: &str,
        id: &str,
        receipt: wechat_protocol::media_send::MediaSendResult,
    ) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.state = match receipt.status {
            wechat_protocol::media_send::SendStatus::ServerAccepted => "accepted",
            wechat_protocol::media_send::SendStatus::Rejected => "failed",
            wechat_protocol::media_send::SendStatus::NeedsReview => "needs_review",
        }
        .into();
        row.phase = "completed".into();
        row.retryable = false;
        row.data["serverMessageId"] = receipt.server_message_id.clone().into();
        row.data["result"]["nativeReceipt"] = serde_json::to_value(receipt)?;
        self.store.save_media_job(account, &row)?;
        if row.state == "accepted" && self.project_media_send_receipts(account, id).is_err() {
            let mut row = self.store.media_job(account, id)?;
            row.data["result"]["profileProjection"] = "pending".into();
            self.store.save_media_job(account, &row)?;
        }
        Ok(())
    }
}
impl Core {
    fn project_media_send_receipts(&self, account: &str, id: &str) -> Result<()> {
        let row = self.store.media_job(account, id)?;
        let plan: SendPlan = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        let own = self
            .profile_username(account)?
            .context("profile_identity_unavailable")?;
        let mut message_ids = vec![];
        let mut official_media_writeback = if plan.request.kind == MediaKind::Voice {
            "complete"
        } else {
            "unsupported"
        };
        let mut unknown = row.state == "needs_review";
        if plan.request.kind == MediaKind::Voice {
            for (index, part) in plan.voice.iter().enumerate() {
                if matches!(part.status.as_str(), "needs_review" | "submitting") {
                    unknown = true;
                }
                if part.status != "accepted" {
                    continue;
                }
                let server = part
                    .server_id
                    .as_deref()
                    .context("accepted_voice_identity_missing")?;
                let content = wechat_protocol::media_send::local_voice_xml(
                    part.encoded.len(),
                    part.duration_ms,
                )?;
                let receipt = format!("media-projection:{id}:{index}");
                let source = self.project_accepted_media(
                    account,
                    &plan.request.thread_id,
                    34,
                    &content,
                    "",
                    plan.created_at as u64,
                    server,
                    &receipt,
                )?;
                let options = self.write_options(account, &receipt)?;
                let (location, keys) = self.profile_location_keys(account)?;
                wechat_profile::cache_voice_projection(
                    &location,
                    &keys,
                    &source,
                    &plan.request.thread_id,
                    &part.encoded,
                    &options,
                )?;
                let message_id = crate::message_id(&source)?;
                for asset in self.media_assets(account, &message_id)? {
                    let (bytes, mime) = if asset.variant == MediaVariant::Encoded {
                        (part.encoded.clone(), "audio/silk")
                    } else {
                        (
                            wechat_protocol::silk::wav(&wechat_protocol::silk::decode(
                                &part.encoded,
                            )?)?,
                            "audio/wav",
                        )
                    };
                    let mut cached = self.cache_asset(
                        account,
                        &asset.id,
                        bytes,
                        mime,
                        Some(asset.variant),
                        None,
                    )?;
                    cached.official_writeback = "complete".into();
                    self.store.save_media_asset(
                        account,
                        &asset.id,
                        &serde_json::to_value(cached)?,
                        None,
                    )?;
                }
                message_ids.push(message_id);
            }
        } else if row.state == "accepted" {
            let server = row.data["serverMessageId"]
                .as_str()
                .context("accepted_media_identity_missing")?;
            let (source, bytes) = self.source_bytes(account, &plan.request.source_id)?;
            let (kind, content, summary) = match plan.request.kind {
                MediaKind::File => (
                    49,
                    wechat_protocol::media_send::local_file_xml(
                        &own,
                        plan.uploaded_file
                            .as_ref()
                            .context("uploaded_media_missing")?,
                    )?,
                    source.name.clone(),
                ),
                MediaKind::Image => (
                    3,
                    wechat_protocol::media_send::local_image_xml(
                        plan.uploaded_image
                            .as_ref()
                            .context("uploaded_media_missing")?,
                    )?,
                    String::new(),
                ),
                MediaKind::Video => (
                    43,
                    wechat_protocol::media_send::local_video_xml(
                        plan.uploaded_video
                            .as_ref()
                            .context("uploaded_media_missing")?,
                        plan.request
                            .play_seconds
                            .context("video_duration_required")?,
                    )?,
                    String::new(),
                ),
                _ => bail!("unsupported_media_projection"),
            };
            let target = self.project_accepted_media(
                account,
                &plan.request.thread_id,
                kind,
                &content,
                &summary,
                plan.created_at as u64,
                server,
                &format!("media-projection:{id}"),
            )?;
            let message_id = crate::message_id(&target)?;
            let image_bytes = if plan.request.kind == MediaKind::Image {
                let standard = plan
                    .request
                    .standard_source_id
                    .as_ref()
                    .map(|id| self.source_bytes(account, id).map(|v| v.1))
                    .transpose()?;
                let thumbnail = plan
                    .request
                    .thumbnail_source_id
                    .as_ref()
                    .map(|id| self.source_bytes(account, id).map(|v| v.1))
                    .transpose()?;
                Some(image_variants(&bytes, standard, thumbnail)?)
            } else {
                None
            };
            for asset in self.media_assets(account, &message_id)? {
                let (raw, expected, mime) = match plan.request.kind {
                    MediaKind::Image => {
                        let variants = image_bytes.as_ref().unwrap();
                        let uploaded = plan.uploaded_image.as_ref().unwrap();
                        match asset.variant {
                            MediaVariant::Original => (
                                variants.0.clone(),
                                uploaded.original.sha256.clone(),
                                "image/jpeg",
                            ),
                            MediaVariant::Standard => (
                                variants.1.clone(),
                                uploaded.standard.sha256.clone(),
                                "image/jpeg",
                            ),
                            MediaVariant::Thumbnail => (
                                variants.2.clone(),
                                uploaded.thumbnail.sha256.clone(),
                                "image/jpeg",
                            ),
                            _ => continue,
                        }
                    }
                    MediaKind::File => (
                        bytes.clone(),
                        plan.uploaded_file.as_ref().unwrap().sha256.clone(),
                        "application/octet-stream",
                    ),
                    MediaKind::Video => {
                        let uploaded = plan.uploaded_video.as_ref().unwrap();
                        if asset.variant == MediaVariant::Thumbnail {
                            let raw = if let Some(id) = plan
                                .request
                                .thumbnail_source_id
                                .as_ref()
                                .or(plan.prepared_video_thumbnail_source_id.as_ref())
                            {
                                self.source_bytes(account, id)?.1
                            } else {
                                // Compatibility for a pre-thumbnail-checkpoint job:
                                // accept regenerated bytes only on exact saved SHA.
                                video_thumbnail(&bytes, || Ok(()))?
                            };
                            (raw, uploaded.thumbnail.sha256.clone(), "image/jpeg")
                        } else if asset.variant == MediaVariant::Normal {
                            (bytes.clone(), uploaded.video.sha256.clone(), "video/mp4")
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };
                ensure!(digest(&raw) == expected, "media_integrity_failed");
                let status = match plan.request.kind {
                    MediaKind::Image => self.project_received_image(account, id, &asset, &raw),
                    MediaKind::File | MediaKind::Video => {
                        self.project_received_hardlink(account, id, &asset, &raw)
                    }
                    _ => "unsupported",
                };
                if status != "complete" || official_media_writeback == "unsupported" {
                    official_media_writeback = status;
                }
                let mut cached =
                    self.cache_asset(account, &asset.id, raw, mime, Some(asset.variant), None)?;
                cached.official_writeback = status.into();
                self.store.save_media_asset(
                    account,
                    &asset.id,
                    &serde_json::to_value(cached)?,
                    None,
                )?;
            }
            message_ids.push(message_id);
        }
        let mut row = self.store.media_job(account, id)?;
        row.data["result"]["profileProjection"] = if unknown {
            "unknown"
        } else if message_ids.is_empty() {
            "not_applicable"
        } else {
            "complete"
        }
        .into();
        row.data["result"]["messageIds"] = message_ids.into();
        row.data["result"]["officialMediaWriteback"] = official_media_writeback.into();
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
}
fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn image_variants(
    bytes: &[u8],
    standard: Option<Vec<u8>>,
    thumbnail: Option<Vec<u8>>,
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode()?;
    ensure!(
        decoded.width() as u64 * decoded.height() as u64 <= 16_777_216,
        "image_decode_limit"
    );
    let original =
        if bytes.len() <= 65520 && image::guess_format(bytes)? == image::ImageFormat::Jpeg {
            bytes.to_vec()
        } else {
            fit_jpeg(&decoded, 1280, 65520)?
        };
    let original_image = image::load_from_memory(&original)?;
    let supplied_jpeg = |raw: Vec<u8>| -> Result<Vec<u8>> {
        ensure!(
            raw.len() <= 65520 && image::guess_format(&raw)? == image::ImageFormat::Jpeg,
            "unsupported_image_variant"
        );
        wechat_protocol::media::inspect_image(&raw)?;
        Ok(raw)
    };
    let standard = match standard {
        Some(s) => supplied_jpeg(s)?,
        None => fit_jpeg(&original_image, 640, 65520)?,
    };
    let standard_image = image::load_from_memory(&standard)?;
    let thumbnail = match thumbnail {
        Some(s) => supplied_jpeg(s)?,
        None => fit_jpeg(&standard_image, 160, 65520)?,
    };
    Ok((original, standard, thumbnail))
}
fn fit_jpeg(image: &image::DynamicImage, max_edge: u32, max_bytes: usize) -> Result<Vec<u8>> {
    for edge in [max_edge, max_edge / 2, max_edge / 4, max_edge / 8] {
        let resized = image.thumbnail(edge.max(1), edge.max(1)).to_rgb8();
        for quality in [85, 70, 55, 40] {
            let mut bytes = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
                .encode_image(&resized)?;
            if bytes.len() <= max_bytes {
                return Ok(bytes);
            }
        }
    }
    bail!("unsupported_image_size")
}
fn run_ffmpeg(
    input: &[u8],
    output: &str,
    options: &[&str],
    mut cancelled: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let dir = tempfile::tempdir()?;
    let input_path = dir.path().join("input.bin");
    let output_path = dir.path().join(output);
    let mut f = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&input_path)?;
    f.write_all(input)?;
    drop(f);
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            "wav,mp3,flac,ogg,aac,mov,matroska,webm",
            "-threads",
            "1",
            "-i",
        ])
        .arg(&input_path)
        .args(options)
        .args(["-fs", "33554432"])
        .arg(&output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("media_encoder_unavailable"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Err(e) = cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "media_encoder_failed");
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("media_encoder_timeout");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let metadata = std::fs::metadata(&output_path)?;
    ensure!(
        metadata.len() > 0 && metadata.len() <= 32 * 1024 * 1024,
        "media_encoder_output_limit"
    );
    Ok(std::fs::read(output_path)?)
}
/// Narrow RIFF/WAVE fast path: one PCM16 mono16kHz data chunk, with bounded
/// unknown chunks and checked RIFF padding. Other valid formats use FFmpeg.
fn pcm16k_wav(bytes: &[u8]) -> Result<Option<Vec<i16>>> {
    if bytes.len() < 12 || bytes[..4] != *b"RIFF" || bytes[8..12] != *b"WAVE" {
        return Ok(None);
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
    ensure!(
        declared.checked_add(8) == Some(bytes.len()),
        "media_audio_invalid"
    );
    let mut at = 12usize;
    let mut format = None;
    let mut data = None;
    let mut chunks = 0;
    while at < bytes.len() {
        chunks += 1;
        ensure!(
            chunks <= 1024 && bytes.len() - at >= 8,
            "media_audio_invalid"
        );
        let kind = &bytes[at..at + 4];
        let length = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        at += 8;
        let end = at.checked_add(length).context("media_audio_invalid")?;
        ensure!(end <= bytes.len(), "media_audio_invalid");
        if kind == b"fmt " {
            ensure!(format.is_none() && length >= 16, "media_audio_invalid");
            let raw = &bytes[at..end];
            let u16_at = |offset| u16::from_le_bytes([raw[offset], raw[offset + 1]]);
            let u32_at = |offset| u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
            let supported = u16_at(0) == 1
                && u16_at(2) == 1
                && u32_at(4) == 16000
                && u32_at(8) == 32000
                && u16_at(12) == 2
                && u16_at(14) == 16
                && (length == 16 || (length == 18 && u16_at(16) == 0));
            format = Some(supported);
        } else if kind == b"data" {
            ensure!(data.is_none(), "media_audio_invalid");
            data = Some(&bytes[at..end]);
        }
        at = end.checked_add(length & 1).context("media_audio_invalid")?;
        ensure!(at <= bytes.len(), "media_audio_invalid");
    }
    let supported = format.context("media_audio_invalid")?;
    let raw = data.context("media_audio_invalid")?;
    if !supported {
        return Ok(None);
    }
    ensure!(
        !raw.is_empty() && raw.len().is_multiple_of(2) && raw.len() <= 300 * 16000 * 2,
        "voice_duration_limit"
    );
    Ok(Some(
        raw.chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect(),
    ))
}
fn audio_pcm(bytes: &[u8], cancelled: impl FnMut() -> Result<()>) -> Result<Vec<i16>> {
    if wechat_protocol::silk::validate_framing(bytes).is_ok() {
        return wechat_protocol::silk::decode(bytes);
    }
    if let Some(pcm) = pcm16k_wav(bytes)? {
        return Ok(pcm);
    }
    let raw = run_ffmpeg(
        bytes,
        "audio.pcm",
        &[
            "-vn", "-ac", "1", "-ar", "16000", "-f", "s16le", "-t", "301",
        ],
        cancelled,
    )?;
    ensure!(
        raw.len() % 2 == 0 && raw.len() <= 300 * 16000 * 2,
        "voice_duration_limit"
    );
    Ok(raw
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]))
        .collect())
}
fn video_duration(input: &[u8], mut cancelled: impl FnMut() -> Result<()>) -> Result<u32> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let dir = tempfile::tempdir()?;
    let input_path = dir.path().join("input.bin");
    let result_path = dir.path().join("probe.json");
    let mut source = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&input_path)?;
    source.write_all(input)?;
    drop(source);
    let result = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&result_path)?;
    let mut child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            "mov",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(&input_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(result))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| anyhow::anyhow!("media_encoder_unavailable"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Err(error) = cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if std::fs::metadata(&result_path)?.len() > 65536 {
            let _ = child.kill();
            let _ = child.wait();
            bail!("media_probe_output_limit");
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                ensure!(status.success(), "media_probe_failed");
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("media_encoder_timeout");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let bytes = std::fs::read(result_path)?;
    ensure!(bytes.len() <= 65536, "media_probe_output_limit");
    let result: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("media_probe_failed"))?;
    let duration = result["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .context("video_duration_required")?;
    ensure!(
        duration.is_finite() && duration > 0.0 && duration <= 3600.0,
        "video_duration_limit"
    );
    Ok(duration.ceil() as u32)
}
fn video_thumbnail(bytes: &[u8], cancelled: impl FnMut() -> Result<()>) -> Result<Vec<u8>> {
    let jpeg = run_ffmpeg(
        bytes,
        "thumb.jpg",
        &[
            "-frames:v",
            "1",
            "-vf",
            "scale=240:240:force_original_aspect_ratio=decrease",
            "-q:v",
            "5",
        ],
        cancelled,
    )?;
    wechat_protocol::media::inspect_image(&jpeg)?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_core() -> (tempfile::TempDir, Arc<Core>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let core = Core::new(store, None).unwrap();
        (dir, core)
    }
    #[test]
    fn destination_validation_supports_direct_and_group_without_test_only_restrictions() {
        for value in ["filehelper", "wxid_synthetic", "123456@chatroom"] {
            assert!(validate_destination(value).is_ok());
        }
        for value in [
            "",
            "@chatroom",
            "x@y@chatroom",
            "display name",
            "line\nbreak",
        ] {
            assert!(validate_destination(value).is_err());
        }
    }
    #[test]
    fn queued_media_respects_disconnected_and_auto_connect_policy() {
        let (_dir, core) = test_core();
        assert!(!core.media_network_suspended("one").unwrap());
        core.store
            .put_runtime("one", "desired", b"disconnected")
            .unwrap();
        assert!(core.media_network_suspended("one").unwrap());
        core.store
            .put_runtime("one", "desired", b"connected")
            .unwrap();
        core.store.put_runtime("one", "auto-connect", b"0").unwrap();
        assert!(core.media_network_suspended("one").unwrap());
        core.store
            .put_runtime(
                "one",
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )
            .unwrap();
        assert!(!core.media_network_suspended("one").unwrap());
    }
    #[test]
    fn idempotency_uses_content_and_variant_roles_instead_of_upload_ids() {
        let mut request = MediaSendRequest {
            thread_id: "filehelper".into(),
            kind: MediaKind::Image,
            source_id: "first".into(),
            standard_source_id: None,
            thumbnail_source_id: None,
            play_seconds: None,
            duration_ms: None,
        };
        let before = media_request_hash(
            &request,
            "same-content",
            Some("standard"),
            Some("thumb"),
            None,
        )
        .unwrap();
        request.source_id = "reuploaded-same-content".into();
        request.standard_source_id = Some("different-standard-id".into());
        assert_eq!(
            before,
            media_request_hash(
                &request,
                "same-content",
                Some("standard"),
                Some("thumb"),
                None
            )
            .unwrap()
        );
        assert_ne!(
            before,
            media_request_hash(
                &request,
                "different-content",
                Some("standard"),
                Some("thumb"),
                None
            )
            .unwrap()
        );
        assert_ne!(
            media_request_hash(&request, "same", Some("variant"), None, None).unwrap(),
            media_request_hash(&request, "same", None, Some("variant"), None).unwrap()
        );
        request.kind = MediaKind::File;
        assert_ne!(
            media_request_hash(&request, "same", None, None, Some("first.txt")).unwrap(),
            media_request_hash(&request, "same", None, None, Some("second.txt")).unwrap()
        );
    }
    #[test]
    fn immutable_sources_and_cached_expired_bytes_survive_restart_protected() {
        let (dir, core) = test_core();
        let raw = b"synthetic private attachment bytes";
        let source = core
            .upload_source("one", "report.txt", "text/plain", raw)
            .unwrap();
        let (_, sealed) = core.store.media_source("one", &source.id).unwrap();
        assert!(!sealed.windows(raw.len()).any(|w| w == raw));
        assert!(core.source_bytes("two", &source.id).is_err());
        for name in ["../escape", "bad\nname", ".", "a/b"] {
            assert!(core.upload_source("one", name, "text/plain", raw).is_err());
        }
        let asset = MediaAsset {
            id: "asset_synthetic".into(),
            account_id: "one".into(),
            message_id: None,
            kind: MediaKind::File,
            variant: MediaVariant::Original,
            actual_variant: None,
            state: "pending".into(),
            content_type: None,
            byte_length: None,
            sha256: None,
            filename: Some("report.txt".into()),
            expires_at: Some(1),
            reason: None,
            retryable: true,
            job_id: None,
            content_url: None,
            animation_frames: None,
            official_writeback: "unsupported".into(),
            preview: None,
        };
        let plan = core
            .vault
            .seal(
                "one",
                "plan:asset_synthetic",
                &serde_json::to_vec(&DownloadPlan::CachedOnly).unwrap(),
            )
            .unwrap();
        core.store
            .put_media_asset(
                "one",
                &MediaAssetRecord {
                    id: asset.id.clone(),
                    message_id: None,
                    variant: "original".into(),
                    kind: "file".into(),
                    metadata: serde_json::to_value(asset).unwrap(),
                    plan,
                    payload: None,
                },
            )
            .unwrap();
        let available = core
            .cache_asset(
                "one",
                "asset_synthetic",
                raw.to_vec(),
                "text/plain",
                Some(MediaVariant::Original),
                None,
            )
            .unwrap();
        assert_eq!(available.state, "available");
        assert_eq!(available.expires_at, Some(1));
        // A logout fence does not turn local-only repair into authentication or
        // a new transport job. Missing profile metadata remains an explicit gap.
        core.store.put_runtime("one", "logged-out", b"1").unwrap();
        let repaired = core.repair_media_writeback("one", 10).unwrap();
        assert_eq!(repaired["scanned"], 1);
        assert_eq!(repaired["blocked"], 1);
        assert!(core.store.queued_media_jobs("one").unwrap().is_empty());
        assert!(core.repair_media_writeback("one", 0).is_err());
        assert!(core.repair_media_writeback("one", 101).is_err());
        let restored = core.source_bytes("one", &source.id).unwrap().1;
        assert_eq!(restored, raw);
        drop(core);
        let store = Arc::new(wechat_store::Store::open_existing(&dir.path().join("data")).unwrap());
        let core = Core::new(store, None).unwrap();
        assert_eq!(core.source_bytes("one", &source.id).unwrap().1, raw);
        assert_eq!(
            core.media_content("one", "asset_synthetic").unwrap().bytes,
            raw
        );
        assert!(core.media_content("two", "asset_synthetic").is_err());
    }
    #[test]
    fn wxgf_source_and_preview_remain_distinct_encrypted_account_scoped_and_restartable() {
        let (dir, core) = test_core();
        let raw = include_bytes!("../../wechat-media/tests/fixtures/static.wxgf");
        let asset: MediaAsset = serde_json::from_value(json!({
            "id":"asset_wxgf", "accountId":"one", "kind":"image", "variant":"original",
            "state":"pending", "retryable":true, "officialWriteback":"unsupported", "expiresAt":1
        }))
        .unwrap();
        core.store
            .put_media_asset(
                "one",
                &MediaAssetRecord {
                    id: asset.id.clone(),
                    message_id: None,
                    variant: "original".into(),
                    kind: "image".into(),
                    metadata: serde_json::to_value(&asset).unwrap(),
                    plan: vec![],
                    payload: None,
                },
            )
            .unwrap();
        let available = core
            .cache_asset(
                "one",
                &asset.id,
                raw.to_vec(),
                "image/x-wxgf",
                Some(MediaVariant::Original),
                Some(1),
            )
            .unwrap();
        assert_eq!(available.animation_frames, None);
        assert_eq!(available.sha256.as_deref(), Some(digest(raw).as_str()));
        assert_eq!(available.expires_at, Some(1));
        let preview = available.preview.as_ref().unwrap();
        assert_eq!(preview.source_sha256, digest(raw));
        assert_ne!(preview.sha256, preview.source_sha256);
        let original = core.media_content("one", &asset.id).unwrap();
        assert_eq!(original.bytes, raw);
        assert_eq!(original.content_type, "image/x-wxgf");
        let still = core.media_preview_content("one", &asset.id).unwrap();
        assert_eq!(still.source_sha256.as_deref(), Some(digest(raw).as_str()));
        assert_eq!(digest(&still.bytes), preview.sha256);
        assert_eq!(wechat_media::inspect_image(&still.bytes).unwrap().frames, 1);
        let cache_id = wxgf_preview_source_id(&digest(raw));
        let (metadata, sealed) = core.store.media_source("one", &cache_id).unwrap();
        assert!(!sealed.windows(still.bytes.len()).any(|b| b == still.bytes));
        let again = core
            .cache_asset(
                "one",
                &asset.id,
                raw.to_vec(),
                "image/x-wxgf",
                Some(MediaVariant::Original),
                None,
            )
            .unwrap();
        assert_eq!(again.preview.unwrap().sha256, preview.sha256);
        assert_eq!(
            core.store
                .media_source("one", &cache_id)
                .unwrap()
                .0
                .created_at,
            metadata.created_at
        );
        assert!(core.media_preview_content("two", &asset.id).is_err());
        drop(core);
        let core = Core::new(
            Arc::new(wechat_store::Store::open_existing(&dir.path().join("data")).unwrap()),
            None,
        )
        .unwrap();
        assert_eq!(core.media_content("one", &asset.id).unwrap().bytes, raw);
        assert_eq!(
            core.media_preview_content("one", &asset.id).unwrap().bytes,
            still.bytes
        );
        let mut tampered = core.media_asset("one", &asset.id).unwrap();
        tampered.preview.as_mut().unwrap().source_sha256 = "0".repeat(64);
        core.store
            .save_media_asset(
                "one",
                &asset.id,
                &serde_json::to_value(tampered).unwrap(),
                None,
            )
            .unwrap();
        assert_eq!(
            core.media_preview_content("one", &asset.id)
                .err()
                .unwrap()
                .to_string(),
            "media_integrity_failed"
        );
    }
    #[test]
    fn concurrent_wxgf_discoveries_reuse_the_same_verified_preview() {
        let (_dir, core) = test_core();
        let barrier = std::sync::Barrier::new(2);
        let bytes = include_bytes!("../../wechat-media/tests/fixtures/static.wxgf");
        let previews = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                core.cache_wxgf_preview("one", "first", bytes)
            });
            let second = scope.spawn(|| {
                barrier.wait();
                core.cache_wxgf_preview("one", "second", bytes)
            });
            (
                first.join().unwrap().unwrap(),
                second.join().unwrap().unwrap(),
            )
        });
        assert_eq!(previews.0.sha256, previews.1.sha256);
        assert_eq!(previews.0.source_sha256, digest(bytes));
        assert_eq!(previews.1.source_sha256, digest(bytes));
    }
    #[test]
    fn uncertain_send_is_terminal_and_preserves_full_decimal_server_id() {
        let (_dir, core) = test_core();
        let (mut row, _) = core
            .store
            .create_media_job(
                "one",
                "job_test",
                "send:test",
                &"0".repeat(64),
                "send",
                &json!({}),
                b"protected",
            )
            .unwrap();
        core.store.claim_media_job("one", &row.id).unwrap();
        row.phase = "submitting".into();
        row.state = "running".into();
        core.store.save_media_job("one", &row).unwrap();
        core.fail_media_job("one", &row.id, "transport_unavailable")
            .unwrap();
        assert_eq!(
            core.media_job("one", &row.id).unwrap().state,
            "needs_review"
        );
        assert!(core.store.retry_media_job("one", &row.id).is_err());
        let (row, _) = core
            .store
            .create_media_job(
                "one",
                "job_accepted",
                "send:accepted",
                &"1".repeat(64),
                "send",
                &json!({}),
                b"protected",
            )
            .unwrap();
        core.finish_media_send(
            "one",
            &row.id,
            wechat_protocol::media_send::MediaSendResult {
                status: wechat_protocol::media_send::SendStatus::ServerAccepted,
                service_code: 0,
                server_message_id: Some(u64::MAX.to_string()),
                client_id_matches: Some(true),
            },
        )
        .unwrap();
        let accepted = core.media_job("one", &row.id).unwrap();
        assert_eq!(accepted.server_message_id, Some(u64::MAX.to_string()));
        assert_eq!(
            accepted.result["nativeReceipt"]["serverMessageId"],
            u64::MAX.to_string()
        );
    }
    #[test]
    fn sticker_discovery_rejects_other_hosts_and_never_returns_xml_bytes() {
        let msg = message(47);
        let xml="<msg><emoji md5=\"00000000000000000000000000000000\" len=\"42\" cdnurl=\"https://127.0.0.1/private\"/></msg>";
        assert!(discover(&msg, xml).is_err());
    }
    fn file_message_xml(encryption: &str, transport: &str) -> String {
        format!(
            "<msg><appmsg><title>synthetic.pdf</title><type>6</type><md5>0123456789abcdef0123456789abcdef</md5><appattach><totallen>8260</totallen><fileext>pdf</fileext>{transport}<aeskey>01010101010101010101010101010101</aeskey>{encryption}</appattach></appmsg></msg>"
        )
    }
    #[test]
    fn file_cdn_versions_zero_one_and_omitted_keep_strict_integrity_metadata() {
        for encryption in ["", "<encryver>0</encryver>", "<encryver>1</encryver>"] {
            let xml =
                file_message_xml(encryption, "<cdnattachurl>synthetic-file-id</cdnattachurl>");
            let plans = discover(&message(49), &xml).unwrap();
            assert_eq!(plans.len(), 1);
            let (kind, variant, plan, filename) = &plans[0];
            assert_eq!(*kind, MediaKind::File);
            assert_eq!(*variant, MediaVariant::Original);
            assert_eq!(filename.as_deref(), Some("synthetic.pdf"));
            let DownloadPlan::Cdn { descriptor } = plan else {
                panic!("file must use the verified CDN path");
            };
            assert_eq!(descriptor.quality, MediaQuality::File);
            assert_eq!(descriptor.file_id, "synthetic-file-id");
            assert_eq!(descriptor.aes_key, vec![1; 16]);
            assert_eq!(descriptor.expected_bytes, Some(8260));
            assert_eq!(
                descriptor.expected_md5.as_deref(),
                Some("0123456789abcdef0123456789abcdef")
            );
            assert!(descriptor.strict_size);
        }
    }
    #[test]
    fn file_discovery_rejects_unknown_or_malformed_versions_and_missing_transport() {
        for (version, expected) in [
            ("2", "unsupported_attachment_encryption"),
            ("-1", "media_metadata_invalid"),
            ("invalid", "media_metadata_invalid"),
            ("18446744073709551616", "media_metadata_invalid"),
            ("", "media_metadata_invalid"),
        ] {
            let xml = file_message_xml(
                &format!("<encryver>{version}</encryver>"),
                "<cdnattachurl>synthetic-file-id</cdnattachurl>",
            );
            assert_eq!(
                discover(&message(49), &xml).err().unwrap().to_string(),
                expected
            );
        }
        for transport in ["", "<cdnattachurl/>"] {
            let xml = file_message_xml("<encryver>0</encryver>", transport);
            assert_eq!(
                discover(&message(49), &xml).err().unwrap().to_string(),
                "unsupported_attachment_transport"
            );
        }
        let xml = file_message_xml(
            "<encryver>0</encryver>",
            "<cdnattachurl>synthetic-file-id</cdnattachurl>",
        )
        .replace("<type>6</type>", "<type>74</type>");
        assert_eq!(
            discover(&message(49), &xml).err().unwrap().to_string(),
            "unsupported_application_media"
        );
    }
    fn message(kind: i64) -> wechat_profile::Message {
        wechat_profile::Message {
            id: wechat_profile::SourceMessageId {
                database: "message/message_0.db".into(),
                table: "Msg_synthetic".into(),
                local_id: "1".into(),
            },
            server_message_id: Some("18446744073709551615".into()),
            server_id_signed: None,
            peer: Some("synthetic".into()),
            sender: Some("synthetic".into()),
            outgoing: Some(false),
            message_type: kind,
            local_type: kind.to_string(),
            create_time: 1,
            sort_seq: None,
            content: wechat_profile::DecodedField {
                text: None,
                state: wechat_profile::DecodeState::Null,
                compression_flag: None,
            },
            source: wechat_profile::DecodedField {
                text: None,
                state: wechat_profile::DecodeState::Null,
                compression_flag: None,
            },
            raw_fields: Default::default(),
        }
    }
    #[test]
    fn image_variants_are_distinct_and_decodable() {
        let mut source = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(800, 600)
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        let raw = source.into_inner();
        let (a, b, c) = image_variants(&raw, None, None).unwrap();
        assert!(image_variants(&a, Some(raw), None).is_err());
        assert!(image_variants(&a, None, Some(vec![0; 65521])).is_err());
        for bytes in [&a, &b, &c] {
            assert!(bytes.len() <= 65520);
            wechat_protocol::media::inspect_image(bytes).unwrap();
        }
        assert_ne!(digest(&a), digest(&c));
    }
    #[test]
    fn accepted_local_xml_preserves_distinct_variants_and_escapes_opaque_ids() {
        let uploaded = |id: &str, quality, key: u8, size: u32, width, height| UploadedMedia {
            descriptor: MediaDescriptor {
                file_id: id.into(),
                aes_key: vec![key; 16],
                quality,
                expected_bytes: Some(size as u64),
                expected_md5: Some("0".repeat(32)),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: size,
            md5: "0".repeat(32),
            sha256: "0".repeat(64),
            file_name: "synthetic.jpg".into(),
            width: Some(width),
            height: Some(height),
        };
        let image = UploadedImage {
            original: uploaded("original<&\"id", MediaQuality::Original, 1, 100, 80, 60),
            standard: uploaded("standard", MediaQuality::Standard, 1, 60, 40, 30),
            thumbnail: uploaded("thumbnail", MediaQuality::Thumbnail, 2, 20, 20, 15),
        };
        let xml = wechat_protocol::media_send::local_image_xml(&image).unwrap();
        let plans = discover(&message(3), &xml).unwrap();
        assert_eq!(plans.len(), 3);
        for ((_, variant, plan, _), id, size, key) in plans
            .into_iter()
            .zip(["original<&\"id", "standard", "thumbnail"])
            .zip([100, 60, 20])
            .zip([1, 1, 2])
            .map(|(((plan, id), size), key)| (plan, id, size, key))
        {
            let DownloadPlan::Cdn { descriptor } = plan else {
                panic!()
            };
            assert_eq!(descriptor.file_id, id);
            assert_eq!(descriptor.expected_bytes, Some(size));
            assert_eq!(descriptor.aes_key, vec![key; 16]);
            assert_ne!(variant, MediaVariant::Best);
        }
        let voice = wechat_protocol::media_send::local_voice_xml(200, 1000).unwrap();
        assert_eq!(discover(&message(34), &voice).unwrap().len(), 2);
    }
    #[test]
    fn video_thumbnail_is_a_distinct_tuple_and_never_validated_as_mp4() {
        let plans=discover(&message(43),"<msg><videomsg cdnvideourl=\"normal-id\" aeskey=\"01010101010101010101010101010101\" length=\"100\" cdnthumburl=\"thumbnail-id\" cdnthumbaeskey=\"02020202020202020202020202020202\" cdnthumblength=\"200\"/></msg>").unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[1].1, MediaVariant::Thumbnail);
        let DownloadPlan::Cdn { descriptor } = &plans[1].2 else {
            panic!()
        };
        assert_eq!(descriptor.quality, MediaQuality::Thumbnail);
        assert_eq!(descriptor.expected_bytes, Some(200));
        assert_eq!(descriptor.aes_key, vec![2; 16]);
        let mut output = std::io::Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(8, 6)
            .write_to(&mut output, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg = output.into_inner();
        assert_eq!(
            validate_asset_bytes(MediaKind::Video, MediaVariant::Thumbnail, &jpeg)
                .unwrap()
                .0,
            "image/jpeg"
        );
        assert!(validate_asset_bytes(MediaKind::Video, MediaVariant::Normal, &jpeg).is_err());
        assert!(validate_asset_bytes(
            MediaKind::Video,
            MediaVariant::Thumbnail,
            b"\0\0\0\x18ftypisomsynthetic"
        )
        .is_err());
    }
    #[test]
    fn raw_and_normal_video_tuples_remain_separate() {
        let plans=discover(&message(43),"<msg><videomsg cdnvideourl=\"normal-id\" aeskey=\"01010101010101010101010101010101\" length=\"100\" cdnrawvideourl=\"original-id\" cdnrawvideoaeskey=\"02020202020202020202020202020202\" rawlength=\"200\"/></msg>").unwrap();
        assert_eq!(plans.len(), 2);
        match &plans[1].2 {
            DownloadPlan::Cdn { descriptor } => {
                assert_eq!(descriptor.file_id, "original-id");
                assert_eq!(descriptor.expected_bytes, Some(200));
                assert_eq!(descriptor.aes_key, vec![2; 16]);
            }
            _ => panic!(),
        }
    }
    #[test]
    fn pcm_wav_fast_path_checks_format_chunks_padding_and_truncation() {
        let mut wav = b"RIFF".to_vec();
        wav.extend([0; 4]);
        wav.extend(b"WAVEJUNK");
        wav.extend(1u32.to_le_bytes());
        wav.extend([77, 0]);
        wav.extend(b"fmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend([1, 0, 1, 0]);
        wav.extend(16000u32.to_le_bytes());
        wav.extend(32000u32.to_le_bytes());
        wav.extend([2, 0, 16, 0]);
        wav.extend(b"data");
        wav.extend(4u32.to_le_bytes());
        wav.extend([0, 128, 255, 127]);
        let length = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&length.to_le_bytes());
        assert_eq!(pcm16k_wav(&wav).unwrap(), Some(vec![i16::MIN, i16::MAX]));
        for n in 12..wav.len() {
            assert!(pcm16k_wav(&wav[..n]).is_err());
        }
        let mut stereo = wav.clone();
        stereo[32] = 2;
        assert!(pcm16k_wav(&stereo).unwrap().is_none());
        let mut truncated_chunk = wav.clone();
        truncated_chunk[50..54].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(pcm16k_wav(&truncated_chunk).is_err());
    }
    #[test]
    fn service_errors_expose_only_fixed_stage_and_whole_numeric_codes() {
        let expired_auth = anyhow::anyhow!("CDN bootstrap returned -13");
        assert_eq!(service_diagnostic(&expired_auth), Some(("bootstrap", -13)));
        assert_eq!(classify_error(&expired_auth), "auth_required");
        let denied = anyhow::anyhow!("CDN HTTP status 403");
        assert_eq!(classify_error(&denied), "media_service_rejected");
        assert_eq!(
            classify_error(&anyhow::anyhow!("CDN HTTP status 503")),
            "transport_unavailable"
        );
        assert!(service_diagnostic(&anyhow::anyhow!(
            "CDN bootstrap returned -13 https://secret.invalid"
        ))
        .is_none());
        assert!(service_diagnostic(&anyhow::anyhow!("prefix CDN download returned 4")).is_none());
        let nested = anyhow::anyhow!("connection closed before message completed")
            .context("opaque request wrapper");
        assert_eq!(classify_error(&nested), "transport_unavailable");
    }
    #[test]
    fn image_validation_errors_expose_only_exact_fixed_codes() {
        for code in [
            "image_decode_failed",
            "image_decode_limit",
            "jpeg_trailer_missing",
            "png_trailer_missing",
            "gif_trailer_missing",
            "webp_length_invalid",
        ] {
            let error = anyhow::anyhow!("{code}").context("opaque validation wrapper");
            assert_eq!(classify_error(&error), code);
        }
        assert_eq!(
            classify_error(&anyhow::anyhow!(
                "image_decode_failed https://private.invalid/image"
            )),
            "media_processing_failed"
        );
    }
    #[test]
    fn fresh_audio_uses_independent_codec() {
        let pcm = (0..16000)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 12000.0) as i16)
            .collect::<Vec<_>>();
        let silk = wechat_protocol::silk::encode(&pcm).unwrap();
        let decoded = audio_pcm(&silk, || Ok(())).unwrap();
        assert_eq!(decoded.len(), 16000);
    }
}
