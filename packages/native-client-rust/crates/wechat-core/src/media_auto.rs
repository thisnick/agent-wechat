//! Receipt-time hub downloads, independent of API clients and UI visibility.
use super::*;

// Keep the existing voice queue readable across daemon upgrades.
const VOICE_PREFIX: &str = "received-voice:";
const MEDIA_PREFIX: &str = "received-media:";

pub(super) fn advertised_length(plan: &DownloadPlan) -> Option<u64> {
    match plan {
        DownloadPlan::Cdn { descriptor } => descriptor.expected_bytes,
        DownloadPlan::Voice { length, .. } => Some(u64::from(*length)),
        DownloadPlan::Sticker { descriptor } => Some(descriptor.expected_bytes),
        _ => None,
    }
}
pub(super) fn allowed(asset: &MediaAsset, limit: u64) -> bool {
    if matches!(
        asset.kind,
        MediaKind::Image | MediaKind::Video | MediaKind::Sticker
    ) && asset.variant == MediaVariant::Thumbnail
    {
        return true;
    }
    if asset.kind == MediaKind::Voice {
        return asset.variant == MediaVariant::Normal;
    }
    matches!(
        (asset.kind, asset.variant),
        (
            MediaKind::Image,
            MediaVariant::Standard | MediaVariant::Original
        ) | (
            MediaKind::Video,
            MediaVariant::Normal | MediaVariant::Original
        ) | (MediaKind::File, MediaVariant::Original)
            | (MediaKind::Sticker, MediaVariant::Original)
    ) && limit > 0
        && asset
            .advertised_byte_length
            .is_some_and(|size| size > 0 && size <= limit)
}

#[derive(Serialize, Deserialize)]
struct PendingMedia {
    request: MediaTransferRequest,
    attempts: u8,
}

impl Core {
    pub(crate) fn queue_received_media(&self, account: &str, message: &str) -> Result<()> {
        let assets = match self.media_assets(account, message) {
            Ok(assets) => assets,
            Err(error) => {
                if error.to_string() != "unsupported_application_media" {
                    self.store.append_event_once(
                        account,
                        &format!("auto-media-unavailable:{message}"),
                        "media.auto-download",
                        &json!({"messageId":message,"state":"unavailable"}),
                    )?;
                }
                return Ok(());
            }
        };
        for asset in assets {
            if asset.auto_download_allowed != Some(true)
                || (asset.state == "available" && asset.actual_variant == Some(asset.variant))
            {
                continue;
            }
            let prefix = if asset.kind == MediaKind::Voice {
                VOICE_PREFIX
            } else {
                MEDIA_PREFIX
            };
            // The sealed plan binds the source row, chat, server ID and content.
            let key = format!("{prefix}{}", asset.id);
            if self.store.runtime(account, &key)?.is_none() {
                let pending = PendingMedia {
                    request: MediaTransferRequest {
                        message_id: message.into(),
                        asset_id: Some(asset.id),
                        quality: asset.variant,
                    },
                    attempts: 0,
                };
                self.store
                    .put_runtime(account, &key, &serde_json::to_vec(&pending)?)?;
            }
        }
        Ok(())
    }

    pub(crate) fn admit_received_media(self: &Arc<Self>, account: &str) -> Result<usize> {
        if self.stopping.load(Ordering::SeqCst)
            || self.media_network_suspended(account)?
            || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            || self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                != Some(self.ownership_generation.as_bytes())
        {
            return Ok(0);
        }
        let mut admitted = 0;
        let mut pending_rows = self.store.runtime_prefix(account, VOICE_PREFIX, 4)?;
        pending_rows.extend(self.store.runtime_prefix(account, MEDIA_PREFIX, 4)?);
        for (key, bytes) in pending_rows {
            let mut pending: PendingMedia = serde_json::from_slice(&bytes)?;
            let message_id = pending.request.message_id.clone();
            let outcome = (|| {
                let asset = self.media_asset(
                    account,
                    pending
                        .request
                        .asset_id
                        .as_deref()
                        .context("invalid_automatic_media")?,
                )?;
                ensure!(
                    asset.message_id.as_deref() == Some(message_id.as_str())
                        && asset.variant == pending.request.quality,
                    "invalid_automatic_media"
                );
                if asset.auto_download_allowed != Some(true) {
                    return Ok(None);
                }
                self.start_media_transfer(account, pending.request.clone())
                    .map(|job| Some((job, asset.kind)))
            })();
            match outcome {
                Ok(Some((job, kind))) => {
                    self.store.append_event_once(account, &format!("auto-media-job:{}", job.id), "media.auto-download", &json!({"messageId":message_id,"kind":kind,"quality":pending.request.quality,"jobId":job.id,"state":job.state}))?;
                    self.store.clear_runtime(account, &key)?;
                    admitted += 1;
                }
                Ok(None) => self.store.clear_runtime(account, &key)?,
                Err(error) if error.to_string() == "media_queue_full" => break,
                Err(_) => {
                    pending.attempts = pending.attempts.saturating_add(1);
                    if pending.attempts >= 3 {
                        self.store.append_event_once(
                            account,
                            &format!("auto-media-unavailable:{key}"),
                            "media.auto-download",
                            &json!({"messageId":message_id,"quality":pending.request.quality,"state":"unavailable"}),
                        )?;
                        self.store.clear_runtime(account, &key)?;
                    } else {
                        self.store
                            .put_runtime(account, &key, &serde_json::to_vec(&pending)?)?;
                    }
                }
            }
        }
        Ok(admitted)
    }

    pub(crate) fn start_received_media_worker(self: &Arc<Self>, account: &str) {
        let core = self.clone();
        let account = account.to_owned();
        tokio::spawn(async move {
            loop {
                if core.stopping.load(Ordering::SeqCst) || core.store.account(&account).is_err() {
                    break;
                }
                if let Ok(guard) = core.lock_account(&account).await {
                    let local = core.clone();
                    let account = account.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _guard = guard;
                        local.admit_received_media(&account)
                    })
                    .await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
}
