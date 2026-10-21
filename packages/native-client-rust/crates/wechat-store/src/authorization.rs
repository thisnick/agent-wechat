//! Authorization installation and logout fencing share the sidecar transaction.
use super::*;
use rusqlite::OptionalExtension;

impl Store {
    pub fn mark_validated_if_active(&self, account: &str, generation: &str) -> Result<()> {
        self.control(account,|db|{
            db.execute("INSERT INTO runtime SELECT 'validated-generation',?1 WHERE NOT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out') AND EXISTS(SELECT 1 FROM runtime WHERE key='desired' AND value=CAST('connected' AS BLOB)) ON CONFLICT(key)DO UPDATE SET value=excluded.value",[generation.as_bytes()])?;Ok(())
        })
    }
    pub fn login_operation_by_key(
        &self,
        account: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<Operation>> {
        let mapped = self.control(account, |db| {
            let value: Option<Vec<u8>> = db
                .query_row(
                    "SELECT value FROM runtime WHERE key=?1",
                    [format!("login-key:{:x}", Sha256::digest(key.as_bytes()))],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(value) = value else {
                return Ok(None);
            };
            let (id, hash): (String, String) = serde_json::from_slice(&value)?;
            if hash != request_hash {
                bail!("idempotency_conflict");
            }
            Ok(Some(id))
        })?;
        mapped.map(|id| self.operation(account, &id)).transpose()
    }
    pub fn save_login_operation(
        &self,
        op: &Operation,
        key: &str,
        request_hash: &str,
    ) -> Result<()> {
        if key.is_empty() || key.len() > 200 {
            bail!("invalid login idempotency key");
        }
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO runtime VALUES(?1,?2)",
                params![
                    format!("login-key:{:x}", Sha256::digest(key.as_bytes())),
                    serde_json::to_vec(&(op.id.clone(), request_hash))?
                ],
            )?;
            tx.execute(
                "INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    op.id,
                    op.kind,
                    op.state,
                    serde_json::to_string(&op.result)?,
                    op.created_at,
                    op.updated_at
                ],
            )?;
            tx.execute(
                "INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)",
                params![
                    serde_json::to_string(
                        &serde_json::json!({"id":op.id,"state":op.state,"kind":op.kind})
                    )?,
                    op.updated_at
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn save_active_authorization(&self, account: &str, encrypted: &[u8]) -> Result<()> {
        self.save_authorization(account, encrypted, false)
    }
    /// The caller has verified a native server response for this exact state.
    pub fn save_accepted_authorization(&self, account: &str, encrypted: &[u8]) -> Result<()> {
        self.save_authorization(account, encrypted, true)
    }
    /// A persistent connection may complete after disconnect/logout or after a
    /// newer login. Publish its cookie update only for the admitted generation.
    pub fn save_push_authorization(
        &self,
        account: &str,
        encrypted: &[u8],
        expected: &[u8],
        epoch: Option<&[u8]>,
    ) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let connected: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='desired' AND value=CAST('connected' AS BLOB))", [], |r| r.get(0))?;
            if !connected || !saved_login_generation(&tx, expected, epoch)? {
                bail!("push_session_superseded");
            }
            tx.execute("UPDATE runtime SET value=?1 WHERE key='account-state'", [encrypted])?;
            accepted_projection_needed(&tx, encrypted)?;
            tx.commit()?;
            Ok(())
        })
    }
    fn save_authorization(&self, account: &str, encrypted: &[u8], accepted: bool) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let fenced: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')", [], |r| r.get(0))?;
            if fenced { bail!("auth_required"); }
            tx.execute("INSERT INTO runtime VALUES('account-state',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [encrypted])?;
            if accepted { accepted_projection_needed(&tx, encrypted)?; }
            tx.commit()?;
            Ok(())
        })
    }
    /// Recheck the admitted state and logout generation in one read snapshot
    /// immediately before publishing an official login file.
    pub fn saved_login_authorization_current(
        &self,
        account: &str,
        expected_state_cipher: &[u8],
        epoch: Option<&[u8]>,
    ) -> Result<bool> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            saved_login_generation(&tx, expected_state_cipher, epoch)
        })
    }
    /// Stage an encrypted official-login publication under the admitted native
    /// credential and logout generation. Superseded journals remain protected.
    pub fn stage_saved_login_publication(
        &self,
        account: &str,
        expected_state_cipher: &[u8],
        epoch: Option<&[u8]>,
        expected_old_journal: Option<&[u8]>,
        new_journal: &[u8],
    ) -> Result<bool> {
        if expected_state_cipher.is_empty() || new_journal.is_empty() {
            bail!("invalid saved login publication");
        }
        self.control(account, |db| {
            let tx=db.transaction()?;
            if !saved_login_generation(&tx,expected_state_cipher,epoch)? {return Ok(false);}
            let existing:Option<Vec<u8>>=tx.query_row("SELECT value FROM runtime WHERE key='saved-login-publication'",[],|r|r.get(0)).optional()?;
            if existing.as_deref()!=expected_old_journal {return Ok(false);}
            if let Some(old)=existing {
                tx.execute("INSERT OR IGNORE INTO runtime VALUES(?1,?2)",params![format!("saved-login-superseded:{:x}",Sha256::digest(&old)),old])?;
            }
            tx.execute("INSERT INTO runtime VALUES('saved-login-publication',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value",[new_journal])?;
            tx.execute("INSERT INTO runtime VALUES('saved-login-projection-needed',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value",[b"1".as_slice()])?;
            tx.commit()?;Ok(true)
        })
    }
    pub fn finish_saved_login_publication(
        &self,
        account: &str,
        expected_journal: Option<&[u8]>,
        expected_state_cipher: &[u8],
        epoch: Option<&[u8]>,
        credential_generation: &str,
    ) -> Result<()> {
        if credential_generation.len() != 64
            || !credential_generation.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid saved login publication");
        }
        self.control(account,|db|{
            let tx=db.transaction()?;
            if !saved_login_generation(&tx,expected_state_cipher,epoch)? {bail!("authorization_generation_changed");}
            let existing:Option<Vec<u8>>=tx.query_row("SELECT value FROM runtime WHERE key='saved-login-publication'",[],|r|r.get(0)).optional()?;
            let prior:Option<Vec<u8>>=tx.query_row("SELECT value FROM runtime WHERE key='saved-login-projected-generation'",[],|r|r.get(0)).optional()?;
            if existing.as_deref()!=expected_journal || (expected_journal.is_none()&&prior.as_deref()!=Some(credential_generation.as_bytes())) {bail!("saved login publication generation changed");}
            let generation=format!("{:x}",Sha256::digest(expected_state_cipher));
            for(key,value)in[("saved-login-projected-generation",credential_generation.as_bytes()),("saved-login-projected-state-generation",generation.as_bytes())] {
                tx.execute("INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![key,value])?;
            }
            tx.execute("DELETE FROM runtime WHERE key IN ('saved-login-publication','saved-login-projection-needed')",[])?;
            if prior.as_deref()!=Some(credential_generation.as_bytes()) {
                tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('credentials.projected',?1,?2)",params![serde_json::to_string(&serde_json::json!({"officialSavedLogin":"complete"}))?,chrono::Utc::now().to_rfc3339()])?;
                let seq=tx.last_insert_rowid();tx.execute("DELETE FROM events WHERE sequence<=?1",[seq.saturating_sub(10_000)])?;
            }
            tx.commit()?;Ok(())
        })
    }

    /// Only a login begun after the current logout epoch can clear its fence.
    pub fn commit_login_authorization(
        &self,
        account: &str,
        encrypted: &[u8],
        expected_logout_epoch: Option<&[u8]>,
        ownership_generation: &str,
    ) -> Result<()> {
        self.commit_login_authorization_for_operation(
            account,
            encrypted,
            expected_logout_epoch,
            ownership_generation,
            None,
        )
    }
    pub fn commit_login_authorization_for_operation(
        &self,
        account: &str,
        encrypted: &[u8],
        expected_logout_epoch: Option<&[u8]>,
        ownership_generation: &str,
        operation: Option<&str>,
    ) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let epoch: Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key='logout-epoch'", [], |r| r.get(0)).optional()?;
            if epoch.as_deref() != expected_logout_epoch { bail!("authorization_generation_changed"); }
            if let Some(id)=operation {
                let state:String=tx.query_row("SELECT state FROM operations WHERE id=?1",[id],|r|r.get(0))?;
                if matches!(state.as_str(),"cancelled"|"expired"|"failed"|"rejected"){bail!("authorization_generation_changed");}
            }
            for (key, value) in [("account-state", encrypted), ("validated-generation", ownership_generation.as_bytes()), ("desired", b"connected".as_slice())] {
                tx.execute("INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
            }
            tx.execute("DELETE FROM runtime WHERE key='logged-out'", [])?;
            accepted_projection_needed(&tx, encrypted)?;
            tx.commit()?;
            Ok(())
        })
    }
}

fn accepted_projection_needed(tx: &rusqlite::Transaction<'_>, encrypted: &[u8]) -> Result<()> {
    let generation = format!("{:x}", Sha256::digest(encrypted));
    for (key, value) in [
        ("saved-login-projection-needed", b"1".as_slice()),
        (
            "saved-login-accepted-state-generation",
            generation.as_bytes(),
        ),
    ] {
        tx.execute(
            "INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key)DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
    }
    Ok(())
}
fn saved_login_generation(
    tx: &rusqlite::Transaction<'_>,
    expected: &[u8],
    epoch: Option<&[u8]>,
) -> Result<bool> {
    let fenced: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')",
        [],
        |r| r.get(0),
    )?;
    let current: Option<Vec<u8>> = tx
        .query_row(
            "SELECT value FROM runtime WHERE key='account-state'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let current_epoch: Option<Vec<u8>> = tx
        .query_row(
            "SELECT value FROM runtime WHERE key='logout-epoch'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let accepted: Option<Vec<u8>> = tx
        .query_row(
            "SELECT value FROM runtime WHERE key='saved-login-accepted-state-generation'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let generation = format!("{:x}", Sha256::digest(expected));
    Ok(!fenced
        && current.as_deref() == Some(expected)
        && current_epoch.as_deref() == epoch
        && accepted.as_deref() == Some(generation.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_connection_updates_require_current_accepted_connected_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .save_active_authorization("one", b"unverified")
            .unwrap();
        assert!(store
            .save_push_authorization("one", b"late", b"unverified", None)
            .is_err());
        store
            .save_accepted_authorization("one", b"accepted")
            .unwrap();
        store
            .save_push_authorization("one", b"cookie-update", b"accepted", None)
            .unwrap();
        assert!(store
            .saved_login_authorization_current("one", b"cookie-update", None)
            .unwrap());
        assert!(store
            .save_push_authorization("one", b"old-socket", b"accepted", None)
            .is_err());
        store
            .put_runtime("one", "desired", b"disconnected")
            .unwrap();
        assert!(store
            .save_push_authorization("one", b"after-disconnect", b"cookie-update", None)
            .is_err());
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .put_runtime("one", "logout-epoch", b"new-epoch")
            .unwrap();
        assert!(store
            .save_push_authorization("one", b"after-logout", b"cookie-update", None)
            .is_err());
        store.put_runtime("one", "logged-out", b"1").unwrap();
        assert!(store
            .save_push_authorization("one", b"after-logout", b"cookie-update", Some(b"new-epoch"))
            .is_err());
        assert_eq!(
            store.runtime("one", "account-state").unwrap().unwrap(),
            b"cookie-update"
        );
        assert!(store.runtime("two", "account-state").unwrap().is_none());
    }
    #[test]
    fn late_responses_and_old_logins_cannot_cross_logout() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("test").unwrap();
        store
            .save_active_authorization("test", b"old-encrypted-state")
            .unwrap();
        store.put_runtime("test", "logged-out", b"1").unwrap();
        store
            .put_runtime("test", "logout-epoch", b"epoch-1")
            .unwrap();
        assert!(store
            .save_active_authorization("test", b"late-response")
            .is_err());
        assert!(store
            .commit_login_authorization("test", b"late-login", None, "process")
            .is_err());
        assert_eq!(
            store.runtime("test", "account-state").unwrap().unwrap(),
            b"old-encrypted-state"
        );
        assert!(store
            .runtime("test", "validated-generation")
            .unwrap()
            .is_none());
        store
            .commit_login_authorization("test", b"approved-new-state", Some(b"epoch-1"), "process")
            .unwrap();
        assert!(store.runtime("test", "logged-out").unwrap().is_none());
        assert_eq!(
            store.runtime("test", "account-state").unwrap().unwrap(),
            b"approved-new-state"
        );
        assert_eq!(
            store.runtime("test", "desired").unwrap().unwrap(),
            b"connected"
        );
    }
    #[test]
    fn saved_login_publication_requires_accepted_state_epoch_and_atomic_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        let generation = "a".repeat(64);
        store
            .save_active_authorization("one", b"unvalidated")
            .unwrap();
        assert!(!store
            .saved_login_authorization_current("one", b"unvalidated", None)
            .unwrap());
        assert!(!store
            .stage_saved_login_publication("one", b"unvalidated", None, None, b"journal")
            .unwrap());
        store
            .save_accepted_authorization("one", b"accepted1")
            .unwrap();
        assert!(store
            .saved_login_authorization_current("one", b"accepted1", None)
            .unwrap());
        assert!(!store
            .saved_login_authorization_current("one", b"accepted1", Some(b"wrong-epoch"))
            .unwrap());
        assert!(store
            .runtime("one", "saved-login-projection-needed")
            .unwrap()
            .is_some());
        assert!(!store
            .stage_saved_login_publication(
                "one",
                b"accepted1",
                Some(b"wrong-epoch"),
                None,
                b"journal1"
            )
            .unwrap());
        assert!(store
            .stage_saved_login_publication("one", b"accepted1", None, None, b"journal1")
            .unwrap());
        store.control("one",|db|{db.execute_batch("CREATE TRIGGER reject_saved_login BEFORE INSERT ON events WHEN NEW.kind='credentials.projected' BEGIN SELECT RAISE(ABORT,'event unavailable');END;")?;Ok(())}).unwrap();
        assert!(store
            .finish_saved_login_publication(
                "one",
                Some(b"journal1"),
                b"accepted1",
                None,
                &generation
            )
            .is_err());
        assert!(store
            .runtime("one", "saved-login-publication")
            .unwrap()
            .is_some());
        assert!(store
            .runtime("one", "saved-login-projected-generation")
            .unwrap()
            .is_none());
        store
            .control("one", |db| {
                db.execute_batch("DROP TRIGGER reject_saved_login;")?;
                Ok(())
            })
            .unwrap();
        store
            .finish_saved_login_publication(
                "one",
                Some(b"journal1"),
                b"accepted1",
                None,
                &generation,
            )
            .unwrap();
        store
            .save_accepted_authorization("one", b"accepted-new-nonce")
            .unwrap();
        store
            .finish_saved_login_publication("one", None, b"accepted-new-nonce", None, &generation)
            .unwrap();
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "credentials.projected")
                .count(),
            1
        );
        assert!(store
            .stage_saved_login_publication("one", b"accepted-new-nonce", None, None, b"journal2")
            .unwrap());
        store
            .save_active_authorization("one", b"unvalidated-replacement")
            .unwrap();
        assert!(!store
            .stage_saved_login_publication(
                "one",
                b"unvalidated-replacement",
                None,
                Some(b"journal2"),
                b"unvalidated-journal"
            )
            .unwrap());
        store
            .save_accepted_authorization("one", b"accepted2")
            .unwrap();
        assert!(!store
            .saved_login_authorization_current("one", b"accepted-new-nonce", None)
            .unwrap());
        assert!(store
            .stage_saved_login_publication(
                "one",
                b"accepted2",
                None,
                Some(b"journal2"),
                b"journal3"
            )
            .unwrap());
        assert!(store
            .runtime(
                "one",
                &format!("saved-login-superseded:{:x}", Sha256::digest(b"journal2"))
            )
            .unwrap()
            .is_some());
        assert!(store
            .finish_saved_login_publication(
                "one",
                Some(b"journal2"),
                b"accepted-new-nonce",
                None,
                &generation
            )
            .is_err());
        store.put_runtime("one", "logged-out", b"fence").unwrap();
        assert!(!store
            .saved_login_authorization_current("one", b"accepted2", None)
            .unwrap());
        assert!(!store
            .stage_saved_login_publication("one", b"accepted2", None, Some(b"journal3"), b"late")
            .unwrap());
        assert!(store
            .finish_saved_login_publication(
                "one",
                Some(b"journal3"),
                b"accepted2",
                None,
                &generation
            )
            .is_err());
    }
    #[test]
    fn cancelled_login_cannot_install_and_idempotency_is_account_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let mut op = Operation {
            id: "login-test".into(),
            account_id: "one".into(),
            kind: "login".into(),
            state: "authenticating".into(),
            result: serde_json::json!({}),
            created_at: "2026-10-20T00:00:00Z".into(),
            updated_at: "2026-10-20T00:00:00Z".into(),
        };
        store
            .save_login_operation(&op, "retry-key", "request-hash")
            .unwrap();
        assert_eq!(
            store
                .login_operation_by_key("one", "retry-key", "request-hash")
                .unwrap()
                .unwrap()
                .id,
            op.id
        );
        assert!(store
            .login_operation_by_key("one", "retry-key", "different-request")
            .is_err());
        assert!(store
            .login_operation_by_key("two", "retry-key", "request-hash")
            .unwrap()
            .is_none());
        op.state = "cancelled".into();
        store.save_operation(&op).unwrap();
        assert!(store
            .commit_login_authorization_for_operation(
                "one",
                b"late-response",
                None,
                "process",
                Some(&op.id)
            )
            .is_err());
        assert!(store.runtime("one", "account-state").unwrap().is_none());
    }
}
