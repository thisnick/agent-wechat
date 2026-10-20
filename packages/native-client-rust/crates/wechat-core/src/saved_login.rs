//! Accepted saved credentials are distinct from active session keys and cursors.
//! Publication uses an encrypted, replayable journal under the caller's account
//! gate; the official client must remain stopped until the generation completes.
use super::*;
#[derive(Serialize, Deserialize)]
struct SavedLoginJournal {
    username: String,
    uin: u32,
    generation: String,
    epoch: Option<Vec<u8>>,
    data: Vec<u8>,
    metadata: Vec<u8>,
    auto_auth_key: Vec<u8>,
    cookie: Vec<u8>,
    key_info: Vec<u8>,
    auto_login_flag: u32,
    key_info_plan: wechat_profile::SavedKeyInfoPlan,
}
impl Drop for SavedLoginJournal {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.data.zeroize();
        self.metadata.zeroize();
        self.auto_auth_key.zeroize();
        self.cookie.zeroize();
        self.key_info.zeroize();
        self.epoch.zeroize();
    }
}
fn credential_generation(state: &AccountState, epoch: Option<&[u8]>) -> Result<String> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&(
        state.uin,
        &state.own_username,
        &state.device.device_id,
        &state.device.device_type,
        state.device.client_version,
        &state.database_password,
        &state.auto_auth_key,
        &state.cookie,
        &state.key_info,
        epoch,
    ))?);
    Ok(format!("{:x}", Sha256::digest(bytes.as_slice())))
}
fn read_saved_file(path: &std::path::Path, root: &std::path::Path, max: u64) -> Result<Vec<u8>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    if std::fs::canonicalize(path)? != path || !path.starts_with(root) {
        bail!("invalid saved-login path");
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > max {
        bail!("invalid saved-login file");
    }
    let mut bytes = Vec::new();
    file.by_ref().take(max + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > max
        || before.len() != after.len()
        || before.modified()? != after.modified()?
    {
        bail!("saved-login source changed");
    }
    Ok(bytes)
}
fn require_saved_login_store(binding: &IdentityBinding) -> Result<()> {
    if binding.relative != "xwechat_files/all_users/config/global_config" {
        bail!("saved_login_store_unsupported");
    }
    Ok(())
}
fn pending_publication(result: Result<Value>) -> Result<Value> {
    match result {
        Err(error)
            if matches!(
                error.to_string().as_str(),
                "authorization_generation_changed"
                    | "saved_login_generation_changed"
                    | "saved_login_acceptance_unproven"
                    | "auth_required"
            ) =>
        {
            Ok(json!({"state":"pending","reason":"authorization_generation_changed"}))
        }
        Err(error)
            if matches!(
                error.downcast_ref::<wechat_profile::ProfileError>(),
                Some(wechat_profile::ProfileError::UnsupportedSchema(_))
            ) =>
        {
            Ok(json!({"state":"pending","reason":"saved_login_schema_unsupported"}))
        }
        Err(error)
            if matches!(
                error.downcast_ref::<wechat_profile::ProfileError>(),
                Some(wechat_profile::ProfileError::SourceChanged)
            ) =>
        {
            Ok(json!({"state":"pending","reason":"saved_login_profile_changed"}))
        }
        Err(error) if error.to_string() == "saved_login_store_unsupported" => {
            Ok(json!({"state":"pending","reason":"saved_login_store_unsupported"}))
        }
        result => result,
    }
}
impl Core {
    fn saved_login_guard(
        &self,
        account: &str,
        generation: &str,
        epoch: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        if self.store.runtime(account, "logged-out")?.is_some()
            || self.store.runtime(account, "logout-epoch")?.as_deref() != epoch
        {
            bail!("authorization_generation_changed");
        }
        let cipher = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let accepted = format!("{:x}", Sha256::digest(&cipher));
        if self
            .store
            .runtime(account, "saved-login-accepted-state-generation")?
            .as_deref()
            != Some(accepted.as_bytes())
        {
            bail!("saved_login_acceptance_unproven");
        }
        let state: AccountState =
            serde_json::from_slice(&self.vault.open_record(account, "account-state", &cipher)?)?;
        state.validate()?;
        if credential_generation(&state, epoch)? != generation {
            bail!("saved_login_generation_changed");
        }
        self.check_state_profile(account, &state)?;
        // Password validation can be slow on a newly opened profile. Recheck
        // the complete admitted state and logout epoch atomically afterward,
        // immediately before returning to an individual file publisher.
        if !self
            .store
            .saved_login_authorization_current(account, &cipher, epoch)?
        {
            bail!("authorization_generation_changed");
        }
        Ok(cipher)
    }
    fn saved_login_matches(
        &self,
        account: &str,
        state: &AccountState,
        format_key: &[u8],
    ) -> Result<bool> {
        let saved = wechat_profile::read_profile_saved_login(
            self.store.account(account)?.profile_path,
            format_key,
            true,
        )?;
        Ok(saved.identity.username == state.own_username
            && saved.identity.uin == state.uin
            && saved.identity.device_id == state.device.device_id
            && saved.auto_auth_key.as_slice() == state.auto_auth_key
            && saved.cookie.as_slice() == state.cookie
            && saved.key_info.as_slice() == state.key_info
            && saved.logout_locked == Some(false))
    }
    fn publish_saved_login_journal(
        &self,
        account: &str,
        journal: &SavedLoginJournal,
        sealed: &[u8],
    ) -> Result<Value> {
        let Some(binding) = self.identity_binding(account)? else {
            bail!("profile_identity_unavailable");
        };
        require_saved_login_store(&binding)?;
        let identity = self.read_bound_identity(account, &binding)?;
        if identity.username != journal.username || identity.uin != journal.uin {
            bail!("account_identity_mismatch");
        }
        self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())?;
        let home = self.store.account(account)?.profile_path;
        let path = home.join(&binding.relative);
        let options = self.write_options(account, "saved-login-publication")?;
        let mut fence_error = None;
        let publication = wechat_profile::publish_saved_key_info_guarded(
            &home,
            &journal.username,
            &journal.key_info_plan,
            &journal.key_info,
            &options,
            || match self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())
            {
                Ok(_) => true,
                Err(error) => {
                    fence_error = Some(error);
                    false
                }
            },
        );
        if let Some(error) = fence_error {
            return Err(error);
        }
        let key_info = publication?;
        self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())?;
        let staged = wechat_profile::stage_accepted_saved_login_projection(
            &journal.data,
            &journal.metadata,
            &binding.format_key,
            &journal.username,
            &wechat_profile::SavedLoginProjection {
                auto_auth_key: &journal.auto_auth_key,
                cookie: &journal.cookie,
                key_info: &journal.key_info,
                auto_login_flag: journal.auto_login_flag,
                pc_account_name: None,
                local_device_id64: None,
            },
        )?;
        self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())?;
        let mut fence_error = None;
        let published =
            wechat_profile::publish_mmkv_append_guarded(&path, &staged, &options, || {
                match self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())
                {
                    Ok(_) => true,
                    Err(error) => {
                        fence_error = Some(error);
                        false
                    }
                }
            });
        if let Some(error) = fence_error {
            return Err(error);
        }
        published?;
        let cipher =
            self.saved_login_guard(account, &journal.generation, journal.epoch.as_deref())?;
        let state: AccountState =
            serde_json::from_slice(&self.vault.open_record(account, "account-state", &cipher)?)?;
        if !self.saved_login_matches(account, &state, &binding.format_key)? {
            bail!("saved_login_readback_mismatch");
        }
        self.store.finish_saved_login_publication(
            account,
            Some(sealed),
            &cipher,
            journal.epoch.as_deref(),
            &journal.generation,
        )?;
        Ok(
            json!({"state":"complete","databaseUpdated":key_info.database_changed,"legacyUpdated":key_info.legacy_changed}),
        )
    }
    /// Call only after installing a server-accepted auth/renewal response, while
    /// holding the account gate. Unvalidated credential import must not call this.
    pub(super) fn project_saved_login(
        &self,
        account: &str,
        accepted: &AccountState,
    ) -> Result<Value> {
        pending_publication(self.project_saved_login_inner(account, accepted))
    }
    fn project_saved_login_inner(&self, account: &str, accepted: &AccountState) -> Result<Value> {
        accepted.validate()?;
        let Some(binding) = self.identity_binding(account)? else {
            return Ok(json!({"state":"pending","reason":"profile_identity_unavailable"}));
        };
        require_saved_login_store(&binding)?;
        let epoch = self.store.runtime(account, "logout-epoch")?;
        let generation = credential_generation(accepted, epoch.as_deref())?;
        let cipher = self.saved_login_guard(account, &generation, epoch.as_deref())?;
        let identity = self.read_bound_identity(account, &binding)?;
        if identity.username != accepted.own_username
            || identity.uin != accepted.uin
            || identity.device_id != accepted.device.device_id
        {
            bail!("account_identity_mismatch");
        }
        let old = self.store.runtime(account, "saved-login-publication")?;
        if let Some(sealed) = old.as_ref() {
            let journal: SavedLoginJournal = serde_json::from_slice(&self.vault.open_record(
                account,
                "saved-login-publication",
                sealed,
            )?)?;
            if journal.generation == generation && journal.epoch == epoch {
                return self.publish_saved_login_journal(account, &journal, sealed);
            }
            // A newer durably accepted generation may supersede an interrupted
            // old one, including recovery after its accepted-state commit. The
            // Store archives the old sealed intent and swaps by exact state CAS.
        } else if self
            .store
            .runtime(account, "saved-login-projected-generation")?
            .as_deref()
            == Some(generation.as_bytes())
        {
            if !self.saved_login_matches(account, accepted, &binding.format_key)? {
                self.store.put_runtime(
                    account,
                    "saved-login-projection-needed",
                    b"profile_generation_changed",
                )?;
                bail!("saved_login_profile_changed");
            }
            self.store.finish_saved_login_publication(
                account,
                None,
                &cipher,
                epoch.as_deref(),
                &generation,
            )?;
            return Ok(json!({"state":"complete","unchanged":true}));
        }
        let home = self.store.account(account)?.profile_path;
        let path = home.join(&binding.relative);
        let data = read_saved_file(&path, &home, 4 * 1024 * 1024 + 4)?;
        let metadata = read_saved_file(&path.with_extension("crc"), &home, 4096)?;
        let auto_login_flag =
            wechat_profile::saved_login_policy(&data, &metadata, &binding.format_key)?;
        let journal = SavedLoginJournal {
            username: accepted.own_username.clone(),
            uin: accepted.uin,
            generation: generation.clone(),
            epoch: epoch.clone(),
            data,
            metadata,
            auto_auth_key: accepted.auto_auth_key.clone(),
            cookie: accepted.cookie.clone(),
            key_info: accepted.key_info.clone(),
            auto_login_flag,
            key_info_plan: wechat_profile::plan_saved_key_info(
                &home,
                &accepted.own_username,
                true,
            )?,
        };
        let plain = zeroize::Zeroizing::new(serde_json::to_vec(&journal)?);
        let sealed = self
            .vault
            .seal(account, "saved-login-publication", &plain)?;
        if !self.store.stage_saved_login_publication(
            account,
            &cipher,
            epoch.as_deref(),
            old.as_deref(),
            &sealed,
        )? {
            return Ok(json!({"state":"pending","reason":"publication_generation_changed"}));
        }
        self.publish_saved_login_journal(account, &journal, &sealed)
    }
    /// Recover a sealed journal, or stage the narrow pre-journal crash window
    /// only when the exact current encrypted state has a durable acceptance tag.
    /// Imported credentials alone do not authorize profile publication.
    pub(super) fn recover_saved_login(&self, account: &str) -> Result<Value> {
        pending_publication(self.recover_saved_login_inner(account))
    }
    fn recover_saved_login_inner(&self, account: &str) -> Result<Value> {
        let sealed = self.store.runtime(account, "saved-login-publication")?;
        let needed = self
            .store
            .runtime(account, "saved-login-projection-needed")?
            .is_some();
        if sealed.is_none() && !needed {
            return Ok(json!({"state":"unchanged"}));
        }
        if self.store.runtime(account, "logged-out")?.is_some() {
            bail!("authorization_generation_changed");
        }
        let cipher = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let hash = format!("{:x}", Sha256::digest(&cipher));
        if self
            .store
            .runtime(account, "saved-login-accepted-state-generation")?
            .as_deref()
            != Some(hash.as_bytes())
        {
            bail!("saved_login_acceptance_unproven");
        }
        let state: AccountState =
            serde_json::from_slice(&self.vault.open_record(account, "account-state", &cipher)?)?;
        let epoch = self.store.runtime(account, "logout-epoch")?;
        if let Some(sealed) = sealed {
            let journal: SavedLoginJournal = serde_json::from_slice(&self.vault.open_record(
                account,
                "saved-login-publication",
                &sealed,
            )?)?;
            if journal.epoch == epoch
                && journal.generation == credential_generation(&state, epoch.as_deref())?
            {
                return self.publish_saved_login_journal(account, &journal, &sealed);
            }
            if !needed {
                bail!("saved_login_generation_changed");
            }
            // Only the durable exact accepted-state proof above can authorize
            // replacing a stale intent; a generic restored state cannot do so.
        }
        self.project_saved_login_inner(account, &state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::protobuf as p;
    #[test]
    fn saved_login_requires_the_known_global_store_not_an_arbitrary_identity_file() {
        let binding = IdentityBinding {
            relative: "xwechat_files/all_users/config/global_config".into(),
            format_key: vec![9; 16],
        };
        require_saved_login_store(&binding).unwrap();
        for relative in [
            "xwechat_files/all_users/config/other",
            "xwechat_files/synthetic/config/login_configv2",
            "../global_config",
        ] {
            let binding = IdentityBinding {
                relative: relative.into(),
                format_key: vec![9; 16],
            };
            let pending = pending_publication(
                require_saved_login_store(&binding).map(|_| json!({"state":"complete"})),
            )
            .unwrap();
            assert_eq!(pending["reason"], "saved_login_store_unsupported");
        }
    }
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
            auto_auth_key: token(1),
            key_info: vec![4; 180],
            database_password: vec![42; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    fn token(n: u8) -> Vec<u8> {
        p::message(&[
            p::bytes(1, &p::buffer(&[n; 8])),
            p::bytes(2, &p::buffer(&[n + 1; 8])),
        ])
        .unwrap()
    }
    fn accept(core: &Core, state: &AccountState) {
        let cipher = core
            .vault
            .seal("one", "account-state", &serde_json::to_vec(state).unwrap())
            .unwrap();
        core.store
            .save_accepted_authorization("one", &cipher)
            .unwrap();
    }
    fn snapshot(core: &Core) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let home = core.store.account("one").unwrap().profile_path;
        let global = home.join("xwechat_files/all_users/config/global_config");
        (
            std::fs::read(&global).unwrap(),
            std::fs::read(global.with_extension("crc")).unwrap(),
            std::fs::read(home.join(".xwechat/login/synthetic_self/key_info.dat")).unwrap(),
        )
    }
    fn events(core: &Core) -> usize {
        core.store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "credentials.projected")
            .count()
    }
    fn journal(core: &Core, state: &AccountState) -> (SavedLoginJournal, Vec<u8>) {
        let home = core.store.account("one").unwrap().profile_path;
        let binding = core.identity_binding("one").unwrap().unwrap();
        let path = home.join(&binding.relative);
        let epoch = core.store.runtime("one", "logout-epoch").unwrap();
        let generation = credential_generation(state, epoch.as_deref()).unwrap();
        let record = SavedLoginJournal {
            username: state.own_username.clone(),
            uin: state.uin,
            generation,
            epoch,
            data: std::fs::read(&path).unwrap(),
            metadata: std::fs::read(path.with_extension("crc")).unwrap(),
            auto_auth_key: state.auto_auth_key.clone(),
            cookie: state.cookie.clone(),
            key_info: state.key_info.clone(),
            auto_login_flag: 0,
            key_info_plan: wechat_profile::plan_saved_key_info(&home, &state.own_username, true)
                .unwrap(),
        };
        let sealed = core
            .vault
            .seal(
                "one",
                "saved-login-publication",
                &serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        let current = core.store.runtime("one", "account-state").unwrap().unwrap();
        assert!(core
            .store
            .stage_saved_login_publication("one", &current, record.epoch.as_deref(), None, &sealed)
            .unwrap());
        (record, sealed)
    }
    #[test]
    fn saved_login_encrypted_profile_restart_generation_and_fences() {
        use md5::{Digest as _, Md5};
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("data");
        let mut store = Arc::new(Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let mut core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let mut accepted = state();
        core.install_login_state("one", &accepted, None).unwrap();
        core.ensure_managed_profile("one", &accepted).unwrap();
        let home = store.account("one").unwrap().profile_path;
        // Authoritative native key-info database, plus a distinct unrelated row.
        let folder = home.join("xwechat_files/all_users/login/synthetic_self");
        std::fs::create_dir_all(&folder).unwrap();
        let database = folder.join("key_info.db");
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);CREATE UNIQUE INDEX LoginKeyInfoTable_USER_KEYINFO ON LoginKeyInfoTable(user_name_md5,key_info_md5);").unwrap();
        db.execute(
            "INSERT INTO LoginKeyInfoTable VALUES(?1,'',?2,?3)",
            rusqlite::params![
                format!("{:x}", Md5::digest(accepted.own_username.as_bytes())),
                format!("{:x}", Md5::digest(&accepted.key_info)),
                accepted.key_info
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO LoginKeyInfoTable VALUES('unrelated','opaque','unrelated',x'0102')",
            [],
        )
        .unwrap();
        drop(db);
        // Keep local launch policy disabled and an unrelated setting across auth.
        let binding = core.identity_binding("one").unwrap().unwrap();
        let global = home.join(&binding.relative);
        let changed = wechat_profile::stage_mmkv_values(
            &std::fs::read(&global).unwrap(),
            &std::fs::read(global.with_extension("crc")).unwrap(),
            &binding.format_key,
            &[
                ("mmkv_key_auto_login_flag".into(), Some(vec![0])),
                ("mmkv_key_logout_locked_flag".into(), Some(vec![1])),
                ("synthetic_unrelated".into(), Some(vec![9])),
            ],
        )
        .unwrap();
        wechat_profile::publish_mmkv_append(
            &global,
            &changed,
            &core.write_options("one", "synthetic-policy").unwrap(),
        )
        .unwrap();
        let initial_events = events(&core);
        // Durable accepted tag recovers a crash before any journal was staged.
        accepted.auto_auth_key = token(3);
        accepted.cookie.clear();
        accepted.key_info = vec![5; 180];
        accept(&core, &accepted);
        assert!(store
            .runtime("one", "saved-login-publication")
            .unwrap()
            .is_none());
        assert_eq!(
            core.recover_saved_login("one").unwrap()["state"],
            "complete"
        );
        assert_eq!(events(&core), initial_events + 1);
        let saved = wechat_profile::read_profile_saved_login(&home, &[9; 16], true).unwrap();
        assert_eq!(saved.auto_login_flag, 0);
        assert_eq!(saved.logout_locked, Some(false));
        assert!(saved.cookie.is_empty());
        for phase in 0..3 {
            accepted.auto_auth_key = token(5 + phase);
            accepted.cookie = vec![5 + phase; 15];
            accepted.key_info = vec![7 + phase; 180];
            accept(&core, &accepted);
            let (record, _sealed) = journal(&core, &accepted);
            if phase > 0 {
                wechat_profile::publish_saved_key_info(
                    &home,
                    &record.username,
                    &record.key_info_plan,
                    &record.key_info,
                    &core
                        .write_options("one", "synthetic-keyinfo-crash")
                        .unwrap(),
                )
                .unwrap();
                let staged = wechat_profile::stage_accepted_saved_login_projection(
                    &record.data,
                    &record.metadata,
                    &binding.format_key,
                    &record.username,
                    &wechat_profile::SavedLoginProjection {
                        auto_auth_key: &record.auto_auth_key,
                        cookie: &record.cookie,
                        key_info: &record.key_info,
                        auto_login_flag: record.auto_login_flag,
                        pc_account_name: None,
                        local_device_id64: None,
                    },
                )
                .unwrap();
                if phase == 1 {
                    std::fs::write(&global, staged.data()).unwrap();
                } else {
                    wechat_profile::publish_mmkv_append(
                        &global,
                        &staged,
                        &core.write_options("one", "synthetic-pair-crash").unwrap(),
                    )
                    .unwrap();
                }
            }
            drop(core);
            drop(store);
            store = Arc::new(Store::open_existing(&root).unwrap());
            core = Core::new(store.clone(), None).unwrap();
            let recovered = core.recover_saved_login("one").unwrap();
            assert!(matches!(
                recovered["state"].as_str(),
                Some("complete" | "unchanged")
            ));
            let saved = wechat_profile::read_profile_saved_login(&home, &[9; 16], true).unwrap();
            assert_eq!(saved.auto_auth_key.as_slice(), accepted.auto_auth_key);
            assert_eq!(saved.cookie.as_slice(), accepted.cookie);
            assert_eq!(saved.key_info.as_slice(), accepted.key_info);
            assert_eq!(saved.auto_login_flag, 0);
            assert_eq!(events(&core), initial_events + phase as usize + 2);
            assert!(store
                .runtime("one", "saved-login-publication")
                .unwrap()
                .is_none());
            assert_eq!(
                core.recover_saved_login("one").unwrap()["state"],
                "unchanged"
            );
        }
        let before = snapshot(&core);
        let count = events(&core);
        // A differently sealed unvalidated import must not inherit old proof.
        let imported = core
            .vault
            .seal(
                "one",
                "account-state",
                &serde_json::to_vec(&accepted).unwrap(),
            )
            .unwrap();
        store.save_active_authorization("one", &imported).unwrap();
        store
            .put_runtime("one", "saved-login-projection-needed", b"1")
            .unwrap();
        assert_eq!(core.recover_saved_login("one").unwrap()["state"], "pending");
        assert_eq!(before, snapshot(&core));
        assert_eq!(events(&core), count);
        store.save_accepted_authorization("one", &imported).unwrap();
        assert_eq!(
            core.recover_saved_login("one").unwrap()["state"],
            "complete"
        );
        assert_eq!(events(&core), count);
        assert_eq!(before, snapshot(&core));
        // A crash after newer accepted-state commit but before replacing an old
        // journal must recover the new proof, without publishing the old token.
        accepted.auto_auth_key = token(10);
        accept(&core, &accepted);
        let (_record, old) = journal(&core, &accepted);
        accepted.auto_auth_key = token(11);
        accept(&core, &accepted);
        assert_eq!(
            core.recover_saved_login("one").unwrap()["state"],
            "complete"
        );
        assert_eq!(
            store
                .runtime(
                    "one",
                    &format!("saved-login-superseded:{:x}", Sha256::digest(&old))
                )
                .unwrap()
                .unwrap(),
            old
        );
        assert_eq!(
            wechat_profile::read_profile_saved_login(&home, &[9; 16], true)
                .unwrap()
                .auto_auth_key
                .as_slice(),
            accepted.auto_auth_key
        );
        let before = snapshot(&core);
        // New epoch fences an interrupted old journal without publishing files.
        accepted.auto_auth_key = token(12);
        accept(&core, &accepted);
        let (_record, old) = journal(&core, &accepted);
        store
            .put_runtime("one", "logout-epoch", b"synthetic-new-epoch")
            .unwrap();
        store.put_runtime("one", "logged-out", b"1").unwrap();
        assert_eq!(core.recover_saved_login("one").unwrap()["state"], "pending");
        assert_eq!(before, snapshot(&core));
        assert_eq!(
            store
                .runtime("one", "saved-login-publication")
                .unwrap()
                .unwrap(),
            old
        );
        core.install_login_state("one", &accepted, Some(b"synthetic-new-epoch"))
            .unwrap();
        assert_eq!(
            core.project_saved_login("one", &accepted).unwrap()["state"],
            "complete"
        );
        assert_eq!(
            store
                .runtime(
                    "one",
                    &format!("saved-login-superseded:{:x}", Sha256::digest(&old))
                )
                .unwrap()
                .unwrap(),
            old
        );
        // An accepted wrong DB password cannot modify any official login file.
        let before = snapshot(&core);
        let valid = store.runtime("one", "account-state").unwrap().unwrap();
        accepted.database_password = vec![99; 32];
        let bad = core
            .vault
            .seal(
                "one",
                "account-state",
                &serde_json::to_vec(&accepted).unwrap(),
            )
            .unwrap();
        store.save_accepted_authorization("one", &bad).unwrap();
        assert!(core.recover_saved_login("one").is_err());
        assert_eq!(before, snapshot(&core));
        store.save_accepted_authorization("one", &valid).unwrap();
        // Unknown authoritative schema is a durable pending result, not a login retry.
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch("CREATE TRIGGER synthetic_unknown AFTER INSERT ON LoginKeyInfoTable BEGIN SELECT 1;END;").unwrap();
        drop(db);
        accepted.database_password = vec![42; 32];
        accepted.auto_auth_key = token(14);
        accept(&core, &accepted);
        assert_eq!(
            core.recover_saved_login("one").unwrap()["reason"],
            "saved_login_schema_unsupported"
        );
        assert_eq!(before, snapshot(&core));
        assert!(store
            .runtime("one", "saved-login-projection-needed")
            .unwrap()
            .is_some());
        let db = rusqlite::Connection::open(&database).unwrap();
        let other:(String,Vec<u8>)=db.query_row("SELECT key_md5,key_info_data FROM LoginKeyInfoTable WHERE user_name_md5='unrelated'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(other, ("opaque".into(), vec![1, 2]));
        let records = wechat_profile::decode_mmkv_records(
            &std::fs::read(&global).unwrap(),
            &std::fs::read(global.with_extension("crc")).unwrap(),
            &[9; 16],
        )
        .unwrap();
        assert_eq!(
            records.raw_value("synthetic_unrelated"),
            Some([9].as_slice())
        );
    }
}
