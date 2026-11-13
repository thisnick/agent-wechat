//! Explicit pack acquisition. A locator never establishes account membership.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use wechat_profile::StickerPackCacheKind as Cache;
use wechat_protocol::sticker_packs as wire;

#[derive(Serialize, Deserialize)]
struct Acquisition {
    pack: String,
    item: Option<String>,
    fence: MediaAuthorizationFence,
    submitted: bool,
    reply: Option<Vec<u8>>,
    detail: Option<wire::StickerPackDetail>,
    captions: Option<wire::StickerPackCaptions>,
    locator: Option<MediaDescriptor>,
}
pub(super) fn pack_asset_id(pack: &str, item: &str, role: &str) -> String {
    format!(
        "sticker_pack_{}_{}_{}",
        &digest(pack.as_bytes())[..24],
        item,
        role
    )
}
impl Core {
    /// Acquire one selected pack, or its selected item, through an idempotent job.
    /// Unadded content remains temporary; it never fabricates profile membership.
    pub fn download_sticker_pack(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        pack: &str,
        item: Option<&str>,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control),
            "invalid_idempotency_key"
        );
        ensure!(
            !pack.is_empty() && pack.len() <= 1024 && !pack.chars().any(char::is_control),
            "invalid_sticker_pack_id"
        );
        if let Some(item) = item {
            ensure!(
                item.len() == 32 && item.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid_sticker_id"
            );
        }
        let hash = digest(&serde_json::to_vec(&(pack, item))?);
        let key = format!("pack-download:{key}");
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let plan = Acquisition {
            pack: pack.into(),
            item: item.map(str::to_ascii_lowercase),
            fence: self.media_authorization_fence(account)?,
            submitted: false,
            reply: None,
            detail: None,
            captions: None,
            locator: None,
        };
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&plan)?,
        )?;
        let (row, created) = self.store.create_media_job(account, &id, &key, &hash, "sticker_acquire",
            &json!({"packId":pack,"preparationId":id,"serverAcceptance":"pending","profileWriteback":"not_applicable","result":{"serverAcceptance":"pending"}}), &payload)?;
        if created {
            self.spawn_media_worker(account, &row.id);
        }
        Ok(public_job(account, &row))
    }
    fn acquisition(&self, account: &str, id: &str) -> Result<Acquisition> {
        let row = self.store.media_job(account, id)?;
        Ok(serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?)
    }
    fn save_acquisition(&self, account: &str, id: &str, plan: &Acquisition) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(plan)?,
        )?;
        if plan.locator.is_some() {
            row.data["serverAcceptance"] = "accepted".into();
            row.data["result"]["serverAcceptance"] = "accepted".into();
        }
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    pub(super) fn perform_sticker_pack_download(
        &self,
        account: &str,
        id: &str,
        permit: &mut MediaAccountPermit,
    ) -> Result<()> {
        let mut plan = self.acquisition(account, id)?;
        ensure!(
            !plan.submitted || plan.locator.is_some(),
            "sticker_submission_needs_review"
        );
        let archive_id = format!("{id}_archive");
        let owned = self.media_content_owned(account, &archive_id, true).ok();
        ensure!(
            owned.is_some() || self.store.media_job(account, id)?.phase != "projecting",
            "media_not_cached"
        );
        let bytes = if let Some(content) = owned {
            content.bytes
        } else {
            if plan.locator.is_none() {
                let mut state = self.resume_media_state(account, id, &plan.fence)?;
                let raw = self.native_request(account, &mut state, |ctx| {
                    ctx.sticker_pack_detail_raw(&plan.pack)
                })?;
                let detail = wire::decode_sticker_pack_detail(&raw, &plan.pack)?;
                ensure!(
                    super::sticker_catalog::eligible_pack(&detail.summary),
                    "sticker_pack_not_eligible"
                );
                ensure!(
                    !detail.items.is_empty()
                        && plan
                            .item
                            .as_ref()
                            .is_none_or(|id| detail.items.iter().any(|item| &item.id == id)),
                    "sticker_pack_item_not_found"
                );
                let mut state = self.resume_media_state(account, id, &plan.fence)?;
                let captions = self.native_request(account, &mut state, |ctx| {
                    ctx.sticker_pack_captions_raw(&plan.pack)
                })?;
                plan.detail = Some(detail);
                plan.captions = Some(wire::decode_sticker_pack_captions(&captions)?);
                self.phase_media(account, id, "submitting")?;
                plan.submitted = true;
                self.save_acquisition(account, id, &plan)?;
                let mut state = self.resume_media_state(account, id, &plan.fence)?;
                let raw = self.native_request(account, &mut state, |ctx| {
                    ctx.sticker_pack_acquisition_raw(&plan.pack)
                })?;
                plan.reply = Some(raw.clone());
                self.save_acquisition(account, id, &plan)?;
                if wechat_protocol::protobuf::base_code(&wechat_protocol::protobuf::decode(&raw)?)?
                    != 0
                {
                    self.phase_media(account, id, "rejected")?;
                    bail!("sticker_pack_acquisition_rejected");
                }
                plan.locator = Some(wire::decode_sticker_pack_acquisition(&raw)?);
                self.save_acquisition(account, id, &plan)?;
            }
            self.phase_media(account, id, "downloading")?;
            let mut state = self.resume_media_state(account, id, &plan.fence)?;
            let config = self.native_request(account, &mut state, |ctx| ctx.cdn_config())?;
            let cdn = CdnClient::new(config, Duration::from_secs(25))?;
            let downloaded = permit.transfer(self, account, || {
                cdn.download_with_progress(plan.locator.as_ref().unwrap(), |n, t| {
                    self.progress_media(account, id, n, t)
                })
            })?;
            self.resume_media_state(account, id, &plan.fence)?;
            ensure!(
                downloaded.substituted_filetype.is_none_or(|t| t == 5),
                "unsupported_media_variant"
            );
            self.store_pack_bytes(
                account,
                id,
                &archive_id,
                "pack.zip",
                &downloaded.bytes,
                "application/zip",
                0,
            )?;
            downloaded.bytes
        };
        let ids = plan
            .detail
            .as_ref()
            .context("sticker_metadata_incomplete")?
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let archive = permit.transfer(self, account, || {
            wechat_media::decode_sticker_archive(&bytes, &plan.pack, &ids)
        })?;
        self.resume_media_state(account, id, &plan.fence)?;
        self.phase_media(account, id, "projecting")?;
        self.finish_verified_pack_content(account, id, &plan, archive)
    }
    #[allow(clippy::too_many_arguments)]
    fn store_pack_bytes(
        &self,
        account: &str,
        job: &str,
        id: &str,
        filename: &str,
        bytes: &[u8],
        mime: &str,
        frames: u32,
    ) -> Result<MediaAsset> {
        let mut asset = MediaAsset {
            id: id.into(),
            account_id: account.into(),
            message_id: None,
            kind: if mime == "application/zip" {
                MediaKind::File
            } else {
                MediaKind::Sticker
            },
            variant: MediaVariant::Original,
            actual_variant: Some(MediaVariant::Original),
            state: "available".into(),
            content_type: Some(mime.into()),
            byte_length: Some(bytes.len() as u64),
            advertised_byte_length: None,
            auto_download_allowed: Some(false),
            sha256: Some(digest(bytes)),
            filename: Some(filename.into()),
            expires_at: None,
            reason: None,
            retryable: false,
            job_id: Some(job.into()),
            content_url: Some(format!("/v1/accounts/{account}/media/{id}/content")),
            animation_frames: (frames > 0).then_some(frames),
            official_writeback: "not_applicable".into(),
            preview: None,
        };
        if frames > 0 {
            if let Ok(preview) = wechat_media::sticker_preview(bytes) {
                let sheet = preview.contact_sheet;
                let extension = if sheet.inspection.content_type == "image/png" {
                    "png"
                } else {
                    "jpg"
                };
                asset.preview = Some(MediaImagePreview {
                    source_sha256: digest(bytes),
                    content_type: sheet.inspection.content_type,
                    byte_length: sheet.bytes.len() as u64,
                    sha256: digest(&sheet.bytes),
                    filename: format!("{id}.preview.{extension}"),
                    content_url: format!(
                        "/v1/accounts/{account}/media/{id}/content?representation=preview"
                    ),
                });
            }
        }
        let mut metadata = serde_json::to_value(&asset)?;
        metadata["preparationExpiresAt"] = (chrono::Utc::now().timestamp() + 86400).into();
        let payload = self
            .vault
            .seal_media(account, &format!("asset:{id}"), bytes)?;
        let plan = self.vault.seal(
            account,
            &format!("plan:{id}"),
            &serde_json::to_vec(&identity::AuthorizedPlan {
                identity: None,
                plan: DownloadPlan::CachedOnly,
            })?,
        )?;
        self.store.put_media_asset(
            account,
            &MediaAssetRecord {
                id: id.into(),
                message_id: None,
                variant: "original".into(),
                kind: if frames == 0 { "file" } else { "sticker" }.into(),
                metadata: metadata.clone(),
                plan,
                payload: Some(payload.clone()),
            },
        )?;
        self.store
            .save_media_asset(account, id, &metadata, Some(&payload))?;
        Ok(asset)
    }
    fn finish_pack_content(
        &self,
        account: &str,
        job: &str,
        plan: &Acquisition,
        bytes: &[u8],
    ) -> Result<()> {
        let detail = plan
            .detail
            .as_ref()
            .context("sticker_metadata_incomplete")?;
        let ids = detail
            .items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let archive = wechat_media::decode_sticker_archive(bytes, &plan.pack, &ids)?;
        self.finish_verified_pack_content(account, job, plan, archive)
    }
    fn finish_verified_pack_content(
        &self,
        account: &str,
        job: &str,
        plan: &Acquisition,
        archive: wechat_media::StickerArchive,
    ) -> Result<()> {
        let added = self
            .profile_reader_owned(account)?
            .installed_sticker_packs()?
            .iter()
            .any(|p| p.id == plan.pack);
        let mut originals = vec![];
        let mut thumbnails = vec![];
        let mut rows = vec![];
        for item in &archive.items {
            rows.push(wechat_profile::StickerPackFileRecord {
                id: item.id.clone(),
                file_type: item.file_type,
                sort_order: i64::from(item.ordinal),
                original_size: item.original.len() as u32,
                original_offset: originals.len() as u32,
                thumbnail_size: item.thumbnail.len() as u32,
                thumbnail_offset: thumbnails.len() as u32,
            });
            originals.extend(&item.original);
            thumbnails.extend(&item.thumbnail);
        }
        if added {
            let identity = self.profile_identity(account)?;
            let (location, keys) = self.profile_location_keys(account)?;
            let options = self.write_options(account, job)?;
            for (kind, data) in [
                (Cache::Original, originals.as_slice()),
                (Cache::Thumbnail, thumbnails.as_slice()),
                (Cache::Icon, archive.icon.as_slice()),
            ] {
                wechat_profile::apply_sticker_pack_cache(
                    &location,
                    identity.uin,
                    &identity.username,
                    &plan.pack,
                    kind,
                    data,
                    &options,
                )?;
                ensure!(
                    wechat_profile::read_sticker_pack_cache(
                        &location,
                        identity.uin,
                        &identity.username,
                        &plan.pack,
                        kind
                    )? == data,
                    "media_integrity_failed"
                );
            }
            wechat_profile::apply_sticker_pack_content(
                &location, &keys, &plan.pack, &rows, &options,
            )?;
            let captions = plan
                .captions
                .iter()
                .flat_map(|r| &r.items)
                .flat_map(|r| {
                    r.captions.iter().map(|(language, caption)| {
                        wechat_profile::StickerPackCaptionRecord {
                            id: r.id.clone(),
                            language: language.clone(),
                            caption: caption.clone(),
                        }
                    })
                })
                .collect::<Vec<_>>();
            wechat_profile::apply_sticker_pack_captions(
                &location, &keys, &plan.pack, &captions, &options,
            )?;
        }
        let mut output = vec![];
        for item in archive.items {
            if plan
                .item
                .as_ref()
                .is_some_and(|selected| selected != &item.id)
            {
                continue;
            }
            let inspection = wechat_media::inspect_image(&item.original)?;
            let extension = match inspection.content_type.as_str() {
                "image/gif" => "gif",
                "image/png" => "png",
                "image/jpeg" => "jpg",
                _ => bail!("unsupported_sticker_archive"),
            };
            let asset = self.store_pack_bytes(
                account,
                job,
                &pack_asset_id(&plan.pack, &item.id, "original"),
                &format!("{}.{extension}", item.id),
                &item.original,
                &inspection.content_type,
                inspection.frames,
            )?;
            let thumb = wechat_media::inspect_image(&item.thumbnail)?;
            self.store_pack_bytes(
                account,
                job,
                &pack_asset_id(&plan.pack, &item.id, "thumbnail"),
                &format!(
                    "{}.thumb.{}",
                    item.id,
                    if thumb.content_type == "image/png" {
                        "png"
                    } else {
                        "jpg"
                    }
                ),
                &item.thumbnail,
                &thumb.content_type,
                thumb.frames,
            )?;
            if added {
                self.pack_profile_asset(account, &plan.pack, &item.id, "original", &asset)?;
                let id = pack_asset_id(&plan.pack, &item.id, "thumbnail");
                self.pack_profile_asset(
                    account,
                    &plan.pack,
                    &item.id,
                    "thumbnail",
                    &self.media_asset(account, &id)?,
                )?;
            }
            output.push(asset);
        }
        let icon_inspection = wechat_media::inspect_image(&archive.icon)?;
        let icon = self.store_pack_bytes(
            account,
            job,
            &pack_asset_id(&plan.pack, "icon", "icon"),
            "icon.png",
            &archive.icon,
            &icon_inspection.content_type,
            1,
        )?;
        if added {
            self.pack_profile_asset(account, &plan.pack, "icon", "icon", &icon)?;
        }
        let mut row = self.store.media_job(account, job)?;
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        row.data["profileWriteback"] = if added { "complete" } else { "not_applicable" }.into();
        row.data["result"] = json!({"packId":plan.pack,"serverAcceptance":"accepted","profileWriteback":row.data["profileWriteback"],"membership":if added {"added"}else{"not_added"},"contentReadiness":"available","items":output});
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    fn pack_profile_asset(
        &self,
        account: &str,
        pack: &str,
        item: &str,
        role: &str,
        asset: &MediaAsset,
    ) -> Result<()> {
        let mut asset = asset.clone();
        asset.official_writeback = "complete".into();
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&(pack, item, role))?);
        self.store.save_media_asset_in_profile(
            account,
            &asset.id,
            &serde_json::to_value(&asset)?,
            &format!("sticker-pack:{encoded}"),
        )?;
        Ok(())
    }
    pub(super) fn pack_cache_item(
        &self,
        account: &str,
        pack: &str,
        item: &str,
        role: &str,
    ) -> Result<Vec<u8>> {
        let identity = self.profile_identity(account)?;
        let (location, _) = self.profile_location_keys(account)?;
        let kind = match role {
            "original" => Cache::Original,
            "thumbnail" => Cache::Thumbnail,
            "icon" => Cache::Icon,
            _ => bail!("unsupported_media_variant"),
        };
        let bytes = wechat_profile::read_sticker_pack_cache(
            &location,
            identity.uin,
            &identity.username,
            pack,
            kind,
        )?;
        if role == "icon" {
            wechat_media::inspect_image(&bytes)?;
            return Ok(bytes);
        }
        let row = self
            .profile_reader_owned(account)?
            .sticker_pack_files(pack)?
            .into_iter()
            .find(|r| r.id == item)
            .context("sticker_pack_item_not_found")?;
        let (offset, size) = if role == "original" {
            (row.original_offset, row.original_size)
        } else {
            (row.thumbnail_offset, row.thumbnail_size)
        };
        let end = offset.checked_add(size).context("media_integrity_failed")?;
        let slice = bytes
            .get(offset as usize..end as usize)
            .context("media_integrity_failed")?;
        if role == "original" {
            use md5::Md5;
            ensure!(
                format!("{:x}", Md5::digest(slice)) == item,
                "media_integrity_failed"
            );
        }
        wechat_media::inspect_image(slice)?;
        Ok(slice.to_vec())
    }
    pub(super) fn repair_sticker_pack_download(&self, account: &str, id: &str) -> Result<bool> {
        let plan = self.acquisition(account, id)?;
        ensure!(plan.locator.is_some(), "sticker_submission_needs_review");
        let bytes = self
            .media_content_owned(account, &format!("{id}_archive"), true)?
            .bytes;
        self.finish_pack_content(account, id, &plan, &bytes)?;
        Ok(true)
    }
    pub(super) fn promote_owned_sticker_pack(&self, account: &str, pack: &str) -> Result<()> {
        for id in self.store.completed_sticker_acquisitions(account, pack)? {
            let Ok(bytes) = self.media_content(account, &format!("{id}_archive")) else {
                continue;
            };
            let plan = self.acquisition(account, &id)?;
            ensure!(
                plan.pack == pack && plan.locator.is_some(),
                "sticker_pack_identity_mismatch"
            );
            let mut row = self.store.media_job(account, &id)?;
            row.data["profileWriteback"] = "pending".into();
            self.store.save_media_job(account, &row)?;
            if self
                .finish_pack_content(account, &id, &plan, &bytes.bytes)
                .is_err()
            {
                let mut row = self.store.media_job(account, &id)?;
                row.data["profileWriteback"] = "blocked".into();
                row.data["result"]["profileWriteback"] = "blocked".into();
                self.store.save_media_job(account, &row)?;
            }
            return Ok(());
        }
        Ok(())
    }
    pub(super) fn indexed_pack_asset(
        &self,
        account: &str,
        pack: &str,
        item: &str,
        role: &str,
    ) -> Option<MediaAsset> {
        let id = pack_asset_id(pack, item, role);
        if let Ok(asset) = self.media_asset(account, &id) {
            if asset.state == "available" && self.media_content(account, &id).is_ok() {
                return Some(asset);
            }
        }
        let bytes = self.pack_cache_item(account, pack, item, role).ok()?;
        let image = wechat_media::inspect_image(&bytes).ok()?;
        let extension = match image.content_type.as_str() {
            "image/gif" => "gif",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => return None,
        };
        let asset = self
            .store_pack_bytes(
                account,
                "profile-index",
                &id,
                &format!("{item}.{extension}"),
                &bytes,
                &image.content_type,
                image.frames,
            )
            .ok()?;
        self.pack_profile_asset(account, pack, item, role, &asset)
            .ok()?;
        self.media_asset(account, &id).ok()
    }
}
