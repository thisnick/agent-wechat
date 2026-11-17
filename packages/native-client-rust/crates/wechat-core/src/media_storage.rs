//! Where attachment bytes are kept.
//!
//! An image, file, video or voice note that was written into the official
//! profile is read back from there; this store keeps no second copy. Media the
//! profile has no supported place for (received sticker attachments, link thumbnails, quoted images) stays in
//! an encrypted file owned by the account. A copy is released only after the
//! profile returned the same bytes through the reader that later serves them.
//!
//! `maintain_media_storage` moves payloads that earlier versions stored inside
//! `control.sqlite`, deletes uploads no send job needs any more, and removes
//! files nothing refers to. It rewrites sealed plans that repeat bytes the
//! account keeps elsewhere (see `media_plan_compaction.rs`) and runs sidecar
//! retention (see `wechat_store`'s retention module), which reduces finished
//! records to their receipts. It never downloads, uploads or resends anything.
use super::*;
use std::collections::BTreeSet;

/// A finished send keeps its upload this long, so a client that reuses the
/// source ID shortly after a send still finds it.
const UPLOAD_GRACE_SECONDS: i64 = 24 * 60 * 60;
/// Profile key for media located through its message, not a content hash.
const BY_MESSAGE: &str = "message";
/// Upper bound on retention steps in one pass; the next pass continues.
const RETENTION_STEPS: usize = 10_000;

impl Core {
    /// Read one attachment from the official profile and check it against the
    /// size and hash recorded when it was first verified. A profile file that
    /// was deleted or changed is an error; nothing is fetched to replace it.
    pub(super) fn profile_media_bytes(
        &self,
        account: &str,
        asset: &MediaAsset,
        key: &str,
    ) -> Result<Vec<u8>> {
        if let Some(reference) = key.strip_prefix("sticker-message:") {
            let (month, id) = reference
                .split_once(':')
                .context("unsupported_media_profile")?;
            let identity = self.profile_identity(account)?;
            let (location, _) = self.profile_location_keys(account)?;
            let bytes = wechat_profile::read_message_sticker_cache(
                &location,
                identity.uin,
                &identity.username,
                month,
                id,
            )?;
            ensure!(
                asset.kind == MediaKind::Sticker
                    && asset.byte_length == Some(bytes.len() as u64)
                    && asset.sha256.as_deref() == Some(digest(&bytes).as_str()),
                "media_integrity_failed"
            );
            return Ok(bytes);
        }
        if asset.kind == MediaKind::Sticker
            && asset.message_id.is_none()
            && (key.starts_with("sticker-saved:") || key.starts_with("sticker-pack:"))
        {
            let bytes = if let Some(id) = key.strip_prefix("sticker-saved:") {
                self.saved_cache_bytes(account, id)?
            } else if let Some(encoded) = key.strip_prefix("sticker-pack:") {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded)?;
                let (pack, item, role): (String, String, String) = serde_json::from_slice(&bytes)?;
                self.pack_cache_item(account, &pack, &item, &role)?
            } else {
                bail!("unsupported_media_profile");
            };
            ensure!(
                asset.byte_length == Some(bytes.len() as u64)
                    && asset.sha256.as_deref() == Some(digest(&bytes).as_str()),
                "media_integrity_failed"
            );
            return Ok(bytes);
        }
        let read = || -> Result<Vec<u8>> {
            let source = source_message_id(
                asset
                    .message_id
                    .as_deref()
                    .context("media_message_missing")?,
            )?;
            let reader = self.profile_reader_owned(account)?;
            Ok(match asset.kind {
                MediaKind::Image => {
                    let quality = match asset.variant {
                        MediaVariant::Original => wechat_profile::ImageQuality::Original,
                        MediaVariant::Standard => wechat_profile::ImageQuality::Standard,
                        MediaVariant::Thumbnail => wechat_profile::ImageQuality::Thumbnail,
                        _ => bail!("unsupported_media_variant"),
                    };
                    let cached = reader.cached_image(&source, quality)?;
                    let verified = match cached.quality {
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
                    };
                    ensure!(verified == Some(asset.variant), "unsupported_media_variant");
                    cached.media.bytes
                }
                MediaKind::File => {
                    reader
                        .cached_hardlink_file(wechat_profile::HardlinkKind::File, key)?
                        .bytes
                }
                MediaKind::Video => {
                    reader
                        .cached_hardlink_file(wechat_profile::HardlinkKind::Video, key)?
                        .bytes
                }
                MediaKind::Voice => {
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
                    let encoded = reader
                        .cached_voice(&shards[0].database, chat, &source.local_id, Some(server))?
                        .bytes;
                    match asset.variant {
                        MediaVariant::Encoded => encoded,
                        MediaVariant::Normal => {
                            wechat_protocol::silk::wav(&wechat_protocol::silk::decode(&encoded)?)?
                        }
                        _ => bail!("unsupported_media_variant"),
                    }
                }
                MediaKind::Sticker => bail!("unsupported_media_variant"),
            })
        };
        let bytes = read().map_err(|_| anyhow::anyhow!("media_not_cached"))?;
        ensure!(
            asset.byte_length == Some(bytes.len() as u64)
                && asset.sha256.as_deref() == Some(digest(&bytes).as_str()),
            "media_integrity_failed"
        );
        Ok(bytes)
    }
    /// Stop keeping our own copy of an attachment once the official profile
    /// holds it and returns exactly `bytes`. Returns whether the copy was
    /// released; on any doubt the copy stays.
    pub(super) fn release_to_profile(
        &self,
        account: &str,
        asset: &MediaAsset,
        bytes: &[u8],
    ) -> Result<bool> {
        self.release_to_profile_payload(account, asset, Payload::Bytes(bytes))
    }
    /// [`Self::release_to_profile`] for a payload that may be a large file. A
    /// file or video in the profile is checked in steps, not read whole.
    pub(super) fn release_to_profile_payload(
        &self,
        account: &str,
        asset: &MediaAsset,
        payload: Payload<'_>,
    ) -> Result<bool> {
        if asset.state != "available"
            || asset.official_writeback != "complete"
            || asset.actual_variant != Some(asset.variant)
            || asset.message_id.is_none()
            || asset.byte_length != Some(payload.len())
            || asset.sha256.as_deref() != Some(payload.sha256().as_str())
        {
            return Ok(false);
        }
        let key = match asset.kind {
            MediaKind::File | MediaKind::Video => payload.md5(),
            MediaKind::Image | MediaKind::Voice => BY_MESSAGE.into(),
            MediaKind::Sticker => return Ok(false),
        };
        let held = match asset.kind {
            MediaKind::File | MediaKind::Video => {
                self.profile_hardlink(account, asset, &key).is_ok()
            }
            _ => self.profile_media_bytes(account, asset, &key).is_ok(),
        };
        if !held {
            return Ok(false);
        }
        self.authorize_media_record(account, &self.store.media_asset(account, &asset.id)?)?;
        self.store.save_media_asset_in_profile(
            account,
            &asset.id,
            &serde_json::to_value(asset)?,
            &key,
        )?;
        Ok(true)
    }
    /// Locate a file or video attachment in the official profile and check it
    /// against the size and hash recorded when it was first verified, reading
    /// it in steps. A deleted or changed file is an error.
    pub(super) fn profile_hardlink(
        &self,
        account: &str,
        asset: &MediaAsset,
        key: &str,
    ) -> Result<wechat_profile::VerifiedHardlink> {
        let kind = match asset.kind {
            MediaKind::File => wechat_profile::HardlinkKind::File,
            MediaKind::Video => wechat_profile::HardlinkKind::Video,
            _ => bail!("unsupported_media_variant"),
        };
        let verified = self
            .profile_reader_owned(account)?
            .verified_hardlink_file(kind, key)
            .map_err(|_| anyhow::anyhow!("media_not_cached"))?;
        ensure!(
            asset.byte_length == Some(verified.byte_length)
                && asset.sha256.as_deref() == Some(verified.sha256.as_str()),
            "media_integrity_failed"
        );
        Ok(verified)
    }
    /// Decode the still preview of a WXGF image. Previews are made when asked
    /// for and are not stored.
    pub(super) fn wxgf_preview(bytes: &[u8]) -> Result<wechat_media::DecodedImage> {
        let decoded = wechat_media::wxgf_preview(bytes)?;
        ensure!(
            ["image/jpeg", "image/png"].contains(&decoded.inspection.content_type.as_str())
                && !decoded.bytes.is_empty()
                && decoded.bytes.len() <= 16 * 1024 * 1024,
            "image_decode_failed"
        );
        Ok(decoded)
    }
    /// The whole storage pass for one account. Safe to repeat and safe after
    /// logout: it only moves or deletes bytes this account already owns.
    pub fn maintain_media_storage(&self, account: &str) -> Result<serde_json::Value> {
        let swept = self.store.sweep_media_files(account)?;
        let (mut moved, mut released, mut removed_previews) = (0, 0, 0);
        loop {
            let batch = self.move_embedded_payloads(account, 50)?;
            moved += batch.0;
            released += batch.1;
            removed_previews += batch.2;
            if batch == (0, 0, 0) {
                break;
            }
        }
        // Uploads are released first: retention compacts a send only once its
        // upload is gone.
        let (previews, uploads, compacted) = self.finish_media_storage(account)?;
        let plans = self.run_plan_compaction(account)?;
        let retention = self.run_retention(account)?;
        let compacted = self.store.compact_control(account).unwrap_or(false) || compacted;
        removed_previews += previews;
        Ok(
            json!({"sweptFiles":swept,"movedPayloads":moved,"releasedToProfile":released,"removedPreviews":removed_previews,"removedUploads":uploads,"compacted":compacted,"compactedPlans":plans,"retention":retention}),
        )
    }
    /// Run sidecar retention for one account until no step has work left.
    fn run_retention(&self, account: &str) -> Result<wechat_store::RetentionStep> {
        let now = chrono::Utc::now().timestamp();
        let mut total = wechat_store::RetentionStep::default();
        let mut after = 0;
        for _ in 0..RETENTION_STEPS {
            let step = self.store.retention_step(account, now, after)?;
            total.add(&step);
            after = step.runtime_after;
            if !step.more {
                break;
            }
        }
        Ok(total)
    }
    /// Run retention one small step at a time under the account's mutation
    /// guard, so other work for the account interleaves, then give freed
    /// pages back to the file system.
    async fn guarded_retention(self: &Arc<Self>, account: &str) {
        let now = chrono::Utc::now().timestamp();
        let mut after = 0;
        for _ in 0..RETENTION_STEPS {
            let Some(step) = self
                .guarded_storage_step(account, move |core, id| {
                    core.store.retention_step(id, now, after)
                })
                .await
            else {
                break;
            };
            after = step.runtime_after;
            if !step.more || self.stopping.load(Ordering::Acquire) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let _ = self
            .guarded_storage_step(account, |core, id| core.store.compact_control(id))
            .await;
    }
    /// Handle up to `limit` payloads that an earlier version stored inside the
    /// database. Returns how many moved to a file, how many were dropped
    /// because the profile holds them, and how many stored previews were
    /// deleted. All zero means none remain.
    fn move_embedded_payloads(&self, account: &str, limit: usize) -> Result<(usize, usize, usize)> {
        let embedded = self.store.embedded_media_payloads(account, limit)?;
        let (mut moved, mut released, mut previews) = (0, 0, 0);
        for (family, id) in &embedded {
            if family == "source" && id.starts_with(WXGF_PREVIEW_PREFIX) {
                // Previews are decoded on request now; this one is not moved.
                if self.store.delete_media_source(account, id)? {
                    previews += 1;
                }
            } else if family == "asset" && self.release_embedded_asset(account, id).unwrap_or(false)
            {
                released += 1;
            } else if self.store.externalize_media_payload(account, family, id)? {
                moved += 1;
            }
        }
        Ok((moved, released, previews))
    }
    /// Delete stored previews and uploads nothing needs, then let the database
    /// file shrink. Returns the previews removed, uploads removed and whether
    /// the database was rewritten.
    fn finish_media_storage(&self, account: &str) -> Result<(usize, usize, bool)> {
        let mut removed_previews = 0;
        loop {
            let previews =
                self.store
                    .media_source_ids_with_prefix(account, WXGF_PREVIEW_PREFIX, 100)?;
            for id in &previews {
                if self.store.delete_media_source(account, id)? {
                    removed_previews += 1;
                }
            }
            if previews.len() < 100 {
                break;
            }
        }
        self.store.expire_sticker_preparation_assets(
            account,
            chrono::Utc::now().timestamp(),
            1000,
        )?;
        let uploads = self.release_finished_uploads(account)?;
        let compacted = self.store.compact_control(account).unwrap_or(false);
        Ok((removed_previews, uploads, compacted))
    }
    /// An earlier version's database copy of an attachment the profile already
    /// holds is dropped instead of being moved to a file.
    fn release_embedded_asset(&self, account: &str, id: &str) -> Result<bool> {
        let record = self.store.media_asset_with_payload(account, id)?;
        self.authorize_media_record(account, &record)?;
        let asset: MediaAsset = serde_json::from_value(record.metadata)?;
        let sealed = record.payload.context("media_not_cached")?;
        let bytes = self
            .vault
            .open_media_record(account, &format!("asset:{id}"), &sealed)?;
        self.release_to_profile(account, &asset, &bytes)
    }
    /// Uploads named by send jobs and sticker preparations at least `grace`
    /// seconds old, split into those an unfinished job still needs and those
    /// only finished jobs used. `None` means a job's needs cannot be read, so
    /// nothing may be released.
    fn upload_use(
        &self,
        account: &str,
        grace: i64,
    ) -> Result<Option<(BTreeSet<String>, BTreeSet<String>)>> {
        let jobs = self.store.send_jobs_for_source_release(account, grace)?;
        let mut needed = BTreeSet::new();
        let mut finished = BTreeSet::new();
        for (id, done) in jobs {
            let row = self.store.media_job(account, &id)?;
            let plan = self
                .vault
                .open_record(account, &format!("media-job:{id}"), &row.payload)
                .ok()
                .and_then(|plain| serde_json::from_slice::<SendPlan>(&plain).ok());
            let Some(plan) = plan else {
                // Without its plan a job's uploads are unknown. Keep everything
                // when that job may still need them; a finished one still
                // releases the upload its record names.
                if done {
                    finished.extend(row.data["sourceId"].as_str().map(str::to_owned));
                    continue;
                }
                return Ok(None);
            };
            let ids = [
                Some(plan.request.source_id.clone()),
                row.data["sourceId"].as_str().map(str::to_owned),
                plan.request.standard_source_id.clone(),
                plan.request.thumbnail_source_id.clone(),
                plan.prepared_video_thumbnail_source_id.clone(),
                plan.prepared_video_source_id.clone(),
            ];
            let set = if done { &mut finished } else { &mut needed };
            set.extend(ids.into_iter().flatten());
        }
        for (source, done) in self
            .store
            .sticker_preparation_sources_for_release(account, grace)?
        {
            if done {
                finished.insert(source);
            } else {
                needed.insert(source);
            }
        }
        Ok(Some((needed, finished)))
    }
    /// Delete one upload now, for a caller that has finished with it, such as
    /// after adding the sticker it was prepared into. An upload that an
    /// unfinished send or preparation still needs is kept: `upload_in_use`.
    /// Returns false when the upload no longer exists.
    pub fn release_upload(&self, account: &str, id: &str) -> Result<bool> {
        self.store.account(account)?;
        let (needed, _) = self.upload_use(account, 0)?.context("upload_in_use")?;
        ensure!(!needed.contains(id), "upload_in_use");
        self.store.delete_media_source(account, id)
    }
    /// Delete uploads whose every send job finished at least a day ago with
    /// nothing left to retry, review or write to the profile. An upload that no
    /// job has used yet is kept.
    fn release_finished_uploads(&self, account: &str) -> Result<usize> {
        let Some((needed, finished)) = self.upload_use(account, UPLOAD_GRACE_SECONDS)? else {
            return Ok(0);
        };
        let mut removed = 0;
        for id in finished.difference(&needed) {
            if self.store.delete_media_source(account, id)? {
                removed += 1;
            }
        }
        Ok(removed)
    }
    /// Run the storage pass for every account in the background, once per
    /// process. The account's mutation guard is taken for one small batch at a
    /// time, so converting a large earlier database does not hold up other work.
    pub(crate) fn spawn_media_storage_maintenance(self: &Arc<Self>) -> Result<()> {
        if self.media_storage_maintained.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let accounts = self.store.accounts()?;
        let core = self.clone();
        tokio::spawn(async move {
            for account in accounts {
                let id = account.id;
                let _ = core
                    .guarded_storage_step(&id, |core, id| core.store.sweep_media_files(id))
                    .await;
                while core
                    .guarded_storage_step(&id, |core, id| core.move_embedded_payloads(id, 8))
                    .await
                    .is_some_and(|batch| batch != (0, 0, 0))
                {
                    tokio::task::yield_now().await;
                }
                let _ = core
                    .guarded_storage_step(&id, |core, id| core.finish_media_storage(id))
                    .await;
                core.guarded_plan_compaction(&id).await;
                core.guarded_retention(&id).await;
            }
            // Release finished uploads and expired preparations and run
            // retention every hour, so nothing waits for the next restart.
            let mut hourly = tokio::time::interval(std::time::Duration::from_secs(3600));
            hourly.tick().await;
            loop {
                hourly.tick().await;
                if core.stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(accounts) = core.store.accounts() else {
                    continue;
                };
                for account in accounts {
                    let _ = core
                        .guarded_storage_step(&account.id, |core, id| {
                            core.store.expire_sticker_preparation_assets(
                                id,
                                chrono::Utc::now().timestamp(),
                                1000,
                            )?;
                            core.release_finished_uploads(id)
                        })
                        .await;
                    core.guarded_plan_compaction(&account.id).await;
                    core.guarded_retention(&account.id).await;
                }
            }
        });
        Ok(())
    }
    /// One blocking storage step under the account's mutation guard. `None`
    /// when the step failed; the next start tries again.
    pub(super) async fn guarded_storage_step<T: Send + 'static>(
        self: &Arc<Self>,
        account: &str,
        step: impl FnOnce(&Core, &str) -> Result<T> + Send + 'static,
    ) -> Option<T> {
        let _guard = self.lock_account(account).await.ok()?;
        let core = self.clone();
        let account = account.to_owned();
        tokio::task::spawn_blocking(move || step(&core, &account))
            .await
            .ok()?
            .ok()
    }
}
