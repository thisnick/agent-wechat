//! An existing operating profile determines the permitted native account.
use super::*;
use std::{io::Read, os::unix::fs::OpenOptionsExt, path::Path};

// Keep the typed cause: only a failed key proof is a password mismatch. A
// snapshot, filesystem, engine or schema error must not be reported as one.
pub(super) fn password_admission_error(error: wechat_profile::ProfileError) -> anyhow::Error {
    use wechat_profile::ProfileError;
    let code = match &error {
        ProfileError::KeyMismatch(_) => "account_database_password_mismatch",
        ProfileError::KeyUnavailable(_) | ProfileError::KeysMissing => "profile_keys_unavailable",
        ProfileError::DatabaseOpen(_) | ProfileError::Database => "profile_database_unavailable",
        ProfileError::SourceChanged => "profile_changed_during_login",
        ProfileError::ResourceLimit => "profile_resource_limit",
        ProfileError::SqlcipherUnavailable => "profile_cipher_unavailable",
        ProfileError::Io(_) => "profile_io_failed",
        _ => "profile_unavailable",
    };
    anyhow::Error::new(error).context(code)
}

impl Core {
    /// Local history keeps its account identity after network logout or forgetting.
    pub(crate) fn profile_username(&self, account: &str) -> Result<Option<String>> {
        if let Some(binding) = self.identity_binding(account)? {
            return Ok(Some(self.read_bound_identity(account, &binding)?.username));
        }
        Ok(self
            .state_unfenced(account)
            .ok()
            .map(|state| state.own_username.clone()))
    }
    pub(super) fn populated_profile(&self, account: &str) -> Result<bool> {
        Ok(
            std::fs::read_dir(self.store.account(account)?.profile_path)?
                .next()
                .transpose()?
                .is_some(),
        )
    }
    pub(super) fn identity_binding(&self, account: &str) -> Result<Option<IdentityBinding>> {
        for purpose in ["profile-identity-binding", "profile-write-binding"] {
            if let Some(encrypted) = self.store.runtime(account, purpose)? {
                return Ok(Some(serde_json::from_slice(
                    &self.vault.open_record(account, purpose, &encrypted)?,
                )?));
            }
        }
        Ok(None)
    }
    pub(super) fn read_bound_identity(
        &self,
        account: &str,
        binding: &IdentityBinding,
    ) -> Result<wechat_profile::ProfileIdentity> {
        let root = self.store.account(account)?.profile_path;
        let path = root.join(&binding.relative);
        if Path::new(&binding.relative).is_absolute() || std::fs::canonicalize(&path)? != path {
            bail!("invalid profile identity path");
        }
        let read = |p: &Path, max: u64| -> Result<Vec<u8>> {
            if !p.starts_with(&root) || std::fs::canonicalize(p)? != p {
                bail!("invalid profile identity path");
            }
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(p)?;
            if !f.metadata()?.is_file() || f.metadata()?.len() > max {
                bail!("invalid profile identity file");
            }
            let mut bytes = Vec::new();
            f.take(max + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max {
                bail!("invalid profile identity file");
            }
            Ok(bytes)
        };
        let metadata = path.with_file_name(format!(
            "{}.crc",
            path.file_name()
                .context("invalid identity filename")?
                .to_string_lossy()
        ));
        let identity = wechat_profile::read_mmkv_identity(
            &read(&path, 4 * 1024 * 1024 + 4)?,
            &read(&metadata, 4096)?,
            &binding.format_key,
        )?;
        // The directory alone is never identity proof. It must agree with the
        // independently decoded MMKV account, and later the authenticated keys.
        if let Some(keys) = self.store.keys_file(account)? {
            let keys = wechat_profile::KeyFile::load(keys)?;
            let expected =
                wechat_profile::official_account_directory(&identity.username, identity.uin)?;
            if keys.account_binding != expected && keys.account_binding != identity.username {
                bail!("account_identity_mismatch");
            }
        }
        Ok(identity)
    }
    pub fn bind_profile_identity(
        &self,
        account: &str,
        mmkv_store: &Path,
        format_key: &[u8],
    ) -> Result<()> {
        if format_key.len() != 16 {
            bail!("invalid profile format key");
        }
        let root = self.store.account(account)?.profile_path;
        let path = std::fs::canonicalize(mmkv_store)?;
        let relative = path
            .strip_prefix(&root)
            .context("identity source must be inside the selected official profile")?
            .to_str()
            .context("identity path must be UTF-8")?;
        let binding = IdentityBinding {
            relative: relative.into(),
            format_key: format_key.to_vec(),
        };
        let identity = self.read_bound_identity(account, &binding)?;
        if let Ok(state) = self.state_unfenced(account) {
            if identity.username != state.own_username || identity.uin != state.uin {
                bail!("account_identity_mismatch");
            }
        }
        self.store.put_runtime(
            account,
            "profile-identity-binding",
            &self.vault.seal(
                account,
                "profile-identity-binding",
                &serde_json::to_vec(&binding)?,
            )?,
        )?;
        self.hydrate_bound_image_key(account, &identity)?;
        self.password_proofs
            .lock()
            .map_err(|_| anyhow::anyhow!("identity proof unavailable"))?
            .remove(account);
        Ok(())
    }
    pub(super) fn hydrate_bound_image_key(
        &self,
        account: &str,
        identity: &wechat_profile::ProfileIdentity,
    ) -> Result<()> {
        let Some(path) = self.store.keys_file(account)? else {
            return Ok(());
        };
        let mut keys = wechat_profile::KeyFile::load(path)?;
        if keys.image_dat.is_none() {
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
        }
        Ok(())
    }
    pub(super) fn require_profile_identity(&self, account: &str) -> Result<()> {
        match self.identity_binding(account)? {
            Some(binding) => {
                self.read_bound_identity(account, &binding)?;
                Ok(())
            }
            None if !self.populated_profile(account)? => Ok(()),
            None => bail!("profile_identity_unavailable"),
        }
    }
    pub(super) fn check_state_profile(&self, account: &str, state: &AccountState) -> Result<()> {
        let Some(binding) = self.identity_binding(account)? else {
            if self.populated_profile(account)? {
                bail!("profile_identity_unavailable");
            }
            return Ok(());
        };
        let identity = self.read_bound_identity(account, &binding)?;
        if identity.username != state.own_username || identity.uin != state.uin {
            bail!("account_identity_mismatch");
        }
        if identity.request_device_id()? != state.device.device_id {
            bail!("account_device_mismatch");
        }
        if !state.database_password.is_empty() {
            let (location, keys) = self.profile_location_keys(account)?;
            let mut stamp = Sha256::new();
            stamp.update(&state.database_password);
            let key_path = self
                .store
                .keys_file(account)?
                .context("profile_keys_missing")?;
            let mut key_file = std::fs::File::open(key_path)?;
            let mut encoded = Vec::new();
            key_file
                .by_ref()
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut encoded)?;
            if encoded.len() > 4 * 1024 * 1024 {
                bail!("invalid keys file");
            }
            stamp.update(&encoded);
            use zeroize::Zeroize;
            encoded.zeroize();
            let mut actual = Vec::new();
            database_files(
                &location.root().join("db_storage"),
                &location.root().join("db_storage"),
                &mut actual,
                0,
            )?;
            actual.sort();
            for relative in &actual {
                let path = location.root().join("db_storage").join(relative);
                if std::fs::canonicalize(&path)? != path {
                    bail!("invalid database path");
                }
                let mut header = [0; 16];
                std::fs::File::open(path)?.read_exact(&mut header)?;
                stamp.update(relative.as_bytes());
                stamp.update(header);
            }
            let stamp = format!("{:x}", stamp.finalize());
            if self
                .password_proofs
                .lock()
                .map_err(|_| anyhow::anyhow!("identity proof unavailable"))?
                .get(account)
                != Some(&stamp)
            {
                wechat_profile::verify_profile_password(&location, &keys, &state.database_password)
                    .map_err(password_admission_error)?;
                self.password_proofs
                    .lock()
                    .map_err(|_| anyhow::anyhow!("identity proof unavailable"))?
                    .insert(account.into(), stamp);
            }
        }
        Ok(())
    }
}

fn database_files(root: &Path, path: &Path, output: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 16 || output.len() > 4096 {
        bail!("invalid database inventory");
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            bail!("invalid database path");
        }
        if kind.is_dir() {
            database_files(root, &path, output, depth + 1)?;
        } else if kind.is_file() && path.extension().is_some_and(|v| v == "db") {
            output.push(
                path.strip_prefix(root)?
                    .to_str()
                    .context("invalid database path")?
                    .to_owned(),
            );
        }
    }
    Ok(())
}
