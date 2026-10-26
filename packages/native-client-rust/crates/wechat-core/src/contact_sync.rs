//! Independent contact drain. Received cursors commit with sealed command bytes;
//! official cursor publication waits for every contact command to be applied.
use super::*;
use wechat_protocol::contact_sync::{self, ContactSyncBatch};

#[derive(Serialize, Deserialize)]
struct Publication {
    username: String,
    data: Vec<u8>,
    metadata: Vec<u8>,
    cursor: Vec<u8>,
}

impl Drop for Publication {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.data.zeroize();
        self.metadata.zeroize();
        self.cursor.zeroize();
    }
}
impl Core {
    fn contact_store(
        &self,
        account: &str,
    ) -> Result<(
        wechat_profile::ProfileLocation,
        wechat_profile::ProfileIdentity,
        std::path::PathBuf,
    )> {
        let binding = self
            .identity_binding(account)?
            .context("contact_identity_unavailable")?;
        let identity = self.read_bound_identity(account, &binding)?;
        let (location, _) = self.profile_location_keys(account)?;
        self.ensure_contact_sync_store(account, &identity, &location)?;
        let path = wechat_profile::account_contact_sync_store_path(&location, &identity)?;
        Ok((location, identity, path))
    }
    fn profile_contact_cursor(&self, account: &str) -> Result<Vec<u8>> {
        let (location, identity, path) = self.contact_store(account)?;
        let data = checkpoint::read_cursor_file(&path, location.root(), 4 * 1024 * 1024)?;
        let metadata =
            checkpoint::read_cursor_file(&path.with_extension("crc"), location.root(), 4096)?;
        let key = wechat_profile::account_mmkv_key(&identity.username)?;
        Ok(wechat_profile::decode_mmkv_records(&data, &metadata, &key)?
            .string_value("sync_11")?
            .unwrap_or_default()
            .to_vec())
    }
    fn contact_cursor(&self, account: &str) -> Result<Vec<u8>> {
        let cursor = match self.store.runtime(account, "contact-received-cursor")? {
            Some(sealed) => self
                .vault
                .open_record(account, "contact-received-cursor", &sealed)?
                .to_vec(),
            None => self.profile_contact_cursor(account)?,
        };
        contact_sync::validate_cursor(&cursor)?;
        Ok(cursor)
    }
    pub(super) async fn drain_contact_sync(
        self: &Arc<Self>,
        account: &str,
        fence: &SessionFence,
    ) -> Result<()> {
        if self.upstream.kind() == crate::upstream::UpstreamKind::Simulator {
            return Ok(());
        }
        for page in 0..32 {
            let guard = self.lock_account(account).await?;
            let core = self.clone();
            let owned = account.to_owned();
            let active = fence.clone();
            let more = blocking(move || {
                let _guard = guard;
                let mut state = core.push_state(&owned, &active)?;
                core.project_contact_pending(&owned)?;
                core.publish_contact_checkpoint(&owned, &active)?;
                let expected = core.store.runtime(&owned, "account-state")?.context("auth_required")?;
                let cursor = core.contact_cursor(&owned)?;
                let batch = core.upstream.contact_sync(&mut state, &cursor, &core.cursor(&owned)?, page != 0)?;
                core.save_push_state(&owned, &state, &expected, &active)?;
                let more = batch.continuation.iter().any(|value| *value != 0);
                let stalled = more && batch.cursor == cursor;
                core.commit_contact_batch(&owned, batch)?;
                core.publish_contact_checkpoint(&owned, &active)?;
                if stalled { bail!("contact_sync_cursor_did_not_progress"); }
                if !more {
                    core.store.put_runtime(&owned, "contact-sync-ready", core.ownership_generation.as_bytes())?;
                    core.store.append_event(&owned, "contacts.sync", &json!({"state":"caught_up","pendingProjectionBatches":core.store.pending_contact_sync_count(&owned)?}))?;
                }
                Ok(more)
            }).await?;
            if !more {
                return Ok(());
            }
        }
        bail!("contact_sync_page_limit")
    }
    fn commit_contact_batch(&self, account: &str, batch: ContactSyncBatch) -> Result<()> {
        let raw = zeroize::Zeroizing::new(serde_json::to_vec(&batch)?);
        let id = format!("contact:{:x}", Sha256::digest(raw.as_slice()));
        self.store.stage_contact_sync(
            account,
            &id,
            &self.vault.seal(account, &format!("apply:{id}"), &raw)?,
            &self
                .vault
                .seal(account, "contact-received-cursor", &batch.cursor)?,
        )?;
        self.project_contact_pending(account)?;
        self.store.append_event_once(
            account,
            &format!("received:{id}"),
            "contacts.sync",
            &json!({"batchId":id,"commandCount":batch.commands.len(),"state":"received"}),
        )?;
        Ok(())
    }
    pub(super) fn project_contact_pending(&self, account: &str) -> Result<()> {
        for (id, sealed) in self.store.pending_contact_sync(account)? {
            let raw = self
                .vault
                .open_record(account, &format!("apply:{id}"), &sealed)?;
            let batch: ContactSyncBatch = serde_json::from_slice(&raw)?;
            let mut blockers = Vec::new();
            for (index, command) in batch.commands.iter().enumerate() {
                let receipt = format!("projected:{id}:{index}");
                if self.store.runtime(account, &receipt)?.is_some() {
                    continue;
                }
                // Contact stream commands are metadata, never message content.
                let result = if command.id == 5 {
                    Err(anyhow::anyhow!("unexpected_contact_message_command"))
                } else {
                    self.apply_sync_command(account, command, &receipt, "contact_sync")
                };
                if let Err(error) = result {
                    if blockers.len() < 50 {
                        blockers.push(sync_projection::projection_blocker(&error).unwrap_or_else(||json!({"commandId":command.id,"reason":"unsupported_or_unavailable_profile"})));
                    }
                }
            }
            if blockers.is_empty() {
                self.store.finish_sync(account, &id)?;
            } else {
                self.store.block_sync(account, &id)?;
                self.store.append_event_once(
                    account,
                    &format!("blocked:{id}"),
                    "projection.blocked",
                    &json!({"batchId":id,"stream":"contact","commands":blockers}),
                )?;
            }
        }
        Ok(())
    }
    fn publish_contact_checkpoint(&self, account: &str, fence: &SessionFence) -> Result<()> {
        if self.store.pending_contact_sync_count(account)? != 0 {
            return Ok(());
        }
        let Some(received) = self.store.runtime(account, "contact-received-cursor")? else {
            return Ok(());
        };
        let (location, identity, path) = self.contact_store(account)?;
        let purpose = "contact-cursor-publication";
        let journal: Publication = match self.store.runtime(account, purpose)? {
            Some(sealed) => {
                serde_json::from_slice(&self.vault.open_record(account, purpose, &sealed)?)?
            }
            None => {
                let cursor = self
                    .vault
                    .open_record(account, "contact-received-cursor", &received)?
                    .to_vec();
                let data = checkpoint::read_cursor_file(&path, location.root(), 4 * 1024 * 1024)?;
                let metadata = checkpoint::read_cursor_file(
                    &path.with_extension("crc"),
                    location.root(),
                    4096,
                )?;
                if let Some(projected) = self.store.runtime(account, "contact-projected-cursor")? {
                    let prior =
                        self.vault
                            .open_record(account, "contact-projected-cursor", &projected)?;
                    if self.profile_contact_cursor(account)? != prior.as_slice() {
                        bail!("contact_profile_generation_changed");
                    }
                    if prior.as_slice() == cursor {
                        return Ok(());
                    }
                }
                let journal = Publication {
                    username: identity.username.clone(),
                    data,
                    metadata,
                    cursor,
                };
                let plain = zeroize::Zeroizing::new(serde_json::to_vec(&journal)?);
                self.store.put_runtime(
                    account,
                    purpose,
                    &self.vault.seal(account, purpose, &plain)?,
                )?;
                journal
            }
        };
        anyhow::ensure!(
            journal.username == identity.username,
            "account_identity_mismatch"
        );
        contact_sync::validate_cursor(&journal.cursor)?;
        let staged = wechat_profile::stage_account_contact_sync_key(
            &journal.data,
            &journal.metadata,
            &journal.username,
            &journal.cursor,
        )?;
        wechat_profile::publish_mmkv_append_guarded(
            &path,
            &staged,
            &self.write_options(account, purpose)?,
            || self.push_state(account, fence).is_ok(),
        )?;
        self.store.put_runtime(
            account,
            "contact-projected-cursor",
            &self
                .vault
                .seal(account, "contact-projected-cursor", &journal.cursor)?,
        )?;
        self.store.clear_runtime(account, purpose)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::{protobuf as p, wire::SyncCommand};
    fn fixture() -> (tempfile::TempDir, Arc<Store>, Arc<Core>, SessionFence) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let mut state = super::super::tests::state();
        state.database_password = vec![42; 32];
        state.auto_auth_key =
            p::message(&[p::bytes(1, &p::buffer(b"a")), p::bytes(2, &p::buffer(b"b"))]).unwrap();
        state.key_info = vec![4; 180];
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        let fence = SessionFence {
            authorization: authorization(&state).unwrap(),
            logout_epoch: None,
        };
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let fields = [
            ("flag".into(), wechat_profile::SqlValue::Integer("3".into())),
            (
                "local_type".into(),
                wechat_profile::SqlValue::Integer("1".into()),
            ),
            (
                "delete_flag".into(),
                wechat_profile::SqlValue::Integer("0".into()),
            ),
        ]
        .into_iter()
        .collect();
        wechat_profile::apply_contact_patch(
            &location,
            &keys,
            &wechat_profile::ContactProjection {
                username: "peer".into(),
                fields,
            },
            &core.write_options("one", "seed").unwrap(),
        )
        .unwrap();
        (dir, store, core, fence)
    }
    fn cursor(value: u64) -> Vec<u8> {
        p::bytes(
            1,
            &p::message(&[p::uint(1, 11), p::uint(2, value)]).unwrap(),
        )
    }
    fn batch(value: u64, muted: bool, unknown: bool) -> ContactSyncBatch {
        let mut commands = vec![SyncCommand {
            id: 2,
            payload: p::message(&[
                p::bytes(1, &p::bytes(1, b"peer")),
                p::uint(7, 512),
                p::uint(8, if muted { 512 } else { 0 }),
            ])
            .unwrap(),
        }];
        if unknown {
            commands.push(SyncCommand {
                id: 912,
                payload: b"retain me".to_vec(),
            });
        }
        ContactSyncBatch {
            cursor: cursor(value),
            continuation: vec![0],
            commands,
            raw: vec![value as u8],
        }
    }
    fn muted(core: &Core) -> Option<bool> {
        core.profile_reader("one")
            .unwrap()
            .thread_muted(&["peer".into()])
            .unwrap()["peer"]
    }
    #[test]
    fn independent_stream_updates_mute_retains_unknown_and_does_not_replay_stale_fields() {
        let (_dir, store, core, fence) = fixture();
        assert_eq!(muted(&core), Some(false));
        core.commit_contact_batch("one", batch(1, true, false))
            .unwrap();
        core.publish_contact_checkpoint("one", &fence).unwrap();
        assert_eq!(muted(&core), Some(true));
        assert_eq!(core.profile_contact_cursor("one").unwrap(), cursor(1));
        assert!(store.runtime("one", "received-cursor").unwrap().is_none());
        core.commit_contact_batch("one", batch(2, false, true))
            .unwrap();
        core.publish_contact_checkpoint("one", &fence).unwrap();
        assert_eq!(muted(&core), Some(false));
        assert_eq!(core.contact_cursor("one").unwrap(), cursor(2));
        assert_eq!(core.profile_contact_cursor("one").unwrap(), cursor(1));
        assert_eq!(store.pending_contact_sync_count("one").unwrap(), 1);
        core.commit_contact_batch("one", batch(3, true, false))
            .unwrap();
        store.retry_blocked("one").unwrap();
        core.project_contact_pending("one").unwrap();
        assert_eq!(muted(&core), Some(true));
        assert!(store.pending_sync("one").unwrap().is_empty());
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "contacts.updated")
                .count(),
            3
        );
    }
    #[test]
    fn group_mute_and_unmute_use_the_same_contact_stream_and_emit_saved_state() {
        let (_dir, store, core, fence) = fixture();
        let (location, keys) = core.profile_location_keys("one").unwrap();
        let room = "synthetic@chatroom";
        wechat_profile::apply_contact_patch(
            &location,
            &keys,
            &wechat_profile::ContactProjection {
                username: room.into(),
                fields: [
                    ("flag", 2),
                    ("local_type", 2),
                    ("delete_flag", 0),
                    ("chat_room_notify", 1),
                    ("is_in_chat_room", 1),
                ]
                .into_iter()
                .map(|(key, value)| {
                    (
                        key.into(),
                        wechat_profile::SqlValue::Integer(value.to_string()),
                    )
                })
                .collect(),
            },
            &core.write_options("one", "seed-room").unwrap(),
        )
        .unwrap();
        for (index, muted) in [true, false].into_iter().enumerate() {
            let mut next = batch(index as u64 + 1, false, false);
            next.commands = vec![SyncCommand {
                id: 2,
                payload: p::message(&[
                    p::bytes(1, &p::bytes(1, room.as_bytes())),
                    p::uint(7, u64::from(u32::MAX)),
                    p::uint(8, 2),
                    p::uint(17, u64::from(!muted)),
                    p::uint(85, 1),
                ])
                .unwrap(),
            }];
            core.commit_contact_batch("one", next).unwrap();
            core.publish_contact_checkpoint("one", &fence).unwrap();
            let reader = core.profile_reader("one").unwrap();
            assert_eq!(
                reader.thread_muted(&[room.into()]).unwrap()[room],
                Some(muted)
            );
            let mut rows = vec![json!({"id":room})];
            core.thread_summaries("one", &mut rows, &reader, None)
                .unwrap();
            assert_eq!(rows[0]["muted"], muted);
        }
        assert_eq!(store.pending_contact_sync_count("one").unwrap(), 0);
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(
                    |event| event.kind == "group.updated" && event.data["origin"] == "contact_sync"
                )
                .count(),
            2
        );
    }
    #[test]
    fn interrupted_cursor_publication_replays_and_obeys_logout_fence() {
        let (_dir, store, core, fence) = fixture();
        let (location, identity, path) = core.contact_store("one").unwrap();
        let journal = Publication {
            username: identity.username.clone(),
            data: std::fs::read(&path).unwrap(),
            metadata: std::fs::read(path.with_extension("crc")).unwrap(),
            cursor: cursor(1),
        };
        store
            .put_runtime(
                "one",
                "contact-received-cursor",
                &core
                    .vault
                    .seal("one", "contact-received-cursor", &journal.cursor)
                    .unwrap(),
            )
            .unwrap();
        store
            .put_runtime(
                "one",
                "contact-cursor-publication",
                &core
                    .vault
                    .seal(
                        "one",
                        "contact-cursor-publication",
                        &serde_json::to_vec(&journal).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
        store.put_runtime("one", "logout-epoch", b"new").unwrap();
        assert!(core.publish_contact_checkpoint("one", &fence).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), journal.data);
        store.clear_runtime("one", "logout-epoch").unwrap();
        let staged = wechat_profile::stage_account_contact_sync_key(
            &journal.data,
            &journal.metadata,
            &identity.username,
            &journal.cursor,
        )
        .unwrap();
        // Crash between data and metadata publication leaves the old prefix valid.
        std::fs::write(&path, staged.data()).unwrap();
        core.publish_contact_checkpoint("one", &fence).unwrap();
        assert_eq!(core.profile_contact_cursor("one").unwrap(), cursor(1));
        assert!(path.starts_with(location.root()));
        assert!(store
            .runtime("one", "contact-cursor-publication")
            .unwrap()
            .is_none());
    }
}
