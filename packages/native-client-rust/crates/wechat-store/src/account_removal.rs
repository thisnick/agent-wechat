//! Unregister profiles without deleting their official files or operational receipts.
use super::*;

impl Store {
    pub fn remove_account(&self, account: &str) -> Result<()> {
        // All account sidecar writes also hold this lock and recheck registration.
        let mut registry = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        Self::require_registered(&registry, account)?;
        let path = self.account_dir(account)?.join("agent/control.sqlite");
        if !fs::symlink_metadata(&path)?.is_file() || !wechat_platform::private_path(&path)? {
            bail!("invalid account sidecar permissions");
        }
        let mut admitted = self
            .admitted
            .lock()
            .map_err(|_| anyhow::anyhow!("database admission lock poisoned"))?;
        let (control, ()) =
            admission::open_prepared(&path, Some(&admitted), Self::prepare_account)?;
        let signed_in: bool = control.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='account-state') AND NOT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')", [], |r| r.get(0))?;
        if signed_in {
            bail!("account_sign_out_required");
        }
        let pending: bool = control.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE state NOT IN ('succeeded','failed','cancelled','rejected','expired','needs_login','needs_review')) OR EXISTS(SELECT 1 FROM runtime WHERE key='logout-pending') OR EXISTS(SELECT 1 FROM media_jobs WHERE state IN ('queued','running')) OR EXISTS(SELECT 1 FROM outbox WHERE state IN ('queued','submitting')) OR EXISTS(SELECT 1 FROM call_sessions WHERE state!='ended')", [], |r| r.get(0))?;
        if pending {
            bail!("operation_in_progress");
        }
        control.close().map_err(|(_, error)| error)?;
        admission::remember(&mut admitted, &path)?;
        let tx = registry.transaction()?;
        // Retain profile paths, key references and IDs for recovery and prevent reuse
        // by an old account-scoped token. Only its display alias becomes available.
        tx.execute(
            "UPDATE accounts SET removed_at=?2 WHERE id=?1 AND removed_at IS NULL",
            params![account, operations::unix_seconds()],
        )?;
        tx.execute(
            "UPDATE account_settings SET alias=NULL WHERE account_id=?1",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unresolved_call_prevents_unregistering_its_recovery_records() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.begin_call("a", "call", "generation").unwrap();
        store.put_runtime("a", "logged-out", b"1").unwrap();
        assert_eq!(
            store.remove_account("a").unwrap_err().to_string(),
            "operation_in_progress"
        );
        store.finish_call("a", "call", false).unwrap();
        assert!(store.remove_account("a").is_err());
        store.finish_call("a", "call", true).unwrap();
        store.remove_account("a").unwrap();
    }
    #[test]
    fn removal_preserves_files_reserves_identity_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        let account = store
            .create_account_with_alias("one", Some("Personal"))
            .unwrap();
        store.create_account("two").unwrap();
        let unknown = account.profile_path.join("unknown-file");
        fs::write(&unknown, b"retain these bytes").unwrap();
        store
            .put_runtime("one", "account-state", b"saved credentials")
            .unwrap();
        assert_eq!(
            store.remove_account("one").unwrap_err().to_string(),
            "account_sign_out_required"
        );
        store.put_runtime("one", "logged-out", b"1").unwrap();
        store.remove_account("one").unwrap();
        assert!(store.account("one").is_err());
        assert!(store.resolve_account("Personal").is_err());
        assert!(store.put_runtime("one", "desired", b"connected").is_err());
        assert_eq!(fs::read(&unknown).unwrap(), b"retain these bytes");
        assert!(root.join("accounts/one/agent/control.sqlite").is_file());
        assert!(store.create_account("one").is_err());
        store.set_account_alias("two", "Personal").unwrap();
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        assert_eq!(store.accounts().unwrap().len(), 1);
        assert_eq!(store.resolve_account("Personal").unwrap(), "two");
        assert_eq!(fs::read(&unknown).unwrap(), b"retain these bytes");
    }
    #[test]
    fn removing_a_direct_profile_keeps_external_files_and_key_reference() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("official-profile");
        private_dir(&source).unwrap();
        fs::write(source.join("unknown.official"), b"official bytes").unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.attach("direct", &source, true).unwrap();
        let keys = temp.path().join("keys.json");
        wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&keys)
            .unwrap();
        store.set_keys_file("direct", &keys).unwrap();
        store.remove_account("direct").unwrap();
        assert_eq!(
            fs::read(source.join("unknown.official")).unwrap(),
            b"official bytes"
        );
        assert!(keys.is_file());
        assert!(store.set_account_alias("direct", "renamed").is_err());
        assert!(store.set_keys_file("direct", &keys).is_err());
        let registry = store.db.lock().unwrap();
        let retained: String = registry
            .query_row(
                "SELECT keys_file FROM account_settings WHERE account_id='direct'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(retained), fs::canonicalize(keys).unwrap());
    }
    #[test]
    fn pending_operations_block_removal() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        store
            .control("one", |db| {
                db.execute(
                    "INSERT INTO operations VALUES('pending','login','awaiting_scan','{}','0','0')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.remove_account("one").unwrap_err().to_string(),
            "operation_in_progress"
        );
        assert!(store.account("one").is_ok());
        store
            .control("one", |db| {
                db.execute("UPDATE operations SET state='cancelled'", [])?;
                Ok(())
            })
            .unwrap();
        store.remove_account("one").unwrap();
    }
}
