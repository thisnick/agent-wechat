//! Preparation uses the existing encrypted media/job store, without account writes.
use super::*;
use crate::{PreparedSticker, StickerPreparationRequest};

impl Core {
    /// Queue idempotent preparation of an uploaded image, GIF, or MOV/MP4 video. The
    /// caller holds the account mutation guard during admission.
    pub fn prepare_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        request: StickerPreparationRequest,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty()
                && key.len() <= 200
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() || byte == b' '),
            "invalid_idempotency_key"
        );
        self.store.expire_sticker_preparation_assets(
            account,
            chrono::Utc::now().timestamp(),
            100,
        )?;
        if let Some(trim) = &request.trim {
            ensure!(
                trim.duration_ms > 0 && trim.duration_ms <= 10000,
                "sticker_trim_invalid"
            );
        }
        let source = self
            .store
            .media_source_metadata(account, &request.source_id)?;
        ensure!(
            source.byte_length <= crate::MAX_STICKER_SOURCE_BYTES as u64,
            "sticker_source_size_limit"
        );
        let hash = digest(&serde_json::to_vec(&(
            &source.sha256,
            request.fit,
            &request.trim,
        ))?);
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&request)?,
        )?;
        let (row, created) = self.store.create_media_job(
            account,
            &id,
            &format!("sticker_prepare:{key}"),
            &hash,
            "sticker_prepare",
            &json!({"sourceId":request.source_id,"completedBytes":0}),
            &payload,
        )?;
        if created {
            self.spawn_media_worker(account, &row.id);
        }
        self.sticker_preparation(account, &row.id)
    }

    /// Read the preparation job and its verified media references. No worker or
    /// network transfer is started by this read.
    pub fn sticker_preparation(&self, account: &str, id: &str) -> Result<MediaJob> {
        let row = self.store.media_job(account, id)?;
        ensure!(
            row.kind == "sticker_prepare",
            "sticker_preparation_not_found"
        );
        let mut job = public_job(account, &row);
        if row.data["result"]["expiresAt"]
            .as_i64()
            .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp())
        {
            job.state = "expired".into();
            job.reason = Some("sticker_preparation_expired".into());
            job.retryable = false;
        }
        Ok(job)
    }

    pub(super) fn perform_sticker_preparation(
        &self,
        account: &str,
        id: &str,
        permit: &mut MediaAccountPermit,
    ) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        let plain = self
            .vault
            .open_record(account, &format!("media-job:{id}"), &row.payload)?;
        let request: StickerPreparationRequest = serde_json::from_slice(&plain)?;
        let (_, bytes) = self.source_bytes(account, &request.source_id)?;
        ensure!(
            bytes.len() <= crate::MAX_STICKER_SOURCE_BYTES,
            "sticker_source_size_limit"
        );
        row.phase = "preparing".into();
        self.store.save_media_job(account, &row)?;
        let output = permit.transfer(self, account, || {
            wechat_media::prepare_sticker(
                &bytes,
                &wechat_media::StickerImageOptions {
                    fit: request.fit,
                    trim: request.trim.map(|trim| wechat_media::StickerTrim {
                        start_ms: trim.start_ms,
                        duration_ms: trim.duration_ms,
                    }),
                },
                || {
                    ensure!(
                        !self.store.media_job(account, id)?.cancel_requested,
                        "media_cancelled"
                    );
                    Ok(())
                },
            )
        })?;
        row = self.store.media_job(account, id)?;
        if row.cancel_requested {
            row.state = "cancelled".into();
            row.phase = "cancelled".into();
            self.store.save_media_job(account, &row)?;
            return Ok(());
        }
        let expires_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)?.timestamp() + 86400;
        ensure!(
            expires_at > chrono::Utc::now().timestamp(),
            "sticker_preparation_expired"
        );
        let original = self.store_prepared_sticker_asset(
            account,
            id,
            "original",
            &output.bytes,
            "image/gif",
            output.inspection.frames,
            expires_at,
        )?;
        let preview = self.store_prepared_sticker_asset(
            account,
            id,
            "preview",
            &output.preview,
            "image/png",
            1,
            expires_at,
        )?;
        let contact_sheet = self.store_prepared_sticker_asset(
            account,
            id,
            "sheet",
            &output.contact_sheet,
            "image/png",
            1,
            expires_at,
        )?;
        let prepared = PreparedSticker {
            id: id.into(),
            source_id: request.source_id,
            original,
            preview,
            contact_sheet,
            width: output.inspection.width,
            height: output.inspection.height,
            duration_ms: output.duration_ms,
            transformations: output.transformations,
            warnings: vec![],
            expires_at,
        };
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        row.data["completedBytes"] = bytes.len().into();
        row.data["result"] = serde_json::to_value(prepared)?;
        self.store.save_media_job(account, &row)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn store_prepared_sticker_asset(
        &self,
        account: &str,
        preparation: &str,
        role: &str,
        bytes: &[u8],
        mime: &str,
        frames: u32,
        expires_at: i64,
    ) -> Result<MediaAsset> {
        let id = format!("{preparation}_{role}");
        let variant = if role == "original" {
            MediaVariant::Original
        } else {
            MediaVariant::Thumbnail
        };
        let asset = MediaAsset {
            id: id.clone(),
            account_id: account.into(),
            message_id: None,
            kind: MediaKind::Sticker,
            variant,
            actual_variant: Some(variant),
            state: "available".into(),
            content_type: Some(mime.into()),
            byte_length: Some(bytes.len() as u64),
            advertised_byte_length: None,
            auto_download_allowed: None,
            sha256: Some(digest(bytes)),
            filename: Some(format!(
                "{id}.{}",
                if role == "original" { "gif" } else { "png" }
            )),
            expires_at: None,
            reason: None,
            retryable: false,
            job_id: Some(preparation.into()),
            content_url: Some(format!("/v1/accounts/{account}/media/{id}/content")),
            animation_frames: Some(frames),
            official_writeback: "not_applicable".into(),
            preview: None,
        };
        let plan = self.vault.seal(
            account,
            &format!("plan:{id}"),
            &serde_json::to_vec(&identity::AuthorizedPlan {
                identity: None,
                plan: DownloadPlan::CachedOnly,
            })?,
        )?;
        let payload = self
            .vault
            .seal_media(account, &format!("asset:{id}"), bytes)?;
        let mut metadata = serde_json::to_value(&asset)?;
        metadata["preparationExpiresAt"] = expires_at.into();
        self.store.put_media_asset(
            account,
            &MediaAssetRecord {
                id: id.clone(),
                message_id: None,
                variant: if role == "original" {
                    "original"
                } else {
                    "thumbnail"
                }
                .into(),
                kind: "sticker".into(),
                metadata: metadata.clone(),
                plan,
                payload: Some(payload),
            },
        )?;
        // Replays after an interrupted local preparation must install the same
        // deterministic asset and never expose metadata without verified bytes.
        self.store.save_media_asset(account, &id, &metadata, None)?;
        Ok(asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GIF: &[u8] = include_bytes!("../../wechat-simulator/fixtures/sticker.gif");

    fn core() -> (tempfile::TempDir, Arc<Core>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        (dir, Core::new(store, None).unwrap())
    }
    async fn finished(core: &Core, id: &str) -> MediaJob {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let job = core.sticker_preparation("one", id).unwrap();
                if !matches!(job.state.as_str(), "queued" | "running") {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn preparation_without_login_is_idempotent_and_serves_verified_real_media() {
        let (_dir, core) = core();
        core.store
            .put_runtime("one", "desired", b"disconnected")
            .unwrap();
        let source = core
            .upload_source("one", "test.gif", "image/gif", GIF)
            .unwrap();
        let request = StickerPreparationRequest {
            source_id: source.id,
            fit: false,
            trim: None,
        };
        let guard = core.lock_account("one").await.unwrap();
        let first = core
            .prepare_sticker("one", "same", request.clone())
            .unwrap();
        let repeated = core
            .prepare_sticker("one", "same", request.clone())
            .unwrap();
        assert_eq!(first.id, repeated.id);
        assert!(core
            .prepare_sticker(
                "one",
                "same",
                StickerPreparationRequest {
                    fit: true,
                    ..request
                }
            )
            .is_err());
        drop(guard);
        let job = finished(&core, &first.id).await;
        assert_eq!(job.state, "available", "{:?}", job.reason);
        let prepared: PreparedSticker = serde_json::from_value(job.result).unwrap();
        let original = core.media_content("one", &prepared.original.id).unwrap();
        assert_eq!(original.bytes, GIF);
        let preview = core.media_content("one", &prepared.preview.id).unwrap();
        assert_eq!(preview.content_type, "image/png");
        assert!(preview.bytes.starts_with(b"\x89PNG"));
        assert_eq!(original.sha256, prepared.original.sha256.unwrap());
        assert!(core.media_content("other", &prepared.original.id).is_err());
    }
    #[tokio::test]
    async fn invalid_input_fails_without_creating_output_assets() {
        let (_dir, core) = core();
        let source = core
            .upload_source("one", "bad.gif", "image/gif", b"not a gif")
            .unwrap();
        let guard = core.lock_account("one").await.unwrap();
        let job = core
            .prepare_sticker(
                "one",
                "invalid",
                StickerPreparationRequest {
                    source_id: source.id,
                    fit: false,
                    trim: None,
                },
            )
            .unwrap();
        drop(guard);
        let job = finished(&core, &job.id).await;
        assert!(matches!(job.state.as_str(), "failed" | "unsupported"));
        assert!(core
            .media_asset("one", &format!("{}_original", job.id))
            .is_err());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn expiry_frees_preparation_files_but_preserves_pending_consumers_and_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let bytes = include_bytes!("../../wechat-simulator/fixtures/sticker.gif");
        let source = core
            .upload_source("one", "sticker.gif", "image/gif", bytes)
            .unwrap();
        let job = core
            .prepare_sticker(
                "one",
                "expires",
                StickerPreparationRequest {
                    source_id: source.id,
                    fit: false,
                    trim: None,
                },
            )
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let job = core.sticker_preparation("one", &job.id).unwrap();
                if job.state == "available" {
                    break job;
                }
                assert!(
                    matches!(job.state.as_str(), "queued" | "running"),
                    "{:?}",
                    job.reason
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let result: PreparedSticker = serde_json::from_value(completed.result).unwrap();
        for asset in [&result.original, &result.preview, &result.contact_sheet] {
            let mut metadata = core.store.media_asset("one", &asset.id).unwrap().metadata;
            metadata["preparationExpiresAt"] = 0.into();
            core.store
                .save_media_asset("one", &asset.id, &metadata, None)
                .unwrap();
        }
        let (mut consumer, _) = core
            .store
            .create_media_job(
                "one",
                "consumer",
                "consumer",
                &"0".repeat(64),
                "download",
                &json!({"preparationId":job.id}),
                b"",
            )
            .unwrap();
        assert_eq!(
            core.store
                .expire_sticker_preparation_assets("one", chrono::Utc::now().timestamp(), 100)
                .unwrap(),
            0
        );
        consumer.state = "available".into();
        core.store.save_media_job("one", &consumer).unwrap();
        assert_eq!(
            core.store
                .expire_sticker_preparation_assets("one", chrono::Utc::now().timestamp(), 100)
                .unwrap(),
            3
        );
        assert_eq!(
            core.media_asset("one", &result.original.id).unwrap().state,
            "expired"
        );
        assert!(core.media_content("one", &result.original.id).is_err());
        assert!(core
            .store
            .media_asset_with_payload("one", &result.original.id)
            .unwrap()
            .payload
            .is_none());
        assert!(core.store.media_job("one", &job.id).is_ok());
    }
}
