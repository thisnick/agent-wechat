//! Registry settings are separate from the official operating profile.
use super::*;
impl Store {
    pub fn resolve_account(&self, value: &str) -> Result<String> {
        if self.accounts()?.iter().any(|a| a.id == value) {
            return Ok(value.into());
        }
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        db.query_row(
            "SELECT account_id FROM account_settings WHERE alias=?1",
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
        let tx = db.transaction()?;
        use rusqlite::OptionalExtension;
        let existing: Option<String> = tx
            .query_row(
                "SELECT account_id FROM account_settings WHERE alias=?1",
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
