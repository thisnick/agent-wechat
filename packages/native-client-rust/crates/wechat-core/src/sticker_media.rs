//! Verified Saved media indexes point to canonical official cache files.
use super::*;
impl Core {
    /// Acquire an advertised pack preview through the ordinary download worker.
    /// This accepts only daemon-issued preview assets and never adds membership.
    pub fn download_sticker_preview(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        id: &str,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control),
            "invalid_idempotency_key"
        );
        ensure!(
            id.starts_with("sticker_preview_"),
            "invalid_sticker_download"
        );
        let key = format!("sticker-preview:{key}");
        let hash = digest(id.as_bytes());
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        let asset = self.media_asset(account, id)?;
        ensure!(
            asset.kind == MediaKind::Image
                && asset.variant == MediaVariant::Thumbnail
                && asset.state != "expired",
            "invalid_sticker_download"
        );
        let row = self.store.media_asset(account, id)?;
        ensure!(
            matches!(
                self.asset_plan(account, &row)?.plan,
                DownloadPlan::LinkThumbnail { .. }
            ),
            "invalid_sticker_download"
        );
        let job = format!("job_{}", uuid::Uuid::new_v4());
        let (mut row, created) = self.store.create_media_job(
            account,
            &job,
            &key,
            &hash,
            "download",
            &json!({"assetId":id,"completedBytes":0}),
            b"",
        )?;
        if created && asset.state == "available" {
            row.state = "available".into();
            row.phase = "completed".into();
            row.data["result"] = serde_json::to_value(&asset)?;
            self.store.save_media_job(account, &row)?;
        } else if created {
            self.spawn_media_worker(account, &job);
        }
        Ok(public_job(account, &row))
    }
    pub(super) fn saved_cache_bytes(&self, account: &str, id: &str) -> Result<Vec<u8>> {
        let identity = self.profile_identity(account)?;
        let record = self
            .profile_reader_owned(account)?
            .saved_sticker_record(id)?;
        let (location, _) = self.profile_location_keys(account)?;
        Ok(wechat_profile::read_saved_sticker_cache(
            &location,
            identity.uin,
            &identity.username,
            id,
            (!record.extern_md5.is_empty()).then_some(record.extern_md5.as_str()),
        )?)
    }
    /// Read one Saved item and available original/preview references. This may
    /// index owned cache bytes; it never downloads or changes Saved membership.
    pub fn custom_sticker(&self, account: &str, id: &str) -> Result<crate::CustomSticker> {
        let mut item = self
            .saved_stickers(account)?
            .items
            .into_iter()
            .find(|item| item.id == id)
            .context("custom_sticker_not_found")?;
        let record = match self.profile_reader_owned(account)?.saved_sticker_record(id) {
            Ok(record) => record,
            Err(wechat_profile::ProfileError::NotCached) => return Ok(item),
            Err(error) => return Err(error.into()),
        };
        let asset_id = format!("sticker_saved_{id}");
        let cached = self.saved_cache_bytes(account, id);
        let mut asset = MediaAsset {
            id: asset_id.clone(),
            account_id: account.into(),
            message_id: None,
            kind: MediaKind::Sticker,
            variant: MediaVariant::Original,
            actual_variant: None,
            state: "pending".into(),
            content_type: None,
            byte_length: None,
            advertised_byte_length: None,
            auto_download_allowed: Some(false),
            sha256: None,
            filename: None,
            expires_at: None,
            reason: None,
            retryable: true,
            job_id: None,
            content_url: None,
            animation_frames: None,
            official_writeback: "pending".into(),
            preview: None,
        };
        let has_cached = match cached {
            Ok(bytes) => {
                let inspected = wechat_media::inspect_image(&bytes)?;
                asset.state = "available".into();
                asset.actual_variant = Some(MediaVariant::Original);
                asset.content_type = Some(inspected.content_type.clone());
                asset.byte_length = Some(bytes.len() as u64);
                asset.sha256 = Some(digest(&bytes));
                asset.animation_frames = Some(inspected.frames);
                let extension = match inspected.content_type.as_str() {
                    "image/gif" => "gif",
                    "image/png" => "png",
                    "image/jpeg" => "jpg",
                    "image/webp" => "webp",
                    "image/x-wxgf" => "wxgf",
                    _ => "bin",
                };
                asset.filename = Some(format!("{id}.{extension}"));
                asset.retryable = false;
                asset.content_url =
                    Some(format!("/v1/accounts/{account}/media/{asset_id}/content"));
                asset.official_writeback = "complete".into();
                // Unsupported visual decoding is reported by the absence of a
                // preview; the verified original remains independently usable.
                if let Ok(decoded) = wechat_media::sticker_preview(&bytes) {
                    let sheet = decoded.contact_sheet;
                    let extension = if sheet.inspection.content_type == "image/png" {
                        "png"
                    } else {
                        "jpg"
                    };
                    asset.preview=Some(MediaImagePreview { source_sha256:digest(&bytes),byte_length:sheet.bytes.len() as u64,
                        sha256:digest(&sheet.bytes),filename:format!("{id}.preview.{extension}"),content_type:sheet.inspection.content_type,
                        content_url:format!("/v1/accounts/{account}/media/{asset_id}/content?representation=preview") });
                }
                true
            }
            Err(error)
                if error
                    .downcast_ref::<wechat_profile::ProfileError>()
                    .is_some_and(|e| matches!(e, wechat_profile::ProfileError::NotCached)) =>
            {
                false
            }
            Err(_) => {
                asset.state = "unavailable".into();
                asset.reason = Some("sticker_cache_unreadable".into());
                false
            }
        };
        if !has_cached && record.cdn_url.is_empty() {
            asset.state = "unavailable".into();
            asset.reason = Some("sticker_source_unavailable".into());
            asset.retryable = false;
        }
        let plan = self.vault.seal(
            account,
            &format!("plan:{asset_id}"),
            &serde_json::to_vec(&identity::AuthorizedPlan {
                identity: None,
                plan: DownloadPlan::SavedSticker {
                    id: id.into(),
                    url: record.cdn_url,
                },
            })?,
        )?;
        self.store.put_media_asset(
            account,
            &MediaAssetRecord {
                id: asset_id.clone(),
                message_id: None,
                kind: "sticker".into(),
                variant: "original".into(),
                metadata: serde_json::to_value(&asset)?,
                plan,
                payload: None,
            },
        )?;
        if has_cached {
            self.store.save_media_asset_in_profile(
                account,
                &asset_id,
                &serde_json::to_value(&asset)?,
                &format!("sticker-saved:{id}"),
            )?;
        } else {
            self.store.save_media_asset(
                account,
                &asset_id,
                &serde_json::to_value(&asset)?,
                None,
            )?;
        }
        item.original = Some(asset);
        Ok(item)
    }
    /// Explicitly acquire one Saved original with an idempotent download job.
    /// Cached originals complete locally without initiating a remote request.
    pub fn download_custom_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        sticker: &str,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty() && key.len() <= 200,
            "invalid_idempotency_key"
        );
        let key = format!("sticker-download:{key}");
        let hash = digest(sticker.as_bytes());
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        let item = self.custom_sticker(account, sticker)?;
        let asset = item.original.context("sticker_source_unavailable")?;
        ensure!(
            asset.state != "unavailable" || asset.retryable,
            "sticker_source_unavailable"
        );
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let (mut row, created) = self.store.create_media_job(
            account,
            &id,
            &key,
            &hash,
            "download",
            &json!({"assetId":asset.id,"completedBytes":0}),
            b"",
        )?;
        if created && asset.state == "available" {
            row.state = "available".into();
            row.phase = "completed".into();
            row.data["result"] = serde_json::to_value(&asset)?;
            self.store.save_media_job(account, &row)?;
        } else if created {
            self.spawn_media_worker(account, &id);
        }
        Ok(public_job(account, &row))
    }
    pub(super) fn write_saved_cache(
        &self,
        account: &str,
        job: &str,
        id: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let state = self.profile_identity(account)?;
        let (location, _) = self.profile_location_keys(account)?;
        let options = self.write_options(account, job)?;
        wechat_profile::apply_saved_sticker_cache(
            &location,
            state.uin,
            &state.username,
            id,
            bytes,
            &options,
        )?;
        Ok(())
    }
}
