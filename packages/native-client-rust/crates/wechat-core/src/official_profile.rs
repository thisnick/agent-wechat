//! Explicit in-place attachment of a stopped official Linux profile.
use super::profile_network::saved_business_hosts;
use super::saved_login::read_saved_file;
use super::*;
use std::{
    collections::BTreeSet,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use wechat_profile::{ProfileLocation, ProfileSavedLogin, ReadOptions};

const GLOBAL: &str = "xwechat_files/all_users/config/global_config";

#[derive(Clone, Default, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfficialProfileRequest {
    // An absolute path on the daemon host. Omission searches its HOME only.
    pub path: Option<String>,
    pub account_id: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct OfficialProfileSelection {
    pub account_id: String,
    pub profile_path: String,
    pub storage_mode: String,
    pub saved_device_login_available: bool,
}

/// Accept a full HOME, its xwechat_files directory, or a selected account root.
/// An account-root spelling must match the independently decoded global identity.
fn selected_home(path: &Path, explicit: bool) -> Result<(PathBuf, Option<PathBuf>)> {
    if !path.is_absolute() {
        bail!("official_profile_path_must_be_absolute");
    }
    let path =
        std::fs::canonicalize(path).map_err(|_| anyhow::anyhow!("official_profile_not_found"))?;
    let (home, selected) = if path.join("xwechat_files").is_dir() {
        (path.clone(), None)
    } else if path.file_name().is_some_and(|n| n == "xwechat_files") {
        (
            path.parent().context("official_profile_not_found")?.into(),
            None,
        )
    } else if path.join("db_storage").is_dir()
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "xwechat_files")
    {
        (
            path.parent()
                .and_then(Path::parent)
                .context("official_profile_not_found")?
                .into(),
            Some(path),
        )
    } else {
        bail!("official_profile_not_found");
    };
    let mut accounts = Vec::new();
    for (i, item) in std::fs::read_dir(home.join("xwechat_files"))?.enumerate() {
        if i >= 4096 {
            bail!("official_profile_inventory_limit");
        }
        let item = item?;
        if item.file_type()?.is_dir() && item.path().join("db_storage").is_dir() {
            accounts.push(item.path());
        }
    }
    if accounts.is_empty() {
        bail!("official_profile_not_found");
    }
    if !explicit && accounts.len() != 1 {
        bail!("official_profile_ambiguous");
    }
    Ok((home, selected))
}

// This supplements the ownership lease. The official app does not honor our lock;
// --official is the operator's exclusive-handoff instruction, not an OS lock on it.
fn reject_open_profile(home: &Path) -> Result<()> {
    fn files(path: &Path, depth: usize, ids: &mut BTreeSet<(u64, u64)>) -> Result<()> {
        if depth > 6 || ids.len() > 8192 {
            bail!("official_profile_inventory_limit");
        }
        let meta = std::fs::symlink_metadata(path)?;
        if meta.is_symlink() {
            bail!("invalid official profile path");
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(path)? {
                files(&entry?.path(), depth + 1, ids)?;
            }
        } else if meta.is_file() {
            ids.insert((meta.dev(), meta.ino()));
        }
        Ok(())
    }
    let mut ids = BTreeSet::new();
    for path in [home.join(GLOBAL), home.join(format!("{GLOBAL}.crc"))] {
        files(&path, 0, &mut ids)?;
    }
    for item in std::fs::read_dir(home.join("xwechat_files"))? {
        let db = item?.path().join("db_storage");
        if db.is_dir() {
            files(&db, 0, &mut ids)?;
        }
    }
    // Matching inodes also catches a container bind mount with another path spelling.
    for process in std::fs::read_dir("/proc")? {
        let process = process?;
        let name = process.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(meta) = fd.path().metadata() {
                if ids.contains(&(meta.dev(), meta.ino())) {
                    bail!("official_profile_in_use");
                }
            }
        }
    }
    Ok(())
}

impl Core {
    pub(super) fn official_login_options(&self, account: &str) -> Result<NativeOptions> {
        let mut options = NativeOptions::default();
        if self
            .store
            .runtime(account, "official-profile-selected")?
            .is_some()
        {
            if let Some((long, short)) =
                saved_business_hosts(&self.store.account(account)?.profile_path)
                    .context("profile_network_routing_unavailable")?
            {
                options.long_host = long;
                options.short_host = short;
            }
        }
        Ok(options)
    }
    pub async fn prepare_official_profile(
        self: &Arc<Self>,
        request: OfficialProfileRequest,
    ) -> Result<OfficialProfileSelection> {
        let _guard = self.official_profiles.lock().await;
        let core = self.clone();
        blocking(move || core.prepare_official_profile_inner(request)).await
    }
    fn prepare_official_profile_inner(
        &self,
        request: OfficialProfileRequest,
    ) -> Result<OfficialProfileSelection> {
        let explicit = request.path.is_some();
        let path = request
            .path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .context("official_profile_not_found")?;
        let (home, selected) = selected_home(&path, explicit)?;
        if home.starts_with(&self.store.root) {
            bail!("official_profile_overlaps_store");
        }
        reject_open_profile(&home)?;
        let format_key = self
            .profile_format_key
            .lock()
            .map_err(|_| anyhow::anyhow!("profile initializer unavailable"))?
            .clone()
            .context("profile_format_key_required")?;
        let identity = wechat_profile::read_mmkv_identity(
            &read_saved_file(&home.join(GLOBAL), &home, 4 * 1024 * 1024 + 4)?,
            &read_saved_file(&home.join(format!("{GLOBAL}.crc")), &home, 4096)?,
            &format_key,
        )?;
        let directory =
            wechat_profile::official_account_directory(&identity.username, identity.uin)?;
        let account_root = home.join("xwechat_files").join(&directory);
        if !account_root.join("db_storage").is_dir()
            || selected.as_ref().is_some_and(|p| p != &account_root)
        {
            bail!("account_identity_mismatch");
        }
        identity.request_device_id()?;
        let accounts = self.store.accounts()?;
        let matches = accounts
            .iter()
            .filter(|a| a.profile_path == home || a.profile_path == account_root)
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            bail!("official_profile_ambiguous");
        }
        let existing = matches.first().copied();
        let selected_id = request
            .account_id
            .as_deref()
            .map(|id| self.store.resolve_account(id))
            .transpose()?;
        if selected_id
            .as_ref()
            .is_some_and(|id| existing.is_none_or(|a| &a.id != id))
        {
            bail!("account_profile_mismatch");
        }
        let account = if let Some(account) = existing {
            if account.storage_mode != "official" || account.profile_path != home {
                bail!("account_profile_mismatch");
            }
            account.clone()
        } else {
            let id = format!(
                "official-{:x}",
                Sha256::digest(home.as_os_str().as_encoded_bytes())
            );
            let (account, lease) = self.store.attach_official_home(&id[..25], &home)?;
            self.leases
                .lock()
                .map_err(|_| anyhow::anyhow!("profile ownership unavailable"))?
                .push(lease);
            self.bootstrap_leases
                .lock()
                .map_err(|_| anyhow::anyhow!("profile ownership unavailable"))?
                .insert(account.id.clone());
            account
        };
        // Rejoining an already configured login must not need its mutation gate:
        // the original login holds that gate while awaiting phone approval.
        let configured = self
            .store
            .runtime(&account.id, "official-profile-selected")?
            .is_some()
            && self.identity_binding(&account.id)?.is_some();
        if configured {
            let bound = self.read_bound_identity(
                &account.id,
                &self
                    .identity_binding(&account.id)?
                    .context("profile_identity_unavailable")?,
            )?;
            if bound.username != identity.username
                || bound.uin != identity.uin
                || bound.device_id != identity.device_id
            {
                bail!("account_identity_mismatch");
            }
            return Ok(OfficialProfileSelection {
                account_id: account.id.clone(),
                profile_path: home.to_string_lossy().into_owned(),
                storage_mode: "official".into(),
                saved_device_login_available: self.official_saved_login(&account.id)?.is_some(),
            });
        }
        let gate = self.gate(&account.id)?;
        let _guard = gate
            .try_lock()
            .map_err(|_| anyhow::anyhow!("operation_in_progress"))?;
        self.bind_profile_identity(&account.id, &home.join(GLOBAL), &format_key)?;
        // No active authorization is imported, no network request is issued, and
        // no official bytes are changed by preparing the profile.
        self.store
            .put_runtime(&account.id, "official-profile-selected", b"v1")?;
        let saved = self.official_saved_login(&account.id)?.is_some();
        Ok(OfficialProfileSelection {
            account_id: account.id,
            profile_path: home.to_string_lossy().into_owned(),
            storage_mode: "official".into(),
            saved_device_login_available: saved,
        })
    }
    pub(super) fn official_saved_login(&self, account: &str) -> Result<Option<ProfileSavedLogin>> {
        if self
            .store
            .runtime(account, "official-profile-selected")?
            .is_none()
        {
            return Ok(None);
        }
        let binding = self
            .identity_binding(account)?
            .context("profile_identity_unavailable")?;
        let identity = self.read_bound_identity(account, &binding)?;
        // Missing/unsupported saved trust permits an explicit QR login. It never
        // permits bypassing the independently checked profile identity.
        let Ok(saved) = wechat_profile::read_profile_saved_login(
            self.store.account(account)?.profile_path,
            &binding.format_key,
            true,
        ) else {
            return Ok(None);
        };
        if saved.identity.username != identity.username
            || saved.identity.uin != identity.uin
            || saved.identity.device_id != identity.device_id
        {
            bail!("account_identity_mismatch");
        }
        if saved.logout_locked == Some(true) || saved.cookie.len() > 15 {
            return Ok(None);
        }
        Ok(Some(saved))
    }
    pub(super) fn official_login_key_info(&self, account: &str) -> Result<Vec<u8>> {
        if self
            .store
            .runtime(account, "official-profile-selected")?
            .is_none()
        {
            return Ok(Vec::new());
        }
        let binding = self
            .identity_binding(account)?
            .context("profile_identity_unavailable")?;
        let identity = self.read_bound_identity(account, &binding)?;
        let (key_info, _) = wechat_profile::read_profile_key_info(
            self.store.account(account)?.profile_path,
            &identity.username,
            true,
        )
        .context("profile_saved_key_info_unavailable")?;
        Ok(key_info.to_vec())
    }
    /// Derive local keys only from the accepted account password, after identity
    /// and device checks, and verify every copied encrypted database first.
    pub(super) fn prepare_official_profile_keys(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<()> {
        if self
            .store
            .runtime(account, "official-profile-selected")?
            .is_none()
            || self.store.keys_file(account)?.is_some()
        {
            return Ok(());
        }
        let binding = self
            .identity_binding(account)?
            .context("profile_identity_unavailable")?;
        let identity = self.read_bound_identity(account, &binding)?;
        if identity.username != state.own_username || identity.uin != state.uin {
            bail!("account_identity_mismatch");
        }
        if identity.request_device_id()? != state.device.device_id {
            bail!("account_device_mismatch");
        }
        if state.database_password.len() != 32 {
            bail!("profile_database_password_required");
        }
        let home = self.store.account(account)?.profile_path;
        let directory =
            wechat_profile::official_account_directory(&identity.username, identity.uin)?;
        let location = ProfileLocation::new(home.join("xwechat_files").join(directory))?;
        let mut keys = wechat_profile::keys_from_password(
            location,
            &state.database_password,
            ReadOptions {
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .map_err(super::identity::password_admission_error)?;
        keys.image_dat = Some(wechat_profile::ImageDatKey::derive(
            identity.uin,
            &identity.username,
        )?);
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
    pub(super) fn finish_official_profile_login(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<()> {
        if self
            .store
            .runtime(account, "official-profile-selected")?
            .is_none()
        {
            return Ok(());
        }
        let binding = self
            .identity_binding(account)?
            .context("profile_identity_unavailable")?;
        let home = self.store.account(account)?.profile_path;
        self.enable_profile_writes(
            account,
            &state.own_username,
            &home.join(&binding.relative),
            &binding.format_key,
        )?;
        let _ = self.project_saved_login(account, state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn home(path: &Path, username: &str) -> PathBuf {
        let directory = wechat_profile::official_account_directory(username, 1).unwrap();
        std::fs::create_dir_all(
            path.join("xwechat_files")
                .join(&directory)
                .join("db_storage"),
        )
        .unwrap();
        let pair = wechat_profile::create_mmkv_v4(&[9; 16], [3; 16], 4096).unwrap();
        let pair = wechat_profile::stage_mmkv_values(
            pair.data(),
            pair.metadata(),
            &[9; 16],
            &[
                (
                    "mmkv_key_user_name".into(),
                    Some(
                        wechat_profile::mmkv_string_value(username.as_bytes())
                            .unwrap()
                            .to_vec(),
                    ),
                ),
                ("mmkv_key_latest_login_uin".into(), Some(vec![1])),
                (
                    "key_wechat_device_id".into(),
                    Some(
                        wechat_profile::mmkv_string_value(&[vec![4; 16], vec![5; 17]].concat())
                            .unwrap()
                            .to_vec(),
                    ),
                ),
            ],
        )
        .unwrap();
        let global = path.join(GLOBAL);
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, pair.data()).unwrap();
        std::fs::write(global.with_extension("crc"), pair.metadata()).unwrap();
        path.join("xwechat_files").join(directory)
    }
    #[test]
    fn discovery_requires_a_single_profile_and_accepts_explicit_spellings() {
        let temp = tempfile::tempdir().unwrap();
        let account = home(temp.path(), "synthetic_self");
        assert_eq!(selected_home(temp.path(), false).unwrap().0, temp.path());
        for path in [
            temp.path().to_path_buf(),
            temp.path().join("xwechat_files"),
            account,
        ] {
            assert_eq!(selected_home(&path, true).unwrap().0, temp.path());
        }
        std::fs::create_dir_all(temp.path().join("xwechat_files/other/db_storage")).unwrap();
        assert_eq!(
            selected_home(temp.path(), false).unwrap_err().to_string(),
            "official_profile_ambiguous"
        );
        assert!(selected_home(Path::new("relative"), true).is_err());
        assert!(selected_home(&temp.path().join("missing"), true).is_err());
    }
    #[tokio::test]
    async fn accepted_identity_derives_existing_database_keys_without_replacing_files() {
        let temp = tempfile::tempdir().unwrap();
        let official = temp.path().join("home");
        let account_root = home(&official, "synthetic_self");
        // Use one real encrypted database to test password proof rather than a
        // mock key file; ordinary schema initialization is already covered.
        let database = account_root.join("db_storage/contact.db");
        let db = rusqlite::Connection::open(&database).unwrap();
        let password = [42u8; 32];
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
            0
        );
        db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;CREATE TABLE synthetic(value TEXT);INSERT INTO synthetic VALUES('preserve');").unwrap();
        drop(db);
        let before = wechat_store::copy::inventory(&official).unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let selected = core
            .prepare_official_profile(OfficialProfileRequest {
                path: Some(official.to_str().unwrap().into()),
                account_id: None,
            })
            .await
            .unwrap();
        let mut state = super::super::tests::state();
        state.device.device_id = vec![4; 16];
        state.database_password = vec![99; 32];
        assert_eq!(
            core.install_login_state(&selected.account_id, &state, None)
                .unwrap_err()
                .to_string(),
            "account_database_password_mismatch"
        );
        assert!(store
            .runtime(&selected.account_id, "account-state")
            .unwrap()
            .is_none());
        assert!(store.keys_file(&selected.account_id).unwrap().is_none());
        assert_eq!(before, wechat_store::copy::inventory(&official).unwrap());
        state.database_password = password.to_vec();
        core.install_login_state(&selected.account_id, &state, None)
            .unwrap();
        let (location, keys) = core.profile_location_keys(&selected.account_id).unwrap();
        assert_eq!(keys.databases.len(), 1);
        assert!(keys.image_dat.is_some());
        wechat_profile::verify_profile_password(&location, &keys, &password).unwrap();
        assert_eq!(
            core.account_state(&selected.account_id)
                .unwrap()
                .own_username,
            "synthetic_self"
        );
        assert_eq!(before, wechat_store::copy::inventory(&official).unwrap());
    }
    #[tokio::test]
    async fn selection_is_idempotent_exclusive_and_never_imports_active_authorization() {
        let temp = tempfile::tempdir().unwrap();
        let official = temp.path().join("home");
        let account_root = home(&official, "synthetic_self");
        let before = wechat_store::copy::inventory(&official.join("xwechat_files")).unwrap();
        // The normal native store may share HOME, never the official subtrees.
        let store = Arc::new(Store::open(&official.join(".local/share/agent-wechat")).unwrap());
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let first = core
            .prepare_official_profile(OfficialProfileRequest {
                path: Some(official.to_str().unwrap().into()),
                account_id: None,
            })
            .await
            .unwrap();
        assert_eq!(first.storage_mode, "official");
        assert!(!first.saved_device_login_available);
        assert!(store
            .runtime(&first.account_id, "account-state")
            .unwrap()
            .is_none());
        assert!(store.keys_file(&first.account_id).unwrap().is_none());
        assert!(store
            .runtime(&first.account_id, "profile-write-binding")
            .unwrap()
            .is_none());
        let active_login_guard = core.lock_account(&first.account_id).await.unwrap();
        let second = core
            .prepare_official_profile(OfficialProfileRequest {
                path: Some(account_root.to_str().unwrap().into()),
                account_id: Some(first.account_id.clone()),
            })
            .await
            .unwrap();
        drop(active_login_guard);
        assert_eq!(first.account_id, second.account_id);
        assert_eq!(store.accounts().unwrap().len(), 1);
        assert!(wechat_store::profile_lease(&account_root).is_err());
        assert_eq!(
            before,
            wechat_store::copy::inventory(&official.join("xwechat_files")).unwrap()
        );
        core.create_account("unrelated").unwrap();
        assert_eq!(
            core.prepare_official_profile(OfficialProfileRequest {
                path: Some(official.to_str().unwrap().into()),
                account_id: Some("unrelated".into())
            })
            .await
            .err()
            .unwrap()
            .to_string(),
            "account_profile_mismatch"
        );
        let mut wrong = super::super::tests::state();
        wrong.device.device_id = vec![4; 16];
        wrong.own_username = "another_account".into();
        assert_eq!(
            core.prepare_official_profile_keys(&first.account_id, &wrong)
                .unwrap_err()
                .to_string(),
            "account_identity_mismatch"
        );
        wrong.own_username = "synthetic_self".into();
        wrong.device.device_id = vec![5; 16];
        assert_eq!(
            core.prepare_official_profile_keys(&first.account_id, &wrong)
                .unwrap_err()
                .to_string(),
            "account_device_mismatch"
        );
        assert!(store.keys_file(&first.account_id).unwrap().is_none());
        drop(core);
        drop(store);
        let store =
            Arc::new(Store::open_existing(&official.join(".local/share/agent-wechat")).unwrap());
        let core = Core::new(store, None).unwrap();
        assert!(core.account_state(&first.account_id).is_err());
        assert_eq!(
            core.account_status(&first.account_id).unwrap()["authenticated"],
            false
        );
        assert!(wechat_store::profile_lease(&account_root).is_err());
    }
}
