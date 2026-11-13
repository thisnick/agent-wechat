//! Idempotent pack membership and reconciliation, independent of media acquisition.
use super::*;
use wechat_protocol::{protobuf as pb, sticker_packs as wire};
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PackIntent {
    Sync {
        saved: bool,
        #[serde(default)]
        controls: Vec<crate::sticker_control::StickerRefresh>,
    },
    Membership {
        id: String,
        added: bool,
    },
}
#[derive(Serialize, Deserialize)]
struct PackJob {
    intent: PackIntent,
    fence: MediaAuthorizationFence,
    reads: Vec<Vec<u8>>,
    reply: Option<Vec<u8>>,
    submitted: bool,
    accepted: bool,
    #[serde(default)]
    grant_submitted: bool,
    #[serde(default)]
    grant_reply: Option<Vec<u8>>,
    #[serde(default)]
    grant_accepted: bool,
}
impl Core {
    /// Request Saved, added packs, or both through one asynchronous operation.
    pub fn sync_stickers(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        scope: crate::StickerSyncScope,
    ) -> Result<MediaJob> {
        match scope {
            crate::StickerSyncScope::Saved => self.sync_custom_stickers(account, key),
            crate::StickerSyncScope::Added => self.queue_pack_job(
                account,
                key,
                PackIntent::Sync {
                    saved: false,
                    controls: vec![],
                },
            ),
            crate::StickerSyncScope::All => self.queue_pack_job(
                account,
                key,
                PackIntent::Sync {
                    saved: true,
                    controls: vec![],
                },
            ),
        }
    }
    pub(crate) fn queue_sticker_control_job(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        controls: Vec<crate::sticker_control::StickerRefresh>,
    ) -> Result<MediaJob> {
        self.queue_pack_job(
            account,
            key,
            PackIntent::Sync {
                saved: true,
                controls,
            },
        )
    }
    /// Establish or remove eligible-free membership with a stable submission key.
    /// This does not initiate the separate pack media acquisition operation.
    pub fn set_sticker_pack_membership(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        id: &str,
        added: bool,
    ) -> Result<MediaJob> {
        ensure!(
            !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control),
            "invalid_sticker_pack_id"
        );
        self.queue_pack_job(
            account,
            key,
            PackIntent::Membership {
                id: id.into(),
                added,
            },
        )
    }
    fn queue_pack_job(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        intent: PackIntent,
    ) -> Result<MediaJob> {
        ensure!(
            !key.is_empty()
                && key.len() <= 200
                && key.bytes().all(|b| b.is_ascii_graphic() || b == b' '),
            "invalid_idempotency_key"
        );
        let key = format!("pack:{key}");
        let hash = digest(&serde_json::to_vec(&intent)?);
        if let Some(row) = self.store.find_media_job(account, &key, &hash)? {
            return Ok(public_job(account, &row));
        }
        let fence = self.media_authorization_fence(account)?;
        self.write_options(account, "pack-admission")?;
        let id = format!("job_{}", uuid::Uuid::new_v4());
        let plan = PackJob {
            intent,
            fence,
            reads: vec![],
            reply: None,
            submitted: false,
            accepted: false,
            grant_submitted: false,
            grant_reply: None,
            grant_accepted: false,
        };
        let payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(&plan)?,
        )?;
        let (row,created)=self.store.create_media_job(account,&id,&key,&hash,"sticker_pack",&json!({"serverAcceptance":"pending","profileWriteback":"pending","result":{"serverAcceptance":"pending","profileWriteback":"pending"}}),&payload)?;
        if created {
            self.spawn_media_worker(account, &row.id);
        }
        Ok(public_job(account, &row))
    }
    fn save_pack_job(&self, account: &str, id: &str, plan: &PackJob) -> Result<()> {
        let mut row = self.store.media_job(account, id)?;
        let status = if plan.accepted {
            "accepted"
        } else if plan.submitted {
            "uncertain"
        } else {
            "pending"
        };
        row.data["serverAcceptance"] = status.into();
        row.data["result"]["serverAcceptance"] = status.into();
        row.data["result"]["acquisitionAcceptance"] = if plan.grant_accepted {
            "accepted"
        } else if plan.grant_submitted {
            "uncertain"
        } else {
            "not_submitted"
        }
        .into();
        row.payload = self.vault.seal(
            account,
            &format!("media-job:{id}"),
            &serde_json::to_vec(plan)?,
        )?;
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
    fn retain_pack_read(
        &self,
        account: &str,
        id: &str,
        plan: &mut PackJob,
        raw: &[u8],
    ) -> Result<()> {
        ensure!(
            plan.reads.len() < 250
                && plan.reads.iter().map(Vec::len).sum::<usize>() + raw.len() <= 8 * 1024 * 1024,
            "sticker_receipt_limit"
        );
        plan.reads.push(raw.to_vec());
        self.save_pack_job(account, id, plan)
    }
    fn read_added_pack_list(
        &self,
        account: &str,
        id: &str,
        plan: &mut PackJob,
    ) -> Result<Vec<wire::StickerPackSummary>> {
        let mut cursor = vec![];
        let mut items = vec![];
        for _ in 0..100 {
            let mut state = self.resume_media_state(account, id, &plan.fence)?;
            let raw = self.native_request(account, &mut state, |ctx| {
                ctx.sticker_pack_page_raw(wire::StickerPackFeed::Added, &cursor)
            })?;
            self.retain_pack_read(account, id, plan, &raw)?;
            self.resume_media_state(account, id, &plan.fence)?;
            let page = wire::decode_sticker_pack_page(&raw)?;
            ensure!(!page.restart, "sticker_pack_list_restarted");
            for summary in page.items {
                ensure!(
                    items.len() < 1000
                        && !items
                            .iter()
                            .any(|item: &wire::StickerPackSummary| item.id == summary.id),
                    "sticker_pack_list_invalid"
                );
                items.push(summary);
            }
            if page.complete {
                return Ok(items);
            }
            ensure!(
                !page.next_cursor.is_empty() && page.next_cursor != cursor,
                "sticker_cursor_not_advancing"
            );
            cursor = page.next_cursor;
        }
        bail!("sticker_page_limit")
    }
    fn reconcile_added_pack_list(
        &self,
        account: &str,
        summaries: &[wire::StickerPackSummary],
    ) -> Result<()> {
        let mut rank = summaries.len() as i64;
        let mut records = vec![];
        for summary in summaries {
            if summary.id == "com.tencent.xin.emoticon.tusiji" {
                continue;
            }
            let fields = pb::decode(&summary.raw)?;
            let string = |number| -> Result<String> {
                Ok(
                    std::str::from_utf8(pb::optional_data(&fields, number)?.unwrap_or_default())?
                        .into(),
                )
            };
            records.push(wechat_profile::StickerPackRecord {
                id: summary.id.clone(),
                name: summary.name.clone(),
                sort_order: rank,
                introduction: string(18)?,
                full_description: string(4)?,
                author: string(5)?,
                copyright: string(11)?,
                store_icon_url: string(2)?,
                panel_url: string(13)?,
                ..Default::default()
            });
            rank -= 1;
        }
        self.project_sticker_packs(account, records)
    }
    pub(super) fn perform_sticker_pack_job(&self, account: &str, id: &str) -> Result<()> {
        let row = self.store.media_job(account, id)?;
        let mut plan: PackJob = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        ensure!(
            !plan.submitted || plan.accepted,
            "sticker_submission_needs_review"
        );
        ensure!(
            !plan.grant_submitted || plan.grant_accepted,
            "sticker_submission_needs_review"
        );
        self.ensure_sticker_database(account, id)?;
        if matches!(plan.intent, PackIntent::Sync { saved: true, .. }) {
            let fence = plan.fence.clone();
            let (order, _) = self.read_saved_order_with(account, id, &fence, &mut |raw| {
                self.retain_pack_read(account, id, &mut plan, raw)
            })?;
            self.reconcile_saved_order_with(
                account,
                id,
                &fence,
                &mut |raw| self.retain_pack_read(account, id, &mut plan, raw),
                order,
            )?;
        }
        let mut summaries = self.read_added_pack_list(account, id, &mut plan)?;
        self.reconcile_added_pack_list(account, &summaries)?;
        if let PackIntent::Membership { id: pack, added } = plan.intent.clone() {
            let present = summaries.iter().any(|summary| summary.id == pack);
            if !plan.accepted && present != added {
                let mut state = self.resume_media_state(account, id, &plan.fence)?;
                let raw = self.native_request(account, &mut state, |ctx| {
                    ctx.sticker_pack_detail_raw(&pack)
                })?;
                self.retain_pack_read(account, id, &mut plan, &raw)?;
                let detail = wire::decode_sticker_pack_detail(&raw, &pack)?;
                ensure!(
                    super::sticker_catalog::eligible_pack(&detail.summary),
                    "sticker_pack_not_eligible"
                );
                if added && !plan.grant_accepted {
                    // Eligible-free activation is separate from membership. The
                    // returned locator is retained; no archive transfer starts.
                    self.phase_media(account, id, "submitting")?;
                    plan.grant_submitted = true;
                    self.save_pack_job(account, id, &plan)?;
                    let mut state = self.resume_media_state(account, id, &plan.fence)?;
                    let raw = self.native_request(account, &mut state, |ctx| {
                        ctx.sticker_pack_acquisition_raw(&pack)
                    })?;
                    plan.grant_reply = Some(raw.clone());
                    self.save_pack_job(account, id, &plan)?;
                    let code = pb::base_code(&pb::decode(&raw)?)?;
                    if code != 0 {
                        self.reject_pack_job(account, id, code, "acquisition")?;
                        bail!("sticker_pack_acquisition_rejected");
                    }
                    wire::decode_sticker_pack_acquisition(&raw)?;
                    plan.grant_accepted = true;
                    self.save_pack_job(account, id, &plan)?;
                }
                self.phase_media(account, id, "submitting")?;
                plan.submitted = true;
                self.save_pack_job(account, id, &plan)?;
                let mut state = self.resume_media_state(account, id, &plan.fence)?;
                let raw = self.native_request(account, &mut state, |ctx| {
                    ctx.sticker_pack_membership_raw(&pack, added)
                })?;
                plan.reply = Some(raw.clone());
                self.save_pack_job(account, id, &plan)?;
                let code = pb::base_code(&pb::decode(&raw)?)?;
                if code != 0 {
                    self.reject_pack_job(account, id, code, "membership")?;
                    bail!("sticker_pack_membership_rejected");
                }
                plan.accepted = true;
                self.save_pack_job(account, id, &plan)?;
                self.phase_media(account, id, "reconciling")?;
                summaries = self.read_added_pack_list(account, id, &mut plan)?;
                self.reconcile_added_pack_list(account, &summaries)?;
            }
            ensure!(
                summaries.iter().any(|summary| summary.id == pack) == added,
                "sticker_pack_readback_mismatch"
            );
            if added {
                self.promote_owned_sticker_pack(account, &pack)?;
            }
        }
        let mut row = self.store.media_job(account, id)?;
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        let status = if plan.accepted {
            "accepted"
        } else {
            "reconciled_noop"
        };
        row.data["serverAcceptance"] = status.into();
        row.data["profileWriteback"] = "complete".into();
        row.data["result"] = json!({"serverAcceptance":status,"profileWriteback":"complete","packCount":self.added_sticker_packs(account)?.items.len()});
        self.store.save_media_job(account, &row)?;
        if let PackIntent::Sync { controls, .. } = &plan.intent {
            self.finish_sticker_controls(account, controls)?;
        }
        Ok(())
    }
    pub(super) fn repair_sticker_pack_job(&self, account: &str, id: &str) -> Result<bool> {
        self.recover_sticker_pack_projections(account)?;
        let mut row = self.store.media_job(account, id)?;
        let plan: PackJob = serde_json::from_slice(&self.vault.open_record(
            account,
            &format!("media-job:{id}"),
            &row.payload,
        )?)?;
        ensure!(plan.accepted, "sticker_submission_needs_review");
        let PackIntent::Membership { id: pack, added } = plan.intent else {
            return Ok(false);
        };
        if self
            .added_sticker_packs(account)?
            .items
            .iter()
            .any(|item| item.id == pack)
            != added
        {
            return Ok(false);
        }
        row.data["profileWriteback"] = "complete".into();
        row.data["result"] = json!({"serverAcceptance":"accepted","profileWriteback":"complete","repairedLocally":true});
        row.state = "available".into();
        row.phase = "completed".into();
        row.retryable = false;
        row.error_code = None;
        self.store.save_media_job(account, &row)?;
        Ok(true)
    }
    fn reject_pack_job(&self, account: &str, id: &str, code: i32, stage: &str) -> Result<()> {
        let mut row = self.phase_media(account, id, "rejected")?;
        row.data["serverAcceptance"] = "rejected".into();
        row.data["result"]["serverAcceptance"] = "rejected".into();
        row.data["result"]["serviceCode"] = code.into();
        row.data["result"]["serviceStage"] = stage.into();
        self.store.save_media_job(account, &row)?;
        Ok(())
    }
}
