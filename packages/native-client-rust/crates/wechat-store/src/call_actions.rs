//! Call operation receipts, separate from canonical chat/call summary state.
use super::*;
use rusqlite::OptionalExtension;
fn id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}
/// Encrypted operational evidence only; reading it never permits submission.
pub struct CallActionRecord {
    pub action_id: String,
    pub route: u16,
    pub request_digest: String,
    pub outcome: String,
    pub encrypted_request: Option<Vec<u8>>,
    pub encrypted_reply: Option<Vec<u8>>,
}
impl Store {
    /// Reserve a session once. Existing unresolved sessions stop new calls after
    /// a crash; this journal never retries an invitation automatically.
    pub fn begin_call(
        &self,
        account: &str,
        call_id: &str,
        authority_generation: &str,
    ) -> Result<bool> {
        if !id(call_id) || !id(authority_generation) {
            bail!("invalid_call_intent");
        }
        self.control(account,|db|{
            let inserted=db.execute("INSERT INTO call_sessions(call_id,authority_generation,state,updated_at) SELECT ?1,?2,'active',?3 WHERE NOT EXISTS(SELECT 1 FROM call_sessions WHERE state!='ended') ON CONFLICT(call_id) DO NOTHING",params![call_id,authority_generation,unix_seconds()])?;
            Ok(inserted==1)
        })
    }
    /// Claim an immutable action before any socket submission. Reuse with a
    /// different request/route fails; matching reuse returns false, never resends.
    pub fn begin_call_action(
        &self,
        account: &str,
        call_id: &str,
        action_id: &str,
        route: u16,
        request_digest: &str,
        encrypted_request: &[u8],
    ) -> Result<bool> {
        if !id(call_id)
            || !id(action_id)
            || encrypted_request.len() < 29
            || encrypted_request.len() > 1024 * 1024 + 29
            || encrypted_request[0] != 1
            || request_digest.len() != 64
            || !request_digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(route, 7994 | 8069 | 11421 | 10342 | 9332 | 9262)
        {
            bail!("invalid_call_action");
        }
        self.control(account,|db|{
            let tx=db.transaction()?;
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM call_sessions WHERE call_id=?1 AND state='active')",[call_id],|r|r.get(0))?;
            if !active{bail!("call_needs_review");}
            let prior:Option<(u16,String)>=tx.query_row("SELECT route,request_digest FROM call_actions WHERE call_id=?1 AND action_id=?2",params![call_id,action_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((old_route,old_digest))=prior{if old_route!=route||old_digest!=request_digest{bail!("call_idempotency_conflict");}return Ok(false);}
            tx.execute("INSERT INTO call_actions(call_id,action_id,route,request_digest,outcome,updated_at,encrypted_request)VALUES(?1,?2,?3,?4,'prepared',?5,?6)",params![call_id,action_id,route,request_digest,unix_seconds(),encrypted_request])?;
            tx.commit()?;Ok(true)
        })
    }
    /// Authenticated raw replies must be sealed by the Core vault before storage.
    /// An unacknowledged request remains needs_review even after local cleanup.
    pub fn finish_call_action(
        &self,
        account: &str,
        call_id: &str,
        action_id: &str,
        outcome: &str,
        encrypted_reply: Option<&[u8]>,
    ) -> Result<()> {
        if !id(call_id)
            || !id(action_id)
            || !matches!(outcome, "received" | "needs_review")
            || encrypted_reply.is_some_and(|v| v.is_empty() || v.len() > 1024 * 1024 + 4096)
            || (outcome == "received" && encrypted_reply.is_none())
        {
            bail!("invalid_call_result");
        }
        self.control(account,|db|{
            let tx=db.transaction()?;
            let changed=tx.execute("UPDATE call_actions SET outcome=?3,encrypted_reply=?4,updated_at=?5 WHERE call_id=?1 AND action_id=?2 AND outcome='prepared'",params![call_id,action_id,outcome,encrypted_reply,unix_seconds()])?;
            if changed!=1{bail!("call_action_already_recorded");}
            if outcome=="needs_review"{tx.execute("UPDATE call_sessions SET state='needs_review',updated_at=?2 WHERE call_id=?1",params![call_id,unix_seconds()])?;}
            tx.commit()?;Ok(())
        })
    }
    pub fn finish_call(
        &self,
        account: &str,
        call_id: &str,
        confirmed_terminal: bool,
    ) -> Result<()> {
        if !id(call_id) {
            bail!("invalid_call_intent");
        }
        self.control(account, |db| {
            let changed = db.execute(
                "UPDATE call_sessions SET state=?2,updated_at=?3 WHERE call_id=?1",
                params![
                    call_id,
                    if confirmed_terminal {
                        "ended"
                    } else {
                        "needs_review"
                    },
                    unix_seconds()
                ],
            )?;
            if changed != 1 {
                bail!("call_not_found");
            }
            Ok(())
        })
    }
    pub fn recover_call_sessions(&self, account: &str) -> Result<()> {
        self.control(account,|db|{let tx=db.transaction()?;
            tx.execute("UPDATE call_sessions SET state='needs_review',updated_at=?1 WHERE state='active'",[unix_seconds()])?;
            tx.execute("UPDATE call_actions SET outcome='needs_review',updated_at=?1 WHERE outcome='prepared'",[unix_seconds()])?;
            tx.commit()?;Ok(())})
    }
    pub fn call_action_records(
        &self,
        account: &str,
        call_id: &str,
    ) -> Result<Vec<CallActionRecord>> {
        if !id(call_id) {
            bail!("invalid_call_intent");
        }
        self.control(account,|db|{
            let mut query=db.prepare("SELECT action_id,route,request_digest,outcome,encrypted_request,encrypted_reply FROM call_actions WHERE call_id=?1 ORDER BY rowid LIMIT 4097")?;
            let records=query.query_map([call_id],|r|Ok(CallActionRecord{action_id:r.get(0)?,route:r.get(1)?,request_digest:r.get(2)?,outcome:r.get(3)?,encrypted_request:r.get(4)?,encrypted_reply:r.get(5)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if records.len()>4096 {bail!("call_action_budget_exceeded");}Ok(records)
        })
    }
    pub fn unresolved_call(&self, account: &str) -> Result<Option<String>> {
        self.control(account, |db| Ok(db.query_row("SELECT call_id FROM call_sessions WHERE state!='ended' ORDER BY updated_at,call_id LIMIT 1", [], |r|r.get(0)).optional()?))
    }
    pub fn has_unresolved_call(&self, account: &str) -> Result<bool> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM call_sessions WHERE state!='ended')",
                [],
                |r| r.get(0),
            )?)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unresolved_actions_survive_restart_and_never_authorize_resubmission() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data");
        {
            let store = Store::open(&path).unwrap();
            store.create_account("a").unwrap();
            assert!(store.begin_call("a", "call1", "generation1").unwrap());
            assert!(store
                .begin_call_action("a", "call1", "invite", 10342, &"a".repeat(64), &[1; 30])
                .unwrap());
            assert!(!store
                .begin_call_action("a", "call1", "invite", 10342, &"a".repeat(64), &[1; 30])
                .unwrap());
            assert!(store
                .begin_call_action("a", "call1", "invite", 10342, &"b".repeat(64), &[1; 30])
                .is_err());
        }
        let store = Store::open_existing(&path).unwrap();
        store.recover_call_sessions("a").unwrap();
        assert!(store.has_unresolved_call("a").unwrap());
        assert!(!store.begin_call("a", "call2", "generation1").unwrap());
        assert!(store
            .begin_call_action("a", "call1", "invite", 10342, &"a".repeat(64), &[1; 30])
            .is_err());
        store.finish_call("a", "call1", true).unwrap();
        assert!(store.begin_call("a", "call2", "generation2").unwrap());
    }
    #[test]
    fn exact_request_is_sealed_before_claim_and_preserved_on_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        let vault = crate::vault::Vault::open(&tmp.path().join("secrets/key"), true).unwrap();
        let request = b"synthetic exact common request with invite ID and task";
        let sealed = vault.seal("a", "call-request:c:1", request).unwrap();
        store.begin_call("a", "c", "generation").unwrap();
        assert!(store
            .begin_call_action("a", "c", "1", 10342, &"a".repeat(64), &sealed)
            .unwrap());
        let other = vault.seal("a", "call-request:c:1", b"replacement").unwrap();
        assert!(!store
            .begin_call_action("a", "c", "1", 10342, &"a".repeat(64), &other)
            .unwrap());
        let retained:Vec<u8>=store.control("a",|db|Ok(db.query_row("SELECT encrypted_request FROM call_actions WHERE call_id='c' AND action_id='1'",[],|r|r.get(0))?)).unwrap();
        assert_eq!(
            &*vault
                .open_record("a", "call-request:c:1", &retained)
                .unwrap(),
            request
        );
        assert!(!retained.windows(request.len()).any(|w| w == request));
        store.recover_call_sessions("a").unwrap();
        assert!(store.has_unresolved_call("a").unwrap());
        assert!(store
            .begin_call_action("a", "c", "1", 10342, &"a".repeat(64), &sealed)
            .is_err());
    }
    #[test]
    fn legacy_call_intents_migrate_without_fabricating_request_context() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        {
            let store = Store::open(&root).unwrap();
            store.create_account("a").unwrap();
            store.begin_call("a", "old", "generation").unwrap();
            store.control("a",|db|{db.execute_batch("ALTER TABLE call_actions DROP COLUMN encrypted_request; PRAGMA user_version=9;")?;db.execute("INSERT INTO call_actions(call_id,action_id,route,request_digest,outcome,updated_at) VALUES('old','1',10342,?1,'prepared',1)",["a".repeat(64)])?;Ok(())}).unwrap();
        }
        let store = Store::open_existing(&root).unwrap();
        let records = store.call_action_records("a", "old").unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].encrypted_request.is_none());
        store.recover_call_sessions("a").unwrap();
        let record = store.call_action_records("a", "old").unwrap().remove(0);
        assert_eq!(record.outcome, "needs_review");
        assert!(record.encrypted_request.is_none());
        assert!(!store.begin_call("a", "new", "new-generation").unwrap());
    }
    #[test]
    fn received_receipt_is_immutable_and_local_cleanup_cannot_claim_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.begin_call("a", "call", "generation").unwrap();
        store
            .begin_call_action("a", "call", "auth", 7994, &"a".repeat(64), &[1; 30])
            .unwrap();
        store
            .finish_call_action(
                "a",
                "call",
                "auth",
                "received",
                Some(b"synthetic-sealed-reply"),
            )
            .unwrap();
        assert!(store
            .finish_call_action("a", "call", "auth", "needs_review", None)
            .is_err());
        store.finish_call("a", "call", false).unwrap();
        assert!(store.has_unresolved_call("a").unwrap());
    }
}
