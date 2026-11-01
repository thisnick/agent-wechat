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
    /// Record only a proven failure before the business request was handed off.
    /// This preserves the request and spent action ID; it grants no retry or new
    /// operation. An active session can still perform guarded cleanup, but an
    /// already uncertain or ended session is never reactivated here.
    pub fn finish_call_action_not_submitted(
        &self,
        account: &str,
        call_id: &str,
        action_id: &str,
    ) -> Result<()> {
        if !id(call_id) || !id(action_id) {
            bail!("invalid_call_result");
        }
        self.control(account, |db| {
            let changed = db.execute(
                "UPDATE call_actions SET outcome='not_submitted',updated_at=?3 WHERE call_id=?1 AND action_id=?2 AND outcome='prepared'",
                params![call_id, action_id, unix_seconds()],
            )?;
            if changed != 1 {
                bail!("call_action_already_recorded");
            }
            Ok(())
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
        assert!(store
            .begin_call_action("a", "c", "1", 10342, &"b".repeat(64), &sealed)
            .is_err());
        let reply = vault
            .seal("a", "call-reply:c:1", b"synthetic receipt")
            .unwrap();
        store
            .finish_call_action("a", "c", "1", "received", Some(&reply))
            .unwrap();
        assert!(store
            .finish_call_action("a", "c", "1", "needs_review", None)
            .is_err());
        store.finish_call("a", "c", false).unwrap();
        drop(store);
        let store = Store::open_existing(&tmp.path().join("data")).unwrap();
        store.recover_call_sessions("a").unwrap();
        let record = store.call_action_records("a", "c").unwrap().remove(0);
        assert_eq!(record.outcome, "received");
        assert_eq!(record.encrypted_reply.as_deref(), Some(reply.as_slice()));
        assert!(!store.begin_call("a", "next", "generation2").unwrap());
        assert!(store.has_unresolved_call("a").unwrap());
        assert!(store
            .begin_call_action("a", "c", "1", 10342, &"a".repeat(64), &sealed)
            .is_err());

        store.finish_call("a", "c", true).unwrap();
        assert!(store.begin_call("a", "next", "generation2").unwrap());
    }
    #[test]
    fn proven_non_submission_preserves_intent_and_never_reuses_the_action() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("a").unwrap();
        store.begin_call("a", "call", "generation").unwrap();
        let request = [1; 30];
        store
            .begin_call_action("a", "call", "1", 9262, &"a".repeat(64), &request)
            .unwrap();
        store
            .finish_call_action_not_submitted("a", "call", "1")
            .unwrap();
        let record = store.call_action_records("a", "call").unwrap().remove(0);
        assert_eq!(record.outcome, "not_submitted");
        assert_eq!(
            record.encrypted_request.as_deref(),
            Some(request.as_slice())
        );
        assert!(record.encrypted_reply.is_none());
        assert!(store
            .finish_call_action_not_submitted("a", "call", "1")
            .is_err());
        assert!(!store
            .begin_call_action("a", "call", "1", 9262, &"a".repeat(64), &request)
            .unwrap());
        assert!(store
            .begin_call_action("a", "call", "2", 11421, &"b".repeat(64), &request)
            .unwrap());
        store
            .begin_call_action("a", "call", "received", 11421, &"d".repeat(64), &request)
            .unwrap();
        store
            .finish_call_action(
                "a",
                "call",
                "received",
                "received",
                Some(b"synthetic-sealed-reply"),
            )
            .unwrap();
        store
            .finish_call_action("a", "call", "2", "needs_review", None)
            .unwrap();
        for action in ["2", "received", "missing"] {
            assert!(store
                .finish_call_action_not_submitted("a", "call", action)
                .is_err());
        }
        assert!(store
            .begin_call_action("a", "call", "cleanup", 11421, &"e".repeat(64), &request)
            .is_err());
        assert!(!store.begin_call("a", "another", "generation").unwrap());
        drop(store);

        let store = Store::open_existing(&root).unwrap();
        store.recover_call_sessions("a").unwrap();
        let records = store.call_action_records("a", "call").unwrap();
        assert_eq!(records[0].outcome, "not_submitted");
        assert_eq!(records[1].outcome, "needs_review");
        assert!(store
            .begin_call_action("a", "call", "3", 11421, &"c".repeat(64), &request)
            .is_err());
    }
    #[test]
    fn legacy_migrations_preserve_evidence_and_never_fabricate_request_context() {
        for version in [9, 10] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("data");
            {
                let store = Store::open(&root).unwrap();
                store.create_account("a").unwrap();
                store.control("a", |db| {
                    db.execute_batch("DROP TABLE call_actions; DROP TABLE call_sessions;")?;
                    db.execute_batch(include_str!("../migrations/account/V009__call_actions.sql"))?;
                    if version == 10 {
                        db.execute_batch(include_str!("../migrations/account/V010__call_request_context.sql"))?;
                    }
                    db.execute("INSERT INTO call_sessions VALUES('old','generation','active',1)", [])?;
                    db.execute("INSERT INTO call_actions(rowid,call_id,action_id,route,request_digest,outcome,encrypted_reply,updated_at) VALUES(4,'old','first',9262,?1,'prepared',NULL,1)", ["a".repeat(64)])?;
                    db.execute("INSERT INTO call_actions(rowid,call_id,action_id,route,request_digest,outcome,encrypted_reply,updated_at) VALUES(42,'old','last',11421,?1,'received',?2,2)", params!["b".repeat(64), &[2_u8; 30][..]])?;
                    if version == 10 {
                        db.execute("UPDATE call_actions SET encrypted_request=?1 WHERE action_id='last'", [&[1_u8; 30][..]])?;
                    }
                    db.pragma_update(None, "user_version", version)?;
                    Ok(())
                }).unwrap();
            }
            let store = Store::open_existing(&root).unwrap();
            let records = store.call_action_records("a", "old").unwrap();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].action_id, "first");
            assert!(records[0].encrypted_request.is_none());
            assert!(records[0].encrypted_reply.is_none());
            assert_eq!(records[1].action_id, "last");
            assert_eq!(
                records[1].encrypted_request.as_deref(),
                (version == 10).then_some([1; 30].as_slice())
            );
            assert_eq!(
                records[1].encrypted_reply.as_deref(),
                Some([2; 30].as_slice())
            );
            store
                .control("a", |db| {
                    assert_eq!(
                        db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))?,
                        11
                    );
                    let rows = db
                        .prepare("SELECT rowid FROM call_actions ORDER BY rowid")?
                        .query_map([], |r| r.get::<_, u64>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    assert_eq!(rows, [4, 42]);
                    assert!(db
                        .prepare("PRAGMA foreign_key_check")?
                        .query([])?
                        .next()?
                        .is_none());
                    Ok(())
                })
                .unwrap();
            store.recover_call_sessions("a").unwrap();
            let records = store.call_action_records("a", "old").unwrap();
            assert_eq!(records[0].outcome, "needs_review");
            assert!(records[0].encrypted_request.is_none());
            assert_eq!(records[1].outcome, "received");
            assert!(!store.begin_call("a", "new", "new-generation").unwrap());
            assert!(store
                .finish_call_action_not_submitted("a", "old", "first")
                .is_err());
        }
    }
}
