//! Sticker sends retain one client identity, every acknowledgement and owned media.
use super::*;
use crate::{StickerSendRequest, StickerSendSource};
use wechat_protocol::sticker_send as wire;
#[derive(Serialize, Deserialize)]
struct StickerSendPlan {
    request: StickerSendRequest,
    fence: MediaAuthorizationFence,
    client_id: String,
    created_at: u32,
    #[serde(default)]
    cache_month: String,
    content_id: String,
    source_id: Option<String>,
    original_url: Option<String>,
    xml: String,
    offset: u32,
    probe: bool,
    submitted: bool,
    replies: Vec<Vec<u8>>,
    server_id: Option<String>,
}
impl Core {
    /// Send an existing or prepared GIF sticker without client-side re-upload.
    /// Missing canonical GIF bytes are acquired as part of this explicit send.
    /// Existing alternate-format Saved caches remain unchanged.
    pub fn send_sticker(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        request: StickerSendRequest,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty() && key.len() <= 200 && !key.chars().any(char::is_control),
            "invalid_idempotency_key"
        );
        validate_destination(&request.thread_id)?;
        let hash = digest(&serde_json::to_vec(&request)?);
        ensure!(
            self.store.send_by_key(account, key).is_err(),
            "idempotency_conflict"
        );
        let key = format!("send:{key}");
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        self.validate_sticker_send_membership(account, &request.source)?;
        let (bytes, original_url, product) = match &request.source {
            StickerSendSource::Saved { sticker_id } => {
                let record = self
                    .profile_reader_owned(account)?
                    .saved_sticker_record(sticker_id)?;
                let cached = self
                    .saved_cache_bytes(account, sticker_id)
                    .ok()
                    .filter(|bytes| {
                        wechat_media::inspect_image(bytes)
                            .is_ok_and(|image| image.content_type == "image/gif")
                    });
                if cached.is_none() {
                    wechat_protocol::media::validate_sticker_url(&record.cdn_url)?;
                }
                (cached, Some(record.cdn_url), None)
            }
            StickerSendSource::Pack {
                pack_id,
                sticker_id,
            } => {
                let asset = self
                    .indexed_pack_asset(account, pack_id, sticker_id, "original")
                    .context("sticker_source_unavailable")?;
                (
                    Some(self.media_content(account, &asset.id)?.bytes),
                    None,
                    Some(pack_id.as_str()),
                )
            }
            StickerSendSource::Prepared { preparation_id } => {
                let prepared: crate::PreparedSticker = serde_json::from_value(
                    self.sticker_preparation(account, preparation_id)?.result,
                )?;
                (
                    Some(self.media_content(account, &prepared.original.id)?.bytes),
                    None,
                    None,
                )
            }
        };
        let expected_id = match &request.source {
            StickerSendSource::Saved { sticker_id }
            | StickerSendSource::Pack { sticker_id, .. } => Some(sticker_id.as_str()),
            StickerSendSource::Prepared { .. } => None,
        };
        let (content_id, source_id, xml) = if let Some(bytes) = bytes {
            let content_id = validate_send_bytes(&bytes, expected_id)?;
            let xml = sticker_xml(&content_id, &bytes, product)?;
            let source =
                self.upload_source(account, &format!("{content_id}.gif"), "image/gif", &bytes)?;
            (content_id, Some(source.id), xml)
        } else {
            (
                expected_id
                    .context("sticker_source_unavailable")?
                    .to_owned(),
                None,
                String::new(),
            )
        };
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let plan = StickerSendPlan {
            request,
            fence: self.media_authorization_fence(account)?,
            client_id: format!("aw_{}", uuid::Uuid::new_v4().simple()),
            created_at: wechat_protocol::wire::unix_time()?,
            cache_month: String::new(),
            content_id,
            source_id,
            original_url,
            xml,
            offset: 0,
            probe: true,
            submitted: false,
            replies: vec![],
            server_id: None,
        };
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&plan)?,
        )?;
        let (row,created)=self.store.create_media_job(account,&id,&key,&hash,"sticker_send",&json!({"threadId":plan.request.thread_id,"sourceId":plan.source_id,"serverAcceptance":"pending","profileWriteback":"pending","result":{"serverAcceptance":"pending","profileWriteback":"pending"}}),&payload)?;
        if created {
            self.spawn_media_worker(account, &id);
        }
        Ok(public_job(account, &row))
    }
    fn validate_sticker_send_membership(
        &self,
        account: &str,
        source: &StickerSendSource,
    ) -> Result<()> {
        match source {
            StickerSendSource::Saved { sticker_id } => ensure!(
                self.saved_stickers(account)?
                    .items
                    .iter()
                    .any(|item| &item.id == sticker_id),
                "custom_sticker_not_found"
            ),
            StickerSendSource::Pack { pack_id, .. } => ensure!(
                self.profile_reader_owned(account)?
                    .installed_sticker_packs()?
                    .iter()
                    .any(|pack| &pack.id == pack_id),
                "sticker_pack_not_added"
            ),
            StickerSendSource::Prepared { .. } => {}
        }
        Ok(())
    }
    fn sticker_send_plan(&self, account: &str, id: &str) -> Result<StickerSendPlan> {
        let row = self.store.media_job(account, id)?;
        Ok(serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?)
    }
    fn save_sticker_send_plan(
        &self,
        account: &str,
        id: &str,
        plan: &StickerSendPlan,
    ) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        row.payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(plan)?,
        )?;
        row.data["serverAcceptance"] = if plan.server_id.is_some() {
            "accepted"
        } else if plan.submitted {
            "uncertain"
        } else {
            "pending"
        }
        .into();
        row.data["result"]["serverAcceptance"] = row.data["serverAcceptance"].clone();
        row.data["completedBytes"] = plan.offset.into();
        row.data["sourceId"] = json!(plan.source_id);
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    pub(super) fn perform_sticker_send(
        &self,
        account: &str,
        id: &str,
        permit: &mut MediaAccountPermit,
    ) -> Result<()> {
        let mut plan = self.sticker_send_plan(account, id)?;
        if plan.server_id.is_some() {
            return self.finish_sticker_send(account, id, &plan);
        }
        ensure!(!plan.submitted, "sticker_submission_needs_review");
        self.validate_sticker_send_membership(account, &plan.request.source)?;
        if plan.source_id.is_none() {
            self.phase_media(account, id, "downloading")?;
            let url = plan
                .original_url
                .as_deref()
                .context("sticker_source_unavailable")?;
            crate::upstream::require_native(self.upstream.kind())?;
            let (bytes, _) = permit.transfer(self, account, || {
                wechat_protocol::media::fetch_sticker_original(url, &plan.content_id, |_, _| Ok(()))
            })?;
            self.resume_media_state(account, id, &plan.fence)?;
            self.validate_sticker_send_membership(account, &plan.request.source)?;
            validate_send_bytes(&bytes, Some(&plan.content_id))?;
            plan.xml = sticker_xml(&plan.content_id, &bytes, None)?;
            plan.source_id = Some(
                self.upload_source(
                    account,
                    &format!("{}.gif", plan.content_id),
                    "image/gif",
                    &bytes,
                )?
                .id,
            );
            self.save_sticker_send_plan(account, id, &plan)?;
        }
        let bytes = self.sticker_send_bytes(account, &plan)?;
        if plan.cache_month.is_empty() {
            plan.cache_month = chrono::Local::now().format("%Y-%m").to_string();
            self.save_sticker_send_plan(account, id, &plan)?;
        }
        self.write_sticker_message_cache(account, id, &plan)?;
        while plan.server_id.is_none() {
            self.phase_media(account, id, "submitting")?;
            plan.submitted = true;
            self.save_sticker_send_plan(account, id, &plan)?;
            let chunk = wire::StickerSendChunk {
                id: &plan.content_id,
                recipient: &plan.request.thread_id,
                client_id: &plan.client_id,
                source: &bytes,
                offset: plan.offset,
                probe: plan.probe,
            };
            let mut state = self.resume_media_state(account, id, &plan.fence)?;
            let raw = self.native_request(account, &mut state, |ctx| {
                ctx.send_sticker_chunk_raw(&chunk)
            })?;
            ensure!(
                raw.len() <= 65536 && plan.replies.len() < 2050,
                "sticker_receipt_limit"
            );
            plan.replies.push(raw.clone());
            self.save_sticker_send_plan(account, id, &plan)?;
            let chunk = wire::StickerSendChunk {
                id: &plan.content_id,
                recipient: &plan.request.thread_id,
                client_id: &plan.client_id,
                source: &bytes,
                offset: plan.offset,
                probe: plan.probe,
            };
            let ack = wire::decode_sticker_send_acknowledgement(&raw, &chunk)?;
            self.resume_media_state(account, id, &plan.fence)?;
            plan.offset = ack.next_offset;
            plan.probe = false;
            if plan.offset as usize == bytes.len() {
                plan.server_id = ack.server_id;
            }
            plan.submitted = false;
            self.save_sticker_send_plan(account, id, &plan)?;
        }
        self.phase_media(account, id, "projecting")?;
        self.finish_sticker_send(account, id, &plan)
    }
    fn finish_sticker_send(&self, account: &str, id: &str, plan: &StickerSendPlan) -> Result<()> {
        self.write_sticker_message_cache(account, id, plan)?;
        let bytes = self.sticker_send_bytes(account, plan)?;
        let server = plan
            .server_id
            .as_deref()
            .context("sticker_submission_needs_review")?;
        let source = self.project_accepted_media(
            account,
            &plan.request.thread_id,
            47,
            &plan.xml,
            "",
            u64::from(plan.created_at),
            server,
            &format!("sticker-send:{id}"),
        )?;
        let message_id = crate::message_id(&source)?;
        let message = self
            .profile_reader_owned(account)?
            .message(&source, Some(&self.profile_identity(account)?.username))?;
        let asset_id = format!("asset_sticker_{}", &digest(message_id.as_bytes())[..40]);
        let image = wechat_media::inspect_image(&bytes)?;
        let asset = MediaAsset {
            id: asset_id.clone(),
            account_id: account.into(),
            message_id: Some(message_id.clone()),
            kind: MediaKind::Sticker,
            variant: MediaVariant::Original,
            actual_variant: Some(MediaVariant::Original),
            state: "available".into(),
            content_type: Some("image/gif".into()),
            byte_length: Some(bytes.len() as u64),
            advertised_byte_length: None,
            auto_download_allowed: Some(false),
            sha256: Some(digest(&bytes)),
            filename: Some(format!("{}.gif", plan.content_id)),
            expires_at: None,
            reason: None,
            retryable: false,
            job_id: Some(id.into()),
            content_url: Some(format!("/v1/accounts/{account}/media/{asset_id}/content")),
            animation_frames: Some(image.frames),
            official_writeback: "complete".into(),
            preview: None,
        };
        let owned = self
            .vault
            .seal_media(account, &format!("asset:{asset_id}"), &bytes)?;
        let binding = self.vault.seal(
            account,
            &format!("plan:{asset_id}"),
            &serde_json::to_vec(&identity::AuthorizedPlan {
                identity: Some(identity::capture(&message)?),
                plan: DownloadPlan::CachedOnly,
            })?,
        )?;
        self.store.put_media_asset(
            account,
            &MediaAssetRecord {
                id: asset_id,
                message_id: Some(message_id.clone()),
                kind: "sticker".into(),
                variant: "original".into(),
                metadata: serde_json::to_value(&asset)?,
                plan: binding,
                payload: Some(owned),
            },
        )?;
        let month = self.sticker_send_month(plan)?;
        self.store.save_media_asset_in_profile(
            account,
            &asset.id,
            &serde_json::to_value(&asset)?,
            &format!("sticker-message:{month}:{}", plan.content_id),
        )?;
        let mut row = self.store.media_job(account, id)?;
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        row.data["profileWriteback"] = "complete".into();
        row.data["result"] = json!({"serverAcceptance":"accepted","serverMessageId":server,"messageId":message_id,"profileWriteback":"complete","officialMediaWriteback":"complete","sticker":asset});
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    pub(super) fn repair_sticker_send(&self, account: &str, id: &str) -> Result<bool> {
        let plan = self.sticker_send_plan(account, id)?;
        self.finish_sticker_send(account, id, &plan)?;
        Ok(true)
    }
    fn sticker_send_bytes(&self, account: &str, plan: &StickerSendPlan) -> Result<Vec<u8>> {
        let (_, bytes) = self.source_bytes(
            account,
            plan.source_id
                .as_deref()
                .context("sticker_source_unavailable")?,
        )?;
        validate_send_bytes(&bytes, Some(&plan.content_id))?;
        Ok(bytes)
    }
    fn sticker_send_month(&self, plan: &StickerSendPlan) -> Result<String> {
        if !plan.cache_month.is_empty() {
            return Ok(plan.cache_month.clone());
        }
        use chrono::TimeZone;
        Ok(chrono::Local
            .timestamp_opt(i64::from(plan.created_at), 0)
            .single()
            .context("invalid_sticker_cache_month")?
            .format("%Y-%m")
            .to_string())
    }
    fn write_sticker_message_cache(
        &self,
        account: &str,
        id: &str,
        plan: &StickerSendPlan,
    ) -> Result<()> {
        let identity = self.profile_identity(account)?;
        let (location, _) = self.profile_location_keys(account)?;
        let bytes = self.sticker_send_bytes(account, plan)?;
        wechat_profile::apply_message_sticker_cache(
            &location,
            identity.uin,
            &identity.username,
            &self.sticker_send_month(plan)?,
            &plan.content_id,
            &bytes,
            &self.write_options(account, id)?,
        )?;
        Ok(())
    }
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn validate_send_bytes(bytes: &[u8], expected: Option<&str>) -> Result<String> {
    let inspected = wechat_media::inspect_image(bytes)?;
    ensure!(
        inspected.content_type == "image/gif" && bytes.len() <= 16 * 1024 * 1024,
        "unsupported_sticker_send_format"
    );
    use md5::Md5;
    let id = format!("{:x}", Md5::digest(bytes));
    ensure!(
        expected.is_none_or(|expected| expected == id),
        "media_integrity_failed"
    );
    Ok(id)
}
fn sticker_xml(id: &str, bytes: &[u8], product: Option<&str>) -> Result<String> {
    let image = wechat_media::inspect_image(bytes)?;
    let product = product
        .map(|id| format!(" productid=\"{}\"", escape(id)))
        .unwrap_or_default();
    Ok(format!("<?xml version=\"1.0\"?><msg><emoji md5=\"{id}\" type=\"2\" len=\"{}\" width=\"{}\" height=\"{}\"{product}/><gameext type=\"0\" content=\"0\"/></msg>", bytes.len(), image.width, image.height))
}
