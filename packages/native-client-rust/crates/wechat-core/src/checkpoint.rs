//! Official account cursor publication follows the ordinary-file durability barrier.
//! The encrypted journal binds both original MMKV files and received generation;
//! its retirement and public commit event share one sidecar transaction.
use super::*;
#[derive(Serialize, Deserialize)]
struct CursorPublication {
    username: String,
    data: Vec<u8>,
    metadata: Vec<u8>,
    cursor: Vec<u8>,
    #[serde(default)]
    received: Vec<u8>,
}
impl Drop for CursorPublication {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.data.zeroize();
        self.metadata.zeroize();
        self.cursor.zeroize();
        self.received.zeroize();
    }
}
fn read_cursor_file(path: &std::path::Path, root: &std::path::Path, max: u64) -> Result<Vec<u8>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    if std::fs::canonicalize(path)? != path || !path.starts_with(root) {
        bail!("invalid account cursor path");
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > max {
        bail!("invalid account cursor file");
    }
    let mut bytes = Vec::new();
    file.by_ref().take(max + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > max
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        bail!("account cursor source changed");
    }
    Ok(bytes)
}
impl Core {
    pub(super) fn publish_profile_checkpoint(&self, account: &str) -> Result<()> {
        if self.store.pending_sync_count(account)? != 0 {
            return Ok(());
        }
        let Some(binding) = self.identity_binding(account)? else {
            return Ok(());
        };
        let identity = self.read_bound_identity(account, &binding)?;
        let (location, _) = self.profile_location_keys(account)?;
        self.ensure_account_sync_store(account, &identity, &location)?;
        let path = wechat_profile::account_sync_store_path(&location, &identity)?;
        let options = self.write_options(account, "cursor-publication")?;
        // One old interrupted generation and the latest generation normally need
        // two iterations. A concurrent change may use the remaining bounded retry;
        // its journal/needed marker remains durable if more work arrives.
        for _ in 0..3 {
            if self.store.pending_sync_count(account)? != 0 {
                return Ok(());
            }
            let (mut journal, sealed_journal) =
                if let Some(sealed) = self.store.runtime(account, "cursor-publication")? {
                    (
                        serde_json::from_slice::<CursorPublication>(&self.vault.open_record(
                            account,
                            "cursor-publication",
                            &sealed,
                        )?)?,
                        sealed,
                    )
                } else {
                    let Some(received) = self.store.runtime(account, "received-cursor")? else {
                        return Ok(());
                    };
                    let cursor = self
                        .vault
                        .open_record(account, "received-cursor", &received)?
                        .to_vec();
                    wechat_protocol::wire::decode_cursor(&cursor)?;
                    let data = read_cursor_file(&path, location.root(), 4 * 1024 * 1024)?;
                    let metadata =
                        read_cursor_file(&path.with_extension("crc"), location.root(), 4096)?;
                    if let Some(projected) = self.store.runtime(account, "projected-cursor")? {
                        let prior =
                            self.vault
                                .open_record(account, "projected-cursor", &projected)?;
                        let key = wechat_profile::account_mmkv_key(&identity.username)?;
                        let records = wechat_profile::decode_mmkv_records(&data, &metadata, &key)?;
                        let actual = records
                            .string_value("mmkv_key_sync_key")?
                            .unwrap_or_default();
                        if actual.as_slice() != prior.as_slice() {
                            self.store.put_runtime(
                                account,
                                "cursor-projection-needed",
                                b"profile_generation_changed",
                            )?;
                            bail!("profile_checkpoint_generation_changed");
                        }
                    }
                    let hash = format!("{:x}", Sha256::digest(&cursor));
                    if self
                        .store
                        .runtime(account, "projected-cursor-hash")?
                        .as_deref()
                        == Some(hash.as_bytes())
                        && self
                            .store
                            .runtime(account, "projected-received-generation")?
                            .as_deref()
                            == Some(format!("{:x}", Sha256::digest(&received)).as_bytes())
                        && !self.store.pending_cursor_projection(account)?
                    {
                        return Ok(());
                    }
                    let journal = CursorPublication {
                        username: identity.username.clone(),
                        data,
                        metadata,
                        cursor,
                        received,
                    };
                    let plain = zeroize::Zeroizing::new(serde_json::to_vec(&journal)?);
                    let sealed = self.vault.seal(account, "cursor-publication", &plain)?;
                    if !self
                        .store
                        .stage_cursor_projection(account, &journal.received, &sealed)?
                    {
                        continue;
                    }
                    (journal, sealed)
                };
            if journal.username != identity.username {
                bail!("account_identity_mismatch");
            }
            wechat_protocol::wire::decode_cursor(&journal.cursor)?;
            if journal.received.is_empty() {
                // Older journals lacked the received-generation binding. They
                // predate this CAS protocol but still bind original file bytes.
                // Never clear a newer cursor's needed marker when replaying one.
                let current = self
                    .store
                    .runtime(account, "received-cursor")?
                    .context("received cursor unavailable")?;
                journal.received = if self
                    .vault
                    .open_record(account, "received-cursor", &current)?
                    .as_slice()
                    == journal.cursor
                {
                    current
                } else {
                    b"legacy-unbound-generation".to_vec()
                };
            }
            let staged = wechat_profile::stage_account_sync_key(
                &journal.data,
                &journal.metadata,
                &journal.username,
                &journal.cursor,
            )?;
            wechat_profile::publish_mmkv_append(&path, &staged, &options)?;
            let hash = format!("{:x}", Sha256::digest(&journal.cursor));
            self.store.finish_cursor_projection(
                account,
                &hash,
                &self
                    .vault
                    .seal(account, "projected-cursor", &journal.cursor)?,
                &sealed_journal,
                &journal.received,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::protobuf as p;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![b'A'; 16],
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 7,
            own_username: "synthetic_self".into(),
            cookie: vec![1; 15],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: p::message(&[
                p::bytes(1, &p::buffer(b"synthetic-a")),
                p::bytes(2, &p::buffer(b"synthetic-b")),
            ])
            .unwrap(),
            key_info: vec![4; 180],
            database_password: vec![42; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    fn cursor(value: u32) -> Vec<u8> {
        p::message(&[
            p::uint(1, 1),
            p::bytes(
                2,
                &p::message(&[p::uint(1, 1), p::uint(2, value as u64)]).unwrap(),
            ),
        ])
        .unwrap()
    }
    fn staged_journal(core: &Core, value: u32) -> (CursorPublication, Vec<u8>, std::path::PathBuf) {
        let cursor = cursor(value);
        let received = core.vault.seal("one", "received-cursor", &cursor).unwrap();
        core.store
            .restore_received_cursor("one", &received)
            .unwrap();
        let binding = core.identity_binding("one").unwrap().unwrap();
        let identity = core.read_bound_identity("one", &binding).unwrap();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let path = wechat_profile::account_sync_store_path(&location, &identity).unwrap();
        let journal = CursorPublication {
            username: identity.username,
            data: std::fs::read(&path).unwrap(),
            metadata: std::fs::read(path.with_extension("crc")).unwrap(),
            cursor,
            received,
        };
        let sealed = core
            .vault
            .seal(
                "one",
                "cursor-publication",
                &serde_json::to_vec(&journal).unwrap(),
            )
            .unwrap();
        assert!(core
            .store
            .stage_cursor_projection("one", &journal.received, &sealed)
            .unwrap());
        (journal, sealed, path)
    }
    fn verify(core: &Core, value: u32, expected_events: usize) {
        let binding = core.identity_binding("one").unwrap().unwrap();
        let identity = core.read_bound_identity("one", &binding).unwrap();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let got = wechat_profile::read_account_sync_key(
            &location,
            &identity,
            &wechat_profile::ReadOptions {
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(got.as_slice(), cursor(value));
        assert!(!core.store.pending_cursor_projection("one").unwrap());
        let projected = core
            .store
            .runtime("one", "projected-cursor")
            .unwrap()
            .unwrap();
        assert_eq!(
            core.vault
                .open_record("one", "projected-cursor", &projected)
                .unwrap()
                .as_slice(),
            cursor(value)
        );
        assert_eq!(
            core.store
                .events("one", 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "sync.committed")
                .count(),
            expected_events
        );
    }
    #[test]
    fn encrypted_profile_checkpoint_restarts_cas_and_newer_generation_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let mut store = Arc::new(Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let mut core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let state = state();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        // A blocked earlier batch must prevent even a valid later cursor from
        // touching the official account store or creating a commit event.
        let received = core
            .vault
            .seal("one", "received-cursor", &cursor(1))
            .unwrap();
        store
            .stage_sync("one", "unapplied", b"synthetic sealed commands", &received)
            .unwrap();
        store.block_sync("one", "unapplied").unwrap();
        core.publish_profile_checkpoint("one").unwrap();
        assert!(store
            .runtime("one", "cursor-publication")
            .unwrap()
            .is_none());
        assert!(store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .all(|e| e.kind != "sync.committed"));
        store.finish_sync("one", "unapplied").unwrap();
        for phase in 0..3 {
            let value = phase + 1;
            let (journal, _sealed, path) = staged_journal(&core, value);
            let staged = wechat_profile::stage_account_sync_key(
                &journal.data,
                &journal.metadata,
                &journal.username,
                &journal.cursor,
            )
            .unwrap();
            if phase == 1 {
                // Data-first crash: old metadata still validates its old prefix.
                std::fs::write(&path, staged.data()).unwrap();
            } else if phase == 2 {
                // Both official files committed, sidecar receipt/event not yet.
                wechat_profile::publish_mmkv_append(
                    &path,
                    &staged,
                    &core.write_options("one", "synthetic-before-crash").unwrap(),
                )
                .unwrap();
            }
            drop(core);
            drop(store);
            store = Arc::new(Store::open_existing(&root).unwrap());
            core = Core::new(store.clone(), None).unwrap();
            core.publish_profile_checkpoint("one").unwrap();
            verify(&core, value, value as usize);
            core.publish_profile_checkpoint("one").unwrap();
            verify(&core, value, value as usize);
        }
        let (journal, _sealed, path) = staged_journal(&core, 4);
        let mut unexpected = journal.metadata.clone();
        unexpected[8] ^= 1;
        std::fs::write(path.with_extension("crc"), unexpected).unwrap();
        assert!(core.publish_profile_checkpoint("one").is_err());
        assert!(store.pending_cursor_projection("one").unwrap());
        assert_eq!(
            store
                .events("one", 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "sync.committed")
                .count(),
            3
        );
        // Restore only this synthetic fixture's exact old metadata; then replay
        // the already journaled original generation, without creating a new one.
        std::fs::write(path.with_extension("crc"), &journal.metadata).unwrap();
        core.publish_profile_checkpoint("one").unwrap();
        verify(&core, 4, 4);
        let (_journal, _sealed, _path) = staged_journal(&core, 5);
        let later = core
            .vault
            .seal("one", "received-cursor", &cursor(6))
            .unwrap();
        store.restore_received_cursor("one", &later).unwrap();
        core.publish_profile_checkpoint("one").unwrap();
        verify(&core, 6, 6);
        let events = store
            .events("one", 0, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "sync.committed")
            .collect::<Vec<_>>();
        assert_eq!(events[4].data["currentReceivedCheckpoint"], false);
        assert_eq!(events[5].data["currentReceivedCheckpoint"], true);
        // An independently changed but valid profile cursor is not an invitation
        // to overwrite it with an older sidecar cursor, even on an apparent no-op.
        let binding = core.identity_binding("one").unwrap().unwrap();
        let identity = core.read_bound_identity("one", &binding).unwrap();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let path = wechat_profile::account_sync_store_path(&location, &identity).unwrap();
        let external = wechat_profile::stage_account_sync_key(
            &std::fs::read(&path).unwrap(),
            &std::fs::read(path.with_extension("crc")).unwrap(),
            &identity.username,
            &cursor(7),
        )
        .unwrap();
        wechat_profile::publish_mmkv_append(
            &path,
            &external,
            &core
                .write_options("one", "synthetic-external-change")
                .unwrap(),
        )
        .unwrap();
        assert!(core
            .publish_profile_checkpoint("one")
            .unwrap_err()
            .to_string()
            .contains("profile_checkpoint_generation_changed"));
        assert!(store.pending_cursor_projection("one").unwrap());
        assert_eq!(
            store
                .events("one", 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "sync.committed")
                .count(),
            6
        );
        let actual = wechat_profile::read_account_sync_key(
            &location,
            &identity,
            &wechat_profile::ReadOptions {
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(actual.as_slice(), cursor(7));
    }
}
