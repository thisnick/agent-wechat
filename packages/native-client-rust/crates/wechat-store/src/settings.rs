//! Registry settings are separate from the official operating profile.
use super::*;
/// Hub policy; this is never written into an official WeChat profile.
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountSettings {
    #[ts(type = "number")]
    pub auto_download_max_bytes: u64,
}
impl Default for AccountSettings {
    fn default() -> Self {
        Self {
            auto_download_max_bytes: 50 * 1024 * 1024,
        }
    }
}
impl Store {
    pub fn account_settings(&self, account: &str) -> Result<AccountSettings> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        Self::require_registered(&db, account)?;
        use rusqlite::OptionalExtension;
        let limit = db
            .query_row(
                "SELECT auto_download_max_bytes FROM account_settings WHERE account_id=?1",
                [account],
                |r| r.get::<_, u64>(0),
            )
            .optional()?;
        Ok(AccountSettings {
            auto_download_max_bytes: limit
                .unwrap_or_else(|| AccountSettings::default().auto_download_max_bytes),
        })
    }
    pub fn set_account_settings(&self, account: &str, settings: &AccountSettings) -> Result<()> {
        if settings.auto_download_max_bytes > wechat_media::MAX_FILE_BYTES as u64 {
            bail!("invalid_auto_download_limit");
        }
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        Self::require_registered(&db, account)?;
        db.execute("INSERT INTO account_settings(account_id,auto_download_max_bytes) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET auto_download_max_bytes=excluded.auto_download_max_bytes",params![account,settings.auto_download_max_bytes])?;
        Ok(())
    }

    pub fn resolve_account(&self, value: &str) -> Result<String> {
        if self.accounts()?.iter().any(|a| a.id == value) {
            return Ok(value.into());
        }
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        db.query_row(
            "SELECT s.account_id FROM account_settings s JOIN accounts a ON a.id=s.account_id WHERE s.alias=?1 AND a.removed_at IS NULL",
            [value],
            |r| r.get(0),
        )
        .context("account not found")
    }
    pub fn account_alias(&self, account: &str) -> Result<Option<String>> {
        self.account(account)?;
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        use rusqlite::OptionalExtension;
        Ok(db
            .query_row(
                "SELECT alias FROM account_settings WHERE account_id=?1",
                [account],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn set_account_alias(&self, account: &str, alias: &str) -> Result<()> {
        self.account(account)?;
        if alias.is_empty()
            || alias.len() > 64
            || !alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("invalid account alias");
        }
        if alias != account && self.accounts()?.iter().any(|a| a.id == alias) {
            bail!("account_alias_conflict");
        }
        let mut db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        Self::require_registered(&db, account)?;
        let tx = db.transaction()?;
        use rusqlite::OptionalExtension;
        let existing: Option<String> = tx
            .query_row(
                "SELECT s.account_id FROM account_settings s JOIN accounts a ON a.id=s.account_id WHERE s.alias=?1 AND a.removed_at IS NULL",
                [alias],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_some_and(|id| id != account) {
            bail!("account_alias_conflict");
        }
        tx.execute("INSERT INTO account_settings(account_id,alias)VALUES(?1,?2) ON CONFLICT(account_id)DO UPDATE SET alias=excluded.alias",params![account,alias])?;
        tx.commit()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_settings_are_per_account_and_survive_reopening() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        let store = Store::open(&path).unwrap();
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        assert_eq!(
            store.account_settings("a").unwrap().auto_download_max_bytes,
            52_428_800
        );
        store.set_account_alias("a", "personal").unwrap();
        store
            .set_account_settings(
                "a",
                &AccountSettings {
                    auto_download_max_bytes: 0,
                },
            )
            .unwrap();
        assert_eq!(
            store.account_settings("b").unwrap().auto_download_max_bytes,
            52_428_800
        );
        assert_eq!(
            store.account_alias("a").unwrap().as_deref(),
            Some("personal")
        );
        assert!(store
            .set_account_settings(
                "a",
                &AccountSettings {
                    auto_download_max_bytes: 536_870_913
                }
            )
            .is_err());
        assert!(store
            .set_account_settings("absent", &AccountSettings::default())
            .is_err());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.account_settings("a").unwrap().auto_download_max_bytes,
            0
        );
        assert_eq!(
            store.account_settings("b").unwrap().auto_download_max_bytes,
            52_428_800
        );
    }
    #[test]
    fn upgrading_registry_settings_keeps_existing_accounts_and_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        let store = Store::open(&path).unwrap();
        store.create_account("a").unwrap();
        store.set_account_alias("a", "personal").unwrap();
        store.db.lock().unwrap().execute_batch("ALTER TABLE account_settings DROP COLUMN auto_download_max_bytes; PRAGMA user_version=3;").unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.resolve_account("personal").unwrap(), "a");
        assert_eq!(
            store.account_settings("a").unwrap().auto_download_max_bytes,
            52_428_800
        );
    }
    #[test]
    fn aliases_are_unique_and_cannot_shadow_ids() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        store.set_account_alias("a", "personal").unwrap();
        assert_eq!(store.resolve_account("personal").unwrap(), "a");
        assert!(store.set_account_alias("b", "personal").is_err());
        assert!(store.set_account_alias("a", "b").is_err());
        store.set_account_alias("a", "a").unwrap();
        assert!(store.resolve_account("personal").is_err());
    }
}
