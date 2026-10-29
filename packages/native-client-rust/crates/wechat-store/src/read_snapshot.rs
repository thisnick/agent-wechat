//! Request-owned read connections. These never migrate a schema or create a store.
use crate::*;
use rusqlite::OpenFlags;

fn pin(path: &Path) -> Result<Connection> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && wechat_platform::private_path(path)?,
        "invalid read snapshot path"
    );
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")?;
    // BEGIN alone does not establish a WAL snapshot.
    db.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(db)
}

impl Store {
    /// Caller coordinates this admission with profile writes. Registry and account
    /// read transactions survive until this request-owned Store is dropped.
    pub fn read_snapshot(self: &Arc<Self>, account: &str) -> Result<Self> {
        // Only our admitted operational stores change journal mode. Official
        // profile modes are never changed by the reader.
        self.db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?
            .pragma_update(None, "journal_mode", "WAL")?;
        let control = self.control(account, |db| {
            db.pragma_update(None, "journal_mode", "WAL")?;
            pin(&self.account_dir(account)?.join("agent/control.sqlite"))
        })?;
        let db = pin(&self.root.join("registry.sqlite"))?;
        Self::require_registered(&db, account)?;
        Ok(Self {
            root: self.root.clone(),
            server_id: self.server_id.clone(),
            db: Mutex::new(db),
            admitted: Mutex::new(Default::default()),
            _lease: self._lease.clone(),
            snapshot: Some((account.into(), Mutex::new(control))),
        })
    }

    pub fn is_read_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readers_retain_their_boundary_without_blocking_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&tmp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.put_runtime("one", "value", b"old").unwrap();
        let a = store.read_snapshot("one").unwrap();
        let b = store.read_snapshot("one").unwrap();
        store.put_runtime("one", "value", b"new").unwrap();
        store
            .append_event("one", "test", &serde_json::json!({}))
            .unwrap();
        assert_eq!(
            a.runtime("one", "value").unwrap().as_deref(),
            Some(b"old".as_slice())
        );
        assert_eq!(b.event_boundary("one").unwrap(), "0");
        assert!(a.put_runtime("one", "value", b"bad").is_err());
        assert!(a.runtime("other", "value").is_err());
        assert_eq!(store.event_boundary("one").unwrap(), "1");
    }
}
