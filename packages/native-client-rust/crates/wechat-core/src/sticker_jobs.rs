//! Saved operations retain submissions and read receipts in existing media jobs.
use super::*;
use crate::{AddCustomStickerRequest, CustomStickerSource, MoveCustomStickerRequest};
use wechat_protocol::{custom_stickers as wire, protobuf as pb};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SavedIntent {
    Sync,
    Add {
        request: AddCustomStickerRequest,
    },
    Move {
        id: String,
        revision: String,
        request: MoveCustomStickerRequest,
    },
    Remove {
        id: String,
        revision: String,
    },
}
#[derive(Serialize, Deserialize)]
struct SavedJob {
    intent: SavedIntent,
    fence: MediaAuthorizationFence,
    // Operational receipts, never a second settled collection.
    reads: Vec<Vec<u8>>,
    replies: Vec<Vec<u8>>,
    upload_offset: u32,
    submitted: bool,
    accepted: bool,
    outcome: Option<(String, wire::CustomStickerMutation)>,
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid_sticker_id"
    );
    Ok(())
}
fn check_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 200
            && key.bytes().all(|b| b.is_ascii_graphic() || b == b' '),
        "invalid_idempotency_key"
    );
    Ok(())
}
fn placement_satisfied(
    order: &[String],
    identity: &String,
    action: &wire::CustomStickerMutation,
) -> bool {
    match action {
        wire::CustomStickerMutation::Add => order.contains(identity),
        wire::CustomStickerMutation::Remove => !order.contains(identity),
        wire::CustomStickerMutation::First => order.first() == Some(identity),
        wire::CustomStickerMutation::After { anchor, .. } => order
            .windows(2)
            .any(|pair| &pair[0] == anchor && &pair[1] == identity),
    }
}
impl Core {
    /// Request fresh Saved reconciliation. GET views never start this operation.
    pub fn sync_custom_stickers(self: &Arc<Self>, account: &str, key: &str) -> Result<MediaJob> {
        self.queue_saved_job(account, key, SavedIntent::Sync)
    }
    /// Save validated prepared media or a verified received original. The caller
    /// holds the account mutation gate during admission.
    pub fn add_custom_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        request: AddCustomStickerRequest,
    ) -> Result<MediaJob> {
        self.queue_saved_job(account, key, SavedIntent::Add { request })
    }
    /// Place one Saved item relative to another using an ordered-list revision.
    pub fn move_custom_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        id: &str,
        revision: &str,
        request: MoveCustomStickerRequest,
    ) -> Result<MediaJob> {
        valid_id(id)?;
        if let Some(anchor) = &request.position.after_id {
            valid_id(anchor)?;
            ensure!(
                !id.eq_ignore_ascii_case(anchor),
                "invalid_sticker_placement"
            );
        }
        self.queue_saved_job(
            account,
            key,
            SavedIntent::Move {
                id: id.to_ascii_lowercase(),
                revision: revision.into(),
                request,
            },
        )
    }
    /// Remove Saved membership asynchronously, preserving messages and unrelated media.
    pub fn remove_custom_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        id: &str,
        revision: &str,
    ) -> Result<MediaJob> {
        valid_id(id)?;
        self.queue_saved_job(
            account,
            key,
            SavedIntent::Remove {
                id: id.to_ascii_lowercase(),
                revision: revision.into(),
            },
        )
    }
    fn queue_saved_job(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        intent: SavedIntent,
    ) -> Result<MediaJob> {
        check_key(key)?;
        let hash = digest(&serde_json::to_vec(&intent)?);
        let key = format!("saved:{key}");
        // Return the original operation before checking an expired input or a
        // changed revision. Only a genuinely new operation needs admission.
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        let fence = self.media_authorization_fence(account)?;
        self.write_options(account, "saved-admission")?;
        match &intent {
            SavedIntent::Move { revision, .. } | SavedIntent::Remove { revision, .. } => {
                ensure!(
                    self.saved_stickers(account)?.revision == *revision,
                    "sticker_revision_mismatch"
                );
            }
            SavedIntent::Add { request } => {
                self.saved_source(account, request, None)?;
            }
            SavedIntent::Sync => {}
        }
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let preparation = match &intent {
            SavedIntent::Add {
                request:
                    AddCustomStickerRequest {
                        source: CustomStickerSource::Prepared { preparation_id },
                    },
            } => Some(preparation_id.clone()),
            _ => None,
        };
        let job = SavedJob {
            intent,
            fence,
            reads: vec![],
            replies: vec![],
            upload_offset: 0,
            submitted: false,
            accepted: false,
            outcome: None,
        };
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&job)?,
        )?;
        let (row, created) = self.store.create_media_job(account, &id, &key, &hash, "sticker_saved", &json!({"preparationId":preparation,"serverAcceptance":"pending","profileWriteback":"pending","result":{"serverAcceptance":"pending","profileWriteback":"pending"}}), &payload)?;
        if created {
            self.spawn_media_worker(account, &row.id);
        }
        Ok(public_job(account, &row))
    }
    fn saved_source(
        &self,
        account: &str,
        request: &AddCustomStickerRequest,
        consumer: Option<&str>,
    ) -> Result<(String, Vec<u8>, bool)> {
        let (asset, upload) = match &request.source {
            CustomStickerSource::Prepared { preparation_id } => {
                let job = if let Some(consumer) = consumer {
                    let pending = self.store.media_job(account, consumer)?;
                    ensure!(
                        pending.kind == "sticker_saved"
                            && pending.data["preparationId"].as_str() == Some(preparation_id)
                            && (pending.state == "running"
                                || pending.data["serverAcceptance"] == "accepted"),
                        "invalid_sticker_consumer"
                    );
                    let row = self.store.media_job(account, preparation_id)?;
                    ensure!(row.kind == "sticker_prepare", "invalid_sticker_source");
                    public_job(account, &row)
                } else {
                    self.sticker_preparation(account, preparation_id)?
                };
                ensure!(
                    job.state == "available",
                    "sticker_preparation_not_available"
                );
                let prepared: crate::PreparedSticker = serde_json::from_value(job.result)?;
                (prepared.original, true)
            }
            CustomStickerSource::Message {
                message_id,
                asset_id,
            } => {
                let row = self.store.media_asset(account, asset_id)?;
                ensure!(
                    row.message_id.as_deref() == Some(message_id),
                    "sticker_source_message_mismatch"
                );
                let asset = self.media_asset(account, asset_id)?;
                ensure!(
                    asset.kind == MediaKind::Sticker && asset.variant == MediaVariant::Original,
                    "invalid_sticker_source"
                );
                (asset, false)
            }
        };
        let content = self.media_content_owned(account, &asset.id, upload && consumer.is_some())?;
        let inspection = wechat_media::inspect_image(&content.bytes)?;
        ensure!(
            inspection.content_type != "image/x-wxgf",
            "unsupported_sticker_source"
        );
        ensure!(
            content.bytes.len() <= wechat_media::MAX_IMAGE_BYTES,
            "sticker_source_size_limit"
        );
        Ok((
            wechat_protocol::crypto::md5(&content.bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            content.bytes,
            upload,
        ))
    }
    fn save_saved_job(&self, account: &str, id: &str, job: &SavedJob) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.data["serverAcceptance"] = if job.accepted {
            "accepted"
        } else if job.submitted {
            "uncertain"
        } else {
            "pending"
        }
        .into();
        row.data["result"]["serverAcceptance"] = row.data["serverAcceptance"].clone();
        row.data["result"]["profileWriteback"] = row.data["profileWriteback"].clone();
        row.data["completedBytes"] = job.upload_offset.into();
        row.payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(job)?,
        )?;
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    fn retain_saved_read(
        &self,
        account: &str,
        id: &str,
        job: &mut SavedJob,
        raw: &[u8],
    ) -> Result<()> {
        ensure!(
            job.reads.len() < 250
                && job.reads.iter().map(Vec::len).sum::<usize>() + raw.len() <= 8 * 1024 * 1024,
            "sticker_receipt_limit"
        );
        job.reads.push(raw.to_vec());
        self.save_saved_job(account, id, job)
    }
    fn read_saved_order(
        &self,
        account: &str,
        id: &str,
        job: &mut SavedJob,
        permit: &mut MediaAccountPermit,
    ) -> Result<(Vec<String>, Option<String>)> {
        let fence = job.fence.clone();
        self.read_saved_order_with(
            account,
            id,
            &fence,
            &mut |raw| self.retain_saved_read(account, id, job, raw),
            permit,
        )
    }
    fn reconcile_saved_order(
        &self,
        account: &str,
        id: &str,
        job: &mut SavedJob,
        order: Vec<String>,
        permit: &mut MediaAccountPermit,
    ) -> Result<()> {
        let fence = job.fence.clone();
        self.reconcile_saved_order_with(
            account,
            id,
            &fence,
            &mut |raw| self.retain_saved_read(account, id, job, raw),
            permit,
            order,
        )
    }
    pub(super) fn perform_saved_sticker_job(
        &self,
        account: &str,
        id: &str,
        permit: &mut MediaAccountPermit,
    ) -> Result<()> {
        let row = self.store.media_job(account, id)?;
        let mut job: SavedJob = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        // An accepted request resumes at reconciliation. A submitted request
        // without a durable acceptance must never reach this worker again.
        ensure!(
            !job.submitted || job.accepted,
            "sticker_submission_needs_review"
        );
        self.ensure_sticker_database(account, id)?;
        let (mut order, version) = self.read_saved_order(account, id, &mut job, permit)?;
        self.reconcile_saved_order(account, id, &mut job, order.clone(), permit)?;
        if !job.accepted {
            let action = match job.intent.clone() {
                SavedIntent::Sync => None,
                SavedIntent::Move {
                    id: moving,
                    revision,
                    request,
                } => {
                    ensure!(
                        crate::stickers::order_revision(&order)? == revision,
                        "sticker_revision_mismatch"
                    );
                    ensure!(order.contains(&moving), "custom_sticker_not_found");
                    let action = match request.position.after_id {
                        None => wire::CustomStickerMutation::First,
                        Some(anchor) => {
                            ensure!(order.contains(&anchor), "custom_sticker_anchor_not_found");
                            wire::CustomStickerMutation::After {
                                anchor,
                                version: version.context("sticker_version_unavailable")?,
                                update_id: format!(
                                    "{}_{}",
                                    uuid::Uuid::new_v4(),
                                    chrono::Utc::now().timestamp_millis()
                                ),
                            }
                        }
                    };
                    Some((moving, action))
                }
                SavedIntent::Remove {
                    id: removing,
                    revision,
                } => {
                    ensure!(
                        crate::stickers::order_revision(&order)? == revision,
                        "sticker_revision_mismatch"
                    );
                    order
                        .contains(&removing)
                        .then_some((removing, wire::CustomStickerMutation::Remove))
                }
                SavedIntent::Add { request } => {
                    let (identity, bytes, upload) =
                        self.saved_source(account, &request, Some(id))?;
                    if order.contains(&identity) {
                        None
                    } else {
                        if upload {
                            while (job.upload_offset as usize) < bytes.len() {
                                self.phase_media(account, id, "uploading")?;
                                job.submitted = true;
                                self.save_saved_job(account, id, &job)?;
                                let mut state = self.resume_media_state(account, id, &job.fence)?;
                                let raw = permit.transfer(self, account, || {
                                    self.native_request(account, &mut state, |ctx| {
                                        ctx.upload_sticker_chunk_raw(&bytes, job.upload_offset)
                                    })
                                })?;
                                job.replies.push(raw.clone());
                                self.save_saved_job(account, id, &job)?;
                                let ack = wechat_protocol::sticker_upload::decode_sticker_upload(
                                    &raw,
                                    job.upload_offset,
                                    bytes.len() as u32,
                                    &identity,
                                )?;
                                job.upload_offset = ack.next_offset;
                                job.submitted = false;
                                self.save_saved_job(account, id, &job)?;
                                self.phase_media(account, id, "preparing")?;
                            }
                        }
                        Some((identity, wire::CustomStickerMutation::Add))
                    }
                }
            };
            if let Some((identity, action)) = action {
                self.phase_media(account, id, "submitting")?;
                job.submitted = true;
                job.outcome = Some((identity.clone(), action.clone()));
                self.save_saved_job(account, id, &job)?;
                let mut state = self.resume_media_state(account, id, &job.fence)?;
                let raw = permit.transfer(self, account, || {
                    self.native_request(account, &mut state, |ctx| {
                        ctx.mutate_custom_sticker(&identity, &action)
                    })
                })?;
                job.replies.push(raw.clone());
                self.save_saved_job(account, id, &job)?;
                if pb::base_code(&pb::decode(&raw)?)? != 0 {
                    self.phase_media(account, id, "rejected")?;
                    bail!("custom_sticker_mutation_rejected");
                }
                job.accepted = true;
                self.save_saved_job(account, id, &job)?;
                self.phase_media(account, id, "reconciling")?;
                let (fresh, _) = self.read_saved_order(account, id, &mut job, permit)?;
                order = fresh;
                self.reconcile_saved_order(account, id, &mut job, order.clone(), permit)?;
            }
        }
        if let Some((identity, action)) = &job.outcome {
            let satisfied = placement_satisfied(&order, identity, action);
            ensure!(satisfied, "sticker_readback_mismatch");
        }
        if let SavedIntent::Add { request } = &job.intent {
            let (identity, bytes, _) = self.saved_source(account, request, Some(id))?;
            self.write_saved_cache(account, id, &identity, &bytes)?;
        }
        let mut row = self.store.media_job(account, id)?;
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.data["serverAcceptance"] = if job.accepted {
            "accepted"
        } else {
            "reconciled_noop"
        }
        .into();
        row.data["profileWriteback"] = "complete".into();
        row.data["result"] = json!({"revision":crate::stickers::order_revision(&order)?,"count":order.len(),"serverAcceptance":row.data["serverAcceptance"],"profileWriteback":"complete"});
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    /// Replay only owned local writeback after a known acceptance. This path
    /// cannot upload, resend, refresh a collection or start a media download.
    pub(super) fn repair_saved_sticker_job(&self, account: &str, id: &str) -> Result<bool> {
        self.recover_saved_sticker_projections(account)?;
        let mut row = self.store.media_job(account, id)?;
        ensure!(row.kind == "sticker_saved", "invalid_sticker_consumer");
        let job: SavedJob = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        ensure!(job.accepted, "sticker_submission_needs_review");
        let saved = self.saved_stickers(account)?;
        let order: Vec<_> = saved.items.iter().map(|item| item.id.clone()).collect();
        if let Some((identity, action)) = &job.outcome {
            if !placement_satisfied(&order, identity, action) {
                return Ok(false);
            }
        }
        if let SavedIntent::Add { request } = &job.intent {
            let (identity, bytes, _) = self.saved_source(account, request, Some(id))?;
            self.write_saved_cache(account, id, &identity, &bytes)?;
        }
        row.data["profileWriteback"] = "complete".into();
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        row.data["result"] = json!({"revision":saved.revision,"count":order.len(),"repairedLocally":true,"serverAcceptance":"accepted","profileWriteback":"complete"});
        self.store.save_media_job(account, &row)?;
        Ok(true)
    }
    pub(super) fn ensure_sticker_database(&self, account: &str, intent: &str) -> Result<()> {
        let (location, mut keys) = self.profile_location_keys(account)?;
        let relative = "emoticon/emoticon.db";
        if location.root().join("db_storage").join(relative).exists() {
            if !keys.databases.contains_key(relative) {
                // Recover a database published before its protected key manifest.
                // Verify the accepted password against every existing database;
                // never replace a present file or infer a new password.
                self.write_options(account, intent)?;
                let state = self.account_state(account)?;
                let mut derived = wechat_profile::keys_from_password(
                    location,
                    &state.database_password,
                    wechat_profile::ReadOptions {
                        stopped_or_consistent_snapshot: true,
                        ..Default::default()
                    },
                )?;
                derived.image_dat = keys.image_dat;
                let target = self
                    .store
                    .root
                    .join("accounts")
                    .join(account)
                    .join("agent")
                    .join(format!(
                        "profile-keys-derived-{}.json",
                        uuid::Uuid::new_v4()
                    ));
                derived.write_new(&target)?;
                self.store.set_keys_file(account, &target)?;
            }
            return Ok(());
        }
        let options = self.write_options(account, intent)?;
        let state = self.account_state(account)?;
        let key = wechat_profile::apply_initialize_emoticon(
            &location,
            &keys,
            &state.database_password,
            &options,
        )?;
        keys.databases.insert(relative.into(), key);
        let target = self
            .store
            .root
            .join("accounts")
            .join(account)
            .join("agent")
            .join(format!(
                "profile-keys-derived-{}.json",
                uuid::Uuid::new_v4()
            ));
        keys.write_new(&target)?;
        self.store.set_keys_file(account, &target)?;
        Ok(())
    }
}
