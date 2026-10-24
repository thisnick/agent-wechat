//! Managed fresh profiles use only the accepted account credential generation.
use super::*;
use std::path::Path;
use wechat_profile::{
    KeyFile, OfficialHomeBootstrap, ProfileLocation, ReadOptions, SavedLoginProjection,
};
impl Core {
    pub fn configure_profile_format_key(&self, key: &[u8]) -> Result<()> {
        if key.len() != 16 {
            bail!("invalid profile format key");
        }
        *self
            .profile_format_key
            .lock()
            .map_err(|_| anyhow::anyhow!("profile initializer unavailable"))? =
            Some(zeroize::Zeroizing::new(key.to_vec()));
        self.recover_managed_profiles()
    }
    /// Call before account workers start; one incompatible account is reported
    /// locally while other account actors remain usable.
    pub fn recover_managed_profiles(&self) -> Result<()> {
        for account in self.store.accounts()? {
            let result = (|| -> Result<()> {
                if self
                    .store
                    .runtime(&account.id, "profile-bootstrap")?
                    .is_some()
                {
                    let state = self.state_unfenced(&account.id)?;
                    self.ensure_managed_profile(&account.id, &state)?;
                } else if self.store.native_profile_creation_receipt(&account.id)? {
                    if let Some(binding) = self.identity_binding(&account.id)? {
                        let identity = self.read_bound_identity(&account.id, &binding)?;
                        self.hydrate_bound_image_key(&account.id, &identity)?;
                        let (location, keys) = self.profile_location_keys(&account.id)?;
                        wechat_profile::ensure_managed_layout_version(
                            &location,
                            &keys,
                            &self.write_options(&account.id, "managed-layout-version")?,
                        )?;
                        self.ensure_account_sync_store(&account.id, &identity, &location)?;
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => self
                    .store
                    .clear_runtime(&account.id, "profile-recovery-pending")?,
                Err(_) => {
                    self.store
                        .put_runtime(&account.id, "profile-recovery-pending", b"1")?;
                    self.store.append_event_once(
                        &account.id,
                        "profile-recovery-pending",
                        "profile.recovery-pending",
                        &json!({"reason":"profile_recovery_requires_attention"}),
                    )?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn ensure_managed_profile(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<Value> {
        let record = self.store.account(account)?;
        let pending = self.store.runtime(account, "profile-bootstrap")?.is_some();
        if !pending && self.populated_profile(account)? {
            self.finish_official_profile_login(account, state)?;
            return Ok(json!({"state":"existing"}));
        }
        if record.storage_mode != "managed" {
            bail!("fresh_profile_requires_managed_storage");
        }
        let accepted_cipher = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let accepted_hash = format!("{:x}", Sha256::digest(&accepted_cipher));
        if self.store.runtime(account, "logged-out")?.is_some()
            || self
                .store
                .runtime(account, "saved-login-accepted-state-generation")?
                .as_deref()
                != Some(accepted_hash.as_bytes())
        {
            bail!("accepted_profile_credentials_required");
        }
        if state.database_password.len() != 32 {
            return Ok(json!({"state":"pending","reason":"login_database_password_required"}));
        }
        let key = self
            .profile_format_key
            .lock()
            .map_err(|_| anyhow::anyhow!("profile initializer unavailable"))?
            .clone();
        let Some(format_key) = key else {
            return Ok(json!({"state":"pending","reason":"profile_format_key_required"}));
        };
        let directory = wechat_profile::official_account_directory(&state.own_username, state.uin)?;
        self.store
            .put_runtime(account, "profile-bootstrap", b"initializing")?;
        let mut report = None;
        let (location, keys) = if !self.populated_profile(account)? {
            let (location, keys, value) = wechat_profile::initialize_official_home_with_login(
                &record.profile_path,
                &OfficialHomeBootstrap {
                    username: state.own_username.clone(),
                    uin: state.uin,
                    device_id: state.device.device_id.clone(),
                    global_values: vec![],
                },
                &state.database_password,
                &format_key,
                &SavedLoginProjection {
                    auto_auth_key: &state.auto_auth_key,
                    cookie: &state.cookie,
                    key_info: &state.key_info,
                    auto_login_flag: 3,
                    pc_account_name: None,
                    local_device_id64: None,
                },
            )?;
            report = Some(value);
            (location, keys)
        } else {
            // Resume a published bootstrap only after independently checking its
            // protected account identity. Never replace a partially present tree.
            let saved =
                wechat_profile::read_profile_saved_login(&record.profile_path, &format_key, true)?;
            if saved.identity.username != state.own_username
                || saved.identity.uin != state.uin
                || saved.identity.request_device_id()? != state.device.device_id
            {
                bail!("account_identity_mismatch");
            }
            let location =
                ProfileLocation::new(record.profile_path.join("xwechat_files").join(&directory))?;
            let keys = wechat_profile::keys_from_password(
                location.clone(),
                &state.database_password,
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )?;
            (location, keys)
        };
        let keys_path = self
            .store
            .root
            .join("accounts")
            .join(&record.id)
            .join("agent/profile-keys.json");
        if keys_path.exists() {
            let existing = KeyFile::load(&keys_path)?;
            wechat_profile::verify_profile_password(
                &location,
                &existing,
                &state.database_password,
            )?;
        } else {
            keys.write_new(&keys_path)?;
        }
        self.store.set_keys_file(account, &keys_path)?;
        // The initial empty-home lease did not contain the newly published
        // database subtree. Retain that subtree's lease before enabling writes.
        {
            let mut held = self
                .bootstrap_leases
                .lock()
                .map_err(|_| anyhow::anyhow!("profile ownership unavailable"))?;
            if !held.contains(account) {
                self.leases
                    .lock()
                    .map_err(|_| anyhow::anyhow!("profile ownership unavailable"))?
                    .entry(account.into())
                    .or_default()
                    .push(wechat_store::profile_lease(location.root())?);
                held.insert(account.into());
            }
        }
        let identity = record
            .profile_path
            .join("xwechat_files/all_users/config/global_config");
        self.bind_profile_identity(account, Path::new(&identity), &format_key)?;
        self.enable_profile_writes(account, &state.own_username, &identity, &format_key)?;
        self.store
            .put_runtime(account, "native-profile-origin", b"created-v1")?;
        self.store.clear_runtime(account, "profile-bootstrap")?;
        let _ = self.project_saved_login(account, state);
        Ok(
            json!({"state":"created","databases":keys.databases.len(),"encrypted":true,"savedLogin":true,"officialStartupVerified":false,"layout":report}),
        )
    }
    pub(super) fn ensure_account_sync_store(
        &self,
        account: &str,
        identity: &wechat_profile::ProfileIdentity,
        location: &ProfileLocation,
    ) -> Result<()> {
        let path = wechat_profile::account_sync_store_path(location, identity)?;
        if path.is_file() && path.with_extension("crc").is_file() {
            return Ok(());
        }
        if self.store.account(account)?.storage_mode != "managed"
            || !self.store.native_profile_creation_receipt(account)?
        {
            bail!("profile_sync_store_missing");
        }
        let purpose = "account-sync-store-create";
        let iv = match self.store.runtime(account, purpose)? {
            Some(sealed) => self.vault.open_record(account, purpose, &sealed)?.to_vec(),
            None => {
                let iv = wechat_protocol::crypto::random::<16>()?.to_vec();
                self.store.put_runtime(
                    account,
                    purpose,
                    &self.vault.seal(account, purpose, &iv)?,
                )?;
                iv
            }
        };
        let iv: [u8; 16] = iv
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid account store creation intent"))?;
        let parent = path.parent().context("invalid account store path")?;
        if !parent.exists() {
            std::fs::create_dir(parent)?;
        }
        let key = wechat_profile::account_mmkv_key(&identity.username)?;
        let pair = wechat_profile::create_mmkv_v4(&key, iv, 65536)?;
        wechat_profile::publish_mmkv_create(&path, &pair, &self.write_options(account, purpose)?)?;
        self.store.clear_runtime(account, purpose)?;
        Ok(())
    }
}
