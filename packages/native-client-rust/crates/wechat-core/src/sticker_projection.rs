//! Local replay of owned Saved receipts. No code here initiates network work.
use super::*;
#[derive(Serialize, Deserialize)]
struct SavedProjectionReceipt {
    before_revision: String,
    projection: wechat_profile::SavedStickerProjection,
}
impl Core {
    pub(crate) fn apply_saved_sticker_receipt(
        &self,
        account: &str,
        before_revision: String,
        projection: wechat_profile::SavedStickerProjection,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let body = SavedProjectionReceipt {
            before_revision,
            projection,
        };
        let payload = self.vault.seal(
            account,
            &format!("sticker-projection:{id}"),
            &serde_json::to_vec(&body)?,
        )?;
        self.store
            .stage_sticker_projection(account, &id, &payload)?;
        self.apply_saved_sticker_projection(account, &id, &body)
    }
    fn apply_saved_sticker_projection(
        &self,
        account: &str,
        id: &str,
        body: &SavedProjectionReceipt,
    ) -> Result<()> {
        let current = self.saved_stickers(account)?;
        if self
            .store
            .runtime(account, "sticker-projection-latest")?
            .as_deref()
            == Some(id.as_bytes())
            && current.revision == body.before_revision
        {
            let options = self.write_options(account, id)?;
            let (location, keys) = self.profile_location_keys(account)?;
            wechat_profile::apply_saved_stickers(&location, &keys, &body.projection, &options)?;
        }
        let saved = self.saved_stickers(account)?;
        self.store
            .finish_sticker_projection(account, id, &saved.revision)
    }
    pub(crate) fn recover_saved_sticker_projections(&self, account: &str) -> Result<usize> {
        let mut count = 0;
        for (id, payload) in self.store.pending_sticker_projections(account)? {
            let body: SavedProjectionReceipt = serde_json::from_slice(&self.vault.open_record(
                account,
                &format!("sticker-projection:{id}"),
                &payload,
            )?)?;
            if self
                .apply_saved_sticker_projection(account, &id, &body)
                .is_ok()
            {
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<Core>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let mut state = crate::media::tests::accepted_media_state();
        state.database_password = vec![42; 32];
        state.device.device_id = vec![b'A'; 16];
        state.cookie = vec![1; 15];
        state.auto_auth_key = wechat_protocol::protobuf::message(&[
            wechat_protocol::protobuf::bytes(1, &wechat_protocol::protobuf::buffer(b"synthetic-a")),
            wechat_protocol::protobuf::bytes(2, &wechat_protocol::protobuf::buffer(b"synthetic-b")),
        ])
        .unwrap();
        state.key_info = vec![4; 180];
        core.test_managed_profile("one", &state).unwrap();
        (dir, core)
    }
    fn stage(core: &Core, id: &str, before: &str, order: &[&str]) {
        let body = SavedProjectionReceipt {
            before_revision: before.into(),
            projection: wechat_profile::SavedStickerProjection {
                order: order.iter().map(|s| s.to_string()).collect(),
                removed_metadata: vec![],
                records: vec![],
            },
        };
        let bytes = core
            .vault
            .seal(
                "one",
                &format!("sticker-projection:{id}"),
                &serde_json::to_vec(&body).unwrap(),
            )
            .unwrap();
        core.store
            .stage_sticker_projection("one", id, &bytes)
            .unwrap();
    }
    #[test]
    fn latest_pending_snapshot_wins_and_local_recovery_is_once() {
        let (_dir, core) = fixture();
        let empty = core.saved_stickers("one").unwrap().revision;
        let a = "00000000000000000000000000000000";
        let b = "11111111111111111111111111111111";
        stage(&core, "old", &empty, &[a]);
        stage(&core, "new", &empty, &[b, a]);
        assert_eq!(
            core.store.pending_sticker_projections("one").unwrap().len(),
            2
        );
        assert!(core.store.pending_sync_count("one").unwrap() >= 2);
        assert_eq!(core.recover_saved_sticker_projections("one").unwrap(), 2);
        let saved = core.saved_stickers("one").unwrap();
        assert_eq!(
            saved
                .items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec![b, a]
        );
        assert_eq!(core.recover_saved_sticker_projections("one").unwrap(), 0);
        assert!(core
            .store
            .pending_sticker_projections("one")
            .unwrap()
            .is_empty());
        // A retained older receipt cannot resurrect the collection after a newer write.
        let old = SavedProjectionReceipt {
            before_revision: empty,
            projection: wechat_profile::SavedStickerProjection {
                order: vec![a.into()],
                removed_metadata: vec![],
                records: vec![],
            },
        };
        core.apply_saved_sticker_projection("one", "old", &old)
            .unwrap();
        assert_eq!(core.saved_stickers("one").unwrap().revision, saved.revision);
    }
    #[test]
    fn individual_original_uses_official_cache_without_a_second_payload() {
        let (_dir, core) = fixture();
        let gif = include_bytes!("../../wechat-simulator/fixtures/sticker.gif");
        use md5::{Digest, Md5};
        let id = format!("{:x}", Md5::digest(gif));
        let before = core.saved_stickers("one").unwrap().revision;
        core.apply_saved_sticker_receipt(
            "one",
            before,
            wechat_profile::SavedStickerProjection {
                order: vec![id.clone()],
                removed_metadata: vec![],
                records: vec![wechat_profile::SavedStickerRecord {
                    id: id.clone(),
                    ..Default::default()
                }],
            },
        )
        .unwrap();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let state = core.account_state("one").unwrap();
        wechat_profile::apply_saved_sticker_cache(
            &location,
            state.uin,
            &state.own_username,
            &id,
            gif,
            &core.write_options("one", "cache-fixture").unwrap(),
        )
        .unwrap();
        let item = core.custom_sticker("one", &id).unwrap();
        let asset = item.original.unwrap();
        assert_eq!(asset.state, "available");
        assert!(asset.preview.is_some());
        assert!(core
            .store
            .media_asset_with_payload("one", &asset.id)
            .unwrap()
            .payload
            .is_none());
        assert_eq!(core.media_content("one", &asset.id).unwrap().bytes, gif);
        let preview = core.media_preview_content("one", &asset.id).unwrap();
        assert_eq!(preview.content_type, "image/png");
        assert!(preview.bytes.starts_with(b"\x89PNG"));
    }
}
