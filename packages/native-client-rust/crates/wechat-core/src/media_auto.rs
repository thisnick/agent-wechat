//! Receipt-time voice caching, independent of API clients and UI visibility.
use super::*;

const PREFIX: &str = "received-voice:";

#[derive(Serialize, Deserialize)]
struct PendingVoice {
    request: MediaTransferRequest,
    attempts: u8,
}

impl Core {
    pub(crate) fn queue_received_voice(&self, account: &str, message: &str) -> Result<()> {
        let discovered = self.media_assets(account, message);
        let assets = match discovered {
            Ok(assets) => assets,
            Err(_) => {
                self.store.append_event_once(
                    account,
                    &format!("auto-voice-unavailable:{message}"),
                    "media.auto-download",
                    &json!({"messageId":message,"kind":"voice","state":"unavailable"}),
                )?;
                return Ok(());
            }
        };
        let Some(asset) = assets
            .into_iter()
            .find(|asset| asset.kind == MediaKind::Voice && asset.variant == MediaVariant::Normal)
        else {
            return Ok(());
        };
        if asset.state == "available" && asset.actual_variant == Some(MediaVariant::Normal) {
            return Ok(());
        }
        // The asset's sealed plan binds the source row, chat, server ID and content.
        let key = format!("{PREFIX}{}", asset.id);
        if self.store.runtime(account, &key)?.is_none() {
            let pending = PendingVoice {
                request: MediaTransferRequest {
                    message_id: message.into(),
                    asset_id: Some(asset.id),
                    quality: MediaVariant::Normal,
                },
                attempts: 0,
            };
            self.store
                .put_runtime(account, &key, &serde_json::to_vec(&pending)?)?;
        }
        Ok(())
    }

    pub(crate) fn admit_received_voice(self: &Arc<Self>, account: &str) -> Result<usize> {
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
        for (key, bytes) in self.store.runtime_prefix(account, PREFIX, 4)? {
            let mut pending: PendingVoice = serde_json::from_slice(&bytes)?;
            let message_id = pending.request.message_id.clone();
            let outcome = (|| {
                ensure!(
                    pending.request.quality == MediaVariant::Normal,
                    "invalid_automatic_voice"
                );
                let asset = self.media_asset(
                    account,
                    pending
                        .request
                        .asset_id
                        .as_deref()
                        .context("invalid_automatic_voice")?,
                )?;
                ensure!(
                    asset.kind == MediaKind::Voice && asset.variant == MediaVariant::Normal,
                    "invalid_automatic_voice"
                );
                self.start_media_transfer(account, pending.request.clone())
            })();
            match outcome {
                Ok(job) => {
                    self.store.append_event_once(account, &format!("auto-voice-job:{}", job.id), "media.auto-download", &json!({"messageId":message_id,"kind":"voice","jobId":job.id,"state":job.state}))?;
                    self.store.clear_runtime(account, &key)?;
                    admitted += 1;
                }
                Err(error) if error.to_string() == "media_queue_full" => break,
                Err(_) => {
                    pending.attempts = pending.attempts.saturating_add(1);
                    if pending.attempts >= 3 {
                        self.store.append_event_once(
                            account,
                            &format!("auto-voice-unavailable:{message_id}"),
                            "media.auto-download",
                            &json!({"messageId":message_id,"kind":"voice","state":"unavailable"}),
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

    pub(crate) fn start_received_voice_worker(self: &Arc<Self>, account: &str) {
        let core = self.clone();
        let account = account.to_owned();
        tokio::spawn(async move {
            loop {
                if core.stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(guard) = core.lock_account(&account).await {
                    let local = core.clone();
                    let account = account.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _guard = guard;
                        local.admit_received_voice(&account)
                    })
                    .await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }
}
