//! Local pack writeback from owned complete-list receipts. No request or transfer.
use super::*;
#[derive(Serialize, Deserialize)]
struct PackProjectionReceipt {
    before: String,
    records: Vec<wechat_profile::StickerPackRecord>,
    removed: Vec<String>,
}
impl Core {
    pub(super) fn pack_membership_revision(&self, account: &str) -> Result<String> {
        let mut ids: Vec<_> = self
            .profile_reader_owned(account)?
            .installed_sticker_packs()?
            .into_iter()
            .map(|pack| pack.id)
            .collect();
        ids.sort();
        crate::stickers::order_revision(&ids)
    }
    pub(super) fn project_sticker_packs(
        &self,
        account: &str,
        records: Vec<wechat_profile::StickerPackRecord>,
    ) -> Result<()> {
        let reader = self.profile_reader_owned(account)?;
        let old = reader.installed_sticker_packs()?;
        let removed = old
            .into_iter()
            .filter(|pack| !records.iter().any(|record| record.id == pack.id))
            .map(|pack| pack.id)
            .collect();
        let receipt = PackProjectionReceipt {
            before: self.pack_membership_revision(account)?,
            records,
            removed,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let payload = self.vault.seal(
            account,
            &format!("sticker-pack-projection:{id}"),
            &serde_json::to_vec(&receipt)?,
        )?;
        self.store
            .stage_sticker_pack_projection(account, &id, &payload)?;
        self.apply_sticker_pack_projection(account, &id, &receipt)
    }
    fn apply_sticker_pack_projection(
        &self,
        account: &str,
        id: &str,
        receipt: &PackProjectionReceipt,
    ) -> Result<()> {
        let latest = self
            .store
            .runtime(account, "sticker-pack-projection-latest")?
            .as_deref()
            == Some(id.as_bytes());
        if latest {
            let revision = self.pack_membership_revision(account)?;
            let options = self.write_options(account, id)?;
            let (location, keys) = self.profile_location_keys(account)?;
            if revision == receipt.before {
                wechat_profile::apply_sticker_pack_membership(
                    &location,
                    &keys,
                    &receipt.records,
                    &receipt.removed,
                    &options,
                )?;
            }
            // A repeated cleanup must never erase caches of a pack re-added by
            // a newer canonical observation, even if its old receipt remains.
            let current = self
                .profile_reader_owned(account)?
                .installed_sticker_packs()?;
            for removed in &receipt.removed {
                if !current.iter().any(|pack| &pack.id == removed) {
                    wechat_profile::apply_remove_sticker_pack_cache(&location, removed, &options)?;
                }
            }
        }
        self.store.finish_sticker_pack_projection(
            account,
            id,
            &self.pack_membership_revision(account)?,
        )
    }
    pub(crate) fn recover_sticker_pack_projections(&self, account: &str) -> Result<usize> {
        let mut applied = 0;
        for (id, payload) in self.store.pending_sticker_pack_projections(account)? {
            let receipt: PackProjectionReceipt = serde_json::from_slice(&self.vault.open_record(
                account,
                &format!("sticker-pack-projection:{id}"),
                &payload,
            )?)?;
            if self
                .apply_sticker_pack_projection(account, &id, &receipt)
                .is_ok()
            {
                applied += 1;
            }
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stage(core: &Core, id: &str, body: &PackProjectionReceipt) {
        let payload = core
            .vault
            .seal(
                "one",
                &format!("sticker-pack-projection:{id}"),
                &serde_json::to_vec(body).unwrap(),
            )
            .unwrap();
        core.store
            .stage_sticker_pack_projection("one", id, &payload)
            .unwrap();
    }
    #[test]
    fn pack_and_saved_recovery_markers_are_independent_and_newer_pack_snapshot_wins() {
        let (_dir, core) = crate::media::storage_tests::profile_core();
        core.store
            .put_runtime("one", "sticker-projection-latest", b"saved-receipt")
            .unwrap();
        let before = core.pack_membership_revision("one").unwrap();
        let old = PackProjectionReceipt {
            before: before.clone(),
            records: vec![wechat_profile::StickerPackRecord {
                id: "old".into(),
                ..Default::default()
            }],
            removed: vec![],
        };
        let new = PackProjectionReceipt {
            before,
            records: vec![wechat_profile::StickerPackRecord {
                id: "new".into(),
                ..Default::default()
            }],
            removed: vec![],
        };
        stage(&core, "old", &old);
        stage(&core, "new", &new);
        assert_eq!(core.recover_sticker_pack_projections("one").unwrap(), 2);
        assert_eq!(core.added_sticker_packs("one").unwrap().items[0].id, "new");
        assert_eq!(
            core.store
                .runtime("one", "sticker-projection-latest")
                .unwrap()
                .unwrap(),
            b"saved-receipt"
        );
        assert_eq!(core.recover_sticker_pack_projections("one").unwrap(), 0);
        core.apply_sticker_pack_projection("one", "old", &old)
            .unwrap();
        assert_eq!(core.added_sticker_packs("one").unwrap().items.len(), 1);
        assert_eq!(core.added_sticker_packs("one").unwrap().items[0].id, "new");
    }
    #[test]
    fn stale_removal_receipt_never_deletes_a_readded_packs_cache() {
        let (_dir, core) = crate::media::storage_tests::profile_core();
        let record = wechat_profile::StickerPackRecord {
            id: "chosen".into(),
            ..Default::default()
        };
        core.project_sticker_packs("one", vec![record.clone()])
            .unwrap();
        let removed = PackProjectionReceipt {
            before: core.pack_membership_revision("one").unwrap(),
            records: vec![],
            removed: vec!["chosen".into()],
        };
        stage(&core, "removal", &removed);
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let options = core.write_options("one", "removal").unwrap();
        wechat_profile::apply_sticker_pack_membership(
            &location,
            &keys,
            &[],
            &["chosen".into()],
            &options,
        )
        .unwrap();
        // Interrupted cleanup leaves owned bytes; a later fresh add supersedes it.
        let identity = core.profile_identity("one").unwrap();
        wechat_profile::apply_sticker_pack_cache(
            &location,
            identity.uin,
            &identity.username,
            "chosen",
            wechat_profile::StickerPackCacheKind::Original,
            b"owned aggregate",
            &options,
        )
        .unwrap();
        core.project_sticker_packs("one", vec![record]).unwrap();
        core.apply_sticker_pack_projection("one", "removal", &removed)
            .unwrap();
        assert_eq!(
            wechat_profile::read_sticker_pack_cache(
                &location,
                identity.uin,
                &identity.username,
                "chosen",
                wechat_profile::StickerPackCacheKind::Original
            )
            .unwrap(),
            b"owned aggregate"
        );
    }
}
