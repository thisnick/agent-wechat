//! Durable one-attempt claims are indexed by actual account/thread/server ID.
use super::*;
use rusqlite::OptionalExtension;

pub struct StoredMessageRecall {
    pub operation: Operation,
    pub phase: String,
    pub encrypted_request: Vec<u8>,
}

fn read(db: &Connection, account: &str, id: &str) -> Result<StoredMessageRecall> {
    let (kind,state,result,created_at,updated_at,phase,encrypted_request):(String,String,String,String,String,String,Vec<u8>)=db.query_row(
        "SELECT o.kind,o.state,o.result_json,o.created_at,o.updated_at,r.phase,r.encrypted_request FROM operations o JOIN outgoing_recalls r ON r.operation_id=o.id WHERE o.id=?1",[id],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    if kind != "message_recall" {
        bail!("recall_operation_identity_conflict");
    }
    Ok(StoredMessageRecall {
        operation: Operation {
            id: id.into(),
            account_id: account.into(),
            kind,
            state,
            result: serde_json::from_str(&result)?,
            created_at,
            updated_at,
        },
        phase,
        encrypted_request,
    })
}
fn event(tx: &rusqlite::Transaction<'_>, op: &Operation) -> Result<()> {
    tx.execute(
        "INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)",
        params![
            serde_json::to_string(
                &serde_json::json!({"id":op.id,"kind":op.kind,"state":op.state})
            )?,
            op.updated_at
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM events WHERE sequence<=?1",
        [seq.saturating_sub(10_000)],
    )?;
    Ok(())
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Called inside the tombstone/event transaction. The notification envelope ID
/// never participates: only an already verified exact recall target can finish
/// a previously uncertain own operation. A duplicate marker emits no op event.
pub(crate) fn confirm_recall_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    thread: &str,
    server: &str,
) -> Result<()> {
    let ids = {
        let mut q=tx.prepare("SELECT r.operation_id FROM outgoing_recalls r JOIN operations o ON o.id=r.operation_id WHERE r.phase='needs_review' AND json_extract(o.result_json,'$.threadId')=?1 AND json_extract(o.result_json,'$.serverId')=?2")?;
        let rows = q.query_map(params![thread, server], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in ids {
        let mut op = read(tx, account, &id)?.operation;
        op.state = "succeeded".into();
        op.result["status"] = serde_json::json!("already_recalled");
        op.result["remoteOutcome"] = serde_json::json!("confirmed");
        op.result["profileProjection"] = serde_json::json!("pending");
        op.result.as_object_mut().map(|v| v.remove("code"));
        op.updated_at = chrono::Utc::now().to_rfc3339();
        tx.execute("UPDATE outgoing_recalls SET phase='succeeded' WHERE operation_id=?1 AND phase='needs_review'",[&id])?;
        tx.execute(
            "UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1",
            params![
                id,
                op.state,
                serde_json::to_string(&op.result)?,
                op.updated_at
            ],
        )?;
        event(tx, &op)?;
    }
    Ok(())
}

pub(crate) fn finish_projection_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    thread: &str,
    server: &str,
) -> Result<()> {
    let ids = {
        let mut q=tx.prepare("SELECT r.operation_id FROM outgoing_recalls r JOIN operations o ON o.id=r.operation_id WHERE r.phase='succeeded' AND json_extract(o.result_json,'$.threadId')=?1 AND json_extract(o.result_json,'$.serverId')=?2 AND json_extract(o.result_json,'$.profileProjection')='pending'")?;
        let rows = q.query_map(params![thread, server], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in ids {
        let mut op = read(tx, account, &id)?.operation;
        op.result["profileProjection"] = serde_json::json!("complete");
        op.updated_at = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE operations SET result_json=?2,updated_at=?3 WHERE id=?1",
            params![id, serde_json::to_string(&op.result)?, op.updated_at],
        )?;
        event(tx, &op)?;
    }
    Ok(())
}

impl Store {
    pub fn message_recall_by_key(
        &self,
        account: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<StoredMessageRecall>> {
        if key.is_empty() || key.len() > 200 || !valid_hash(request_hash) {
            bail!("invalid_recall_request");
        }
        self.control(account, |db| {
            let row: Option<(String, String)> = db
                .query_row(
                    "SELECT operation_id,request_hash FROM recall_keys WHERE key_hash=?1",
                    [digest(key)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, hash)) = row else {
                return Ok(None);
            };
            if hash != request_hash {
                bail!("idempotency_conflict");
            }
            Ok(Some(read(db, account, &id)?))
        })
    }
    pub fn create_message_recall(
        &self,
        op: &Operation,
        key: &str,
        request_hash: &str,
        target_key: &str,
        source_key: &str,
        encrypted: &[u8],
    ) -> Result<(StoredMessageRecall, bool)> {
        if op.kind != "message_recall"
            || !matches!(op.state.as_str(), "queued" | "failed" | "succeeded")
            || key.is_empty()
            || key.len() > 200
            || ![request_hash, target_key, source_key]
                .into_iter()
                .all(valid_hash)
            || encrypted.is_empty()
            || encrypted.len() > 64 * 1024
        {
            bail!("invalid_recall_request");
        }
        self.control(&op.account_id,|db|{
            let tx=db.transaction()?;
            let prior_key:Option<(String,String)>=tx.query_row("SELECT operation_id,request_hash FROM recall_keys WHERE key_hash=?1",[digest(key)],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((id,hash))=prior_key {if hash!=request_hash {bail!("idempotency_conflict");}return Ok((read(&tx,&op.account_id,&id)?,false));}
            let prior:Option<String>=tx.query_row("SELECT operation_id FROM outgoing_recalls WHERE target_key=?1 OR source_key=?2",params![target_key,source_key],|r|r.get(0)).optional()?;
            let (id,created)=if let Some(id)=prior {(id,false)} else {
                let phase=if op.state=="queued" {"queued"}else{op.state.as_str()};
                tx.execute("INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6)",params![op.id,op.kind,op.state,serde_json::to_string(&op.result)?,op.created_at,op.updated_at])?;
                tx.execute("INSERT INTO outgoing_recalls VALUES(?1,?2,?3,?4,?5)",params![op.id,target_key,source_key,phase,encrypted])?;
                event(&tx,op)?;(op.id.clone(),true)
            };
            tx.execute("INSERT INTO recall_keys VALUES(?1,?2,?3)",params![digest(key),request_hash,id])?;
            let record=read(&tx,&op.account_id,&id)?;tx.commit()?;Ok((record,created))
        })
    }
    /// Replay by the frozen source/server pair before re-reading a row which
    /// may since have been recalled, moved, or removed from the profile.
    pub fn message_recall_by_source(
        &self,
        account: &str,
        key: &str,
        request_hash: &str,
        source_key: &str,
    ) -> Result<Option<StoredMessageRecall>> {
        if key.is_empty() || key.len() > 200 || !valid_hash(request_hash) || !valid_hash(source_key)
        {
            bail!("invalid_recall_request");
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT operation_id,request_hash FROM recall_keys WHERE key_hash=?1",
                    [digest(key)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, hash)) = prior {
                if hash != request_hash {
                    bail!("idempotency_conflict");
                }
                return Ok(Some(read(&tx, account, &id)?));
            }
            let id: Option<String> = tx
                .query_row(
                    "SELECT operation_id FROM outgoing_recalls WHERE source_key=?1",
                    [source_key],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(None);
            };
            tx.execute(
                "INSERT INTO recall_keys VALUES(?1,?2,?3)",
                params![digest(key), request_hash, id],
            )?;
            let record = read(&tx, account, &id)?;
            tx.commit()?;
            Ok(Some(record))
        })
    }
    pub fn message_recall(&self, account: &str, id: &str) -> Result<StoredMessageRecall> {
        self.control(account, |db| read(db, account, id))
    }

    /// This transaction is the network admission boundary. The caller holds
    /// its account mutation guard; an independently committed logout still wins.
    pub fn claim_message_recall(
        &self,
        account: &str,
        id: &str,
        expected_state: &[u8],
        epoch: Option<&[u8]>,
        owner: &str,
    ) -> Result<bool> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let record = read(&tx, account, id)?;
            if record.phase != "queued" {
                return Ok(false);
            }
            let value = |key: &str| -> Result<Option<Vec<u8>>> {
                Ok(tx
                    .query_row("SELECT value FROM runtime WHERE key=?1", [key], |r| {
                        r.get(0)
                    })
                    .optional()?)
            };
            let accepted = format!("{:x}", Sha256::digest(expected_state));
            if value("logged-out")?.is_some()
                || value("desired")?.as_deref() != Some(b"connected")
                || value("validated-generation")?.as_deref() != Some(owner.as_bytes())
                || value("account-state")?.as_deref() != Some(expected_state)
                || value("saved-login-accepted-state-generation")?.as_deref()
                    != Some(accepted.as_bytes())
                || value("logout-epoch")?.as_deref() != epoch
            {
                bail!("authorization_generation_changed");
            }
            let mut op = record.operation;
            op.state = "running".into();
            op.updated_at = chrono::Utc::now().to_rfc3339();
            op.result["remoteOutcome"] = serde_json::json!("unknown");
            tx.execute(
                "UPDATE outgoing_recalls SET phase='submitting' WHERE operation_id=?1",
                [id],
            )?;
            tx.execute(
                "UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1",
                params![
                    id,
                    op.state,
                    serde_json::to_string(&op.result)?,
                    op.updated_at
                ],
            )?;
            event(&tx, &op)?;
            tx.commit()?;
            Ok(true)
        })
    }
    /// Retain remote acceptance before any independently committed projection.
    /// A needs_review claim can become successful only with an exact tombstone.
    pub fn save_message_recall_progress(&self, op: &Operation, phase: &str) -> Result<()> {
        self.control(&op.account_id,|db|{
            let tx=db.transaction()?;let prior=read(&tx,&op.account_id,&op.id)?;
            if op.kind!=prior.operation.kind||["messageId","threadId","serverId"].into_iter().any(|k|op.result[k]!=prior.operation.result[k]) {bail!("recall_operation_identity_conflict");}
            let allowed=match prior.phase.as_str(){
                "queued"=>matches!(phase,"failed"|"succeeded"),
                "submitting"=>matches!(phase,"accepted"|"failed"|"needs_review"),
                "accepted"=>matches!(phase,"accepted"|"succeeded"),
                "needs_review"=>matches!(phase,"needs_review"|"succeeded"),
                "succeeded"=>phase=="succeeded",
                "failed"=>phase=="failed",_=>false};
            if !allowed {bail!("invalid_recall_transition");}
            if prior.phase=="needs_review"&&phase=="succeeded" {
                let marker:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM message_recalls WHERE thread_id=?1 AND server_id=?2)",params![op.result["threadId"].as_str(),op.result["serverId"].as_str()],|r|r.get(0))?;
                if !marker {bail!("recall_confirmation_required");}
            }
            tx.execute("UPDATE outgoing_recalls SET phase=?2 WHERE operation_id=?1",params![op.id,phase])?;
            tx.execute("UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1",params![op.id,op.state,serde_json::to_string(&op.result)?,op.updated_at])?;
            event(&tx,op)?;tx.commit()?;Ok(())
        })
    }
    pub fn pending_message_recalls(&self, account: &str) -> Result<usize> {
        self.control(account,|db|Ok(db.query_row("SELECT count(*) FROM outgoing_recalls WHERE phase IN ('queued','submitting','accepted','needs_review')",[],|r|r.get(0))?))
    }
    pub fn accepted_message_recalls(&self, account: &str) -> Result<Vec<StoredMessageRecall>> {
        self.control(account,|db|{
            let ids={let mut q=db.prepare("SELECT r.operation_id FROM outgoing_recalls r JOIN operations o ON o.id=r.operation_id WHERE r.phase='accepted' OR (r.phase='succeeded' AND json_extract(o.result_json,'$.profileProjection')='pending')")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;rows.collect::<rusqlite::Result<Vec<_>>>()?};
            ids.iter().map(|id|read(db,account,id)).collect()
        })
    }
    /// A restarted owner never resumes RPC work. Accepted replies remain
    /// available for local marker/projection repair; all other uncertainty holds.
    pub fn recover_message_recalls(&self, account: &str) -> Result<usize> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            let ids={let mut q=tx.prepare("SELECT operation_id FROM outgoing_recalls WHERE phase IN ('queued','submitting','accepted')")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;rows.collect::<rusqlite::Result<Vec<_>>>()?};
            for id in &ids {
                let record=read(&tx,account,id)?;let mut op=record.operation;
                let phase=if record.phase=="queued" {op.state="failed".into();op.result["status"]=serde_json::json!("unavailable");op.result["remoteOutcome"]=serde_json::json!("not_attempted");"failed"}else {op.state="needs_review".into();op.result["status"]=serde_json::json!("needs_review");if record.phase=="accepted" {"accepted"}else{"needs_review"}};
                op.result["code"]=serde_json::json!("operation_interrupted");op.updated_at=chrono::Utc::now().to_rfc3339();
                tx.execute("UPDATE outgoing_recalls SET phase=?2 WHERE operation_id=?1",params![id,phase])?;
                tx.execute("UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1",params![id,op.state,serde_json::to_string(&op.result)?,op.updated_at])?;event(&tx,&op)?;
            }
            tx.commit()?;Ok(ids.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("data")).unwrap();
        for account in ["a", "b"] {
            store.create_account(account).unwrap();
            store.put_runtime(account, "desired", b"connected").unwrap();
            store
                .put_runtime(account, "validated-generation", b"owner")
                .unwrap();
            store
                .save_accepted_authorization(account, b"cipher")
                .unwrap();
        }
        (temporary, store)
    }
    fn op(account: &str, id: &str) -> Operation {
        Operation {
            id: id.into(),
            account_id: account.into(),
            kind: "message_recall".into(),
            state: "queued".into(),
            result: serde_json::json!({"messageId":"msg-synthetic","threadId":"peer","serverId":u64::MAX.to_string(),"status":"pending","remoteOutcome":"not_attempted","retryable":false,"profileProjection":"pending"}),
            created_at: "2026-10-21T00:00:00Z".into(),
            updated_at: "2026-10-21T00:00:00Z".into(),
        }
    }
    fn create(store: &Store, account: &str, id: &str, key: &str, target: &str) -> Operation {
        let op = op(account, id);
        let (saved, created) = store
            .create_message_recall(
                &op,
                key,
                &digest(&format!("request:{target}")),
                &digest(&format!("target:{target}")),
                &digest(&format!("source:{target}")),
                b"encrypted-synthetic-request",
            )
            .unwrap();
        assert!(created);
        saved.operation
    }
    fn claim(store: &Store, account: &str, id: &str) -> bool {
        store
            .claim_message_recall(account, id, b"cipher", None, "owner")
            .unwrap()
    }

    #[test]
    fn all_keys_share_one_canonical_target_and_conflicting_reuse_is_rejected() {
        let (_temporary, store) = fixture();
        let first = create(&store, "a", "first", "key1", "one");
        let (same, created) = store
            .create_message_recall(
                &op("a", "second"),
                "key2",
                &digest("another-payload-alias"),
                &digest("target:one"),
                &digest("another-source"),
                b"encrypted-alias",
            )
            .unwrap();
        assert!(!created);
        assert_eq!(same.operation.id, first.id);
        assert_eq!(
            store
                .message_recall_by_key("a", "key2", &digest("another-payload-alias"))
                .unwrap()
                .unwrap()
                .operation
                .id,
            first.id
        );
        assert!(store
            .message_recall_by_key("a", "key2", &digest("different-payload"))
            .is_err());
        assert!(store
            .create_message_recall(
                &op("a", "third"),
                "key1",
                &digest("different-payload"),
                &digest("target:two"),
                &digest("source:two"),
                b"encrypted"
            )
            .is_err());
        assert!(claim(&store, "a", "first"));
        assert!(!claim(&store, "a", "first"));
        assert_eq!(
            store.message_recall("a", "first").unwrap().phase,
            "submitting"
        );
        assert_eq!(store.pending_message_recalls("a").unwrap(), 1);
    }
    #[test]
    fn exact_source_replay_survives_removed_row_and_new_idempotency_key() {
        let (_temporary, store) = fixture();
        create(&store, "a", "first", "key1", "one");
        let again = store
            .message_recall_by_source("a", "key2", &digest("request:one"), &digest("source:one"))
            .unwrap()
            .unwrap();
        assert_eq!(again.operation.id, "first");
        assert_eq!(
            store
                .message_recall_by_key("a", "key2", &digest("request:one"))
                .unwrap()
                .unwrap()
                .operation
                .id,
            "first"
        );
        assert!(store
            .message_recall_by_source("a", "key2", &digest("request:other"), &digest("source:one"))
            .is_err());
    }
    #[test]
    fn account_scopes_do_not_share_a_target_or_a_key() {
        let (_temporary, store) = fixture();
        create(&store, "a", "first", "key", "same");
        create(&store, "b", "second", "key", "same");
        assert!(claim(&store, "a", "first"));
        assert!(claim(&store, "b", "second"));
        assert_eq!(
            store
                .message_recall_by_key("b", "key", &digest("request:same"))
                .unwrap()
                .unwrap()
                .operation
                .id,
            "second"
        );
        assert!(store.message_recall("b", "first").is_err());
    }
    #[test]
    fn admission_rechecks_connected_owner_state_epoch_and_accepted_generation() {
        for (key, value) in [
            ("desired", b"disconnected".as_slice()),
            ("logged-out", b"fence".as_slice()),
            ("validated-generation", b"another-owner".as_slice()),
            ("account-state", b"another-cipher".as_slice()),
            ("logout-epoch", b"another-epoch".as_slice()),
            (
                "saved-login-accepted-state-generation",
                b"unaccepted".as_slice(),
            ),
        ] {
            let (_temporary, store) = fixture();
            create(&store, "a", "first", "key", "one");
            store.put_runtime("a", key, value).unwrap();
            assert!(
                store
                    .claim_message_recall("a", "first", b"cipher", None, "owner")
                    .is_err(),
                "fence {key}"
            );
            assert_eq!(store.message_recall("a", "first").unwrap().phase, "queued");
        }
    }
    #[test]
    fn uncertain_claim_cannot_succeed_without_exact_target_confirmation() {
        let (_temporary, store) = fixture();
        let mut op = create(&store, "a", "first", "key", "one");
        assert!(claim(&store, "a", "first"));
        op.state = "needs_review".into();
        op.result["status"] = serde_json::json!("needs_review");
        op.result["remoteOutcome"] = serde_json::json!("unknown");
        store
            .save_message_recall_progress(&op, "needs_review")
            .unwrap();
        assert_eq!(store.pending_message_recalls("a").unwrap(), 1);
        assert!(!claim(&store, "a", "first"));
        op.state = "succeeded".into();
        op.result["status"] = serde_json::json!("already_recalled");
        assert!(store
            .save_message_recall_progress(&op, "succeeded")
            .is_err());
        store
            .record_recall(
                "a",
                "other",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        assert!(store
            .save_message_recall_progress(&op, "succeeded")
            .is_err());
        store
            .record_recall(
                "a",
                "peer",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        let confirmed = store.operation("a", "first").unwrap();
        assert_eq!(confirmed.state, "succeeded");
        assert_eq!(confirmed.result["remoteOutcome"], "confirmed");
        assert_eq!(store.pending_message_recalls("a").unwrap(), 0);
    }
    #[test]
    fn restart_preserves_unknown_and_accepted_receipts_without_reclaiming() {
        let (temporary, store) = fixture();
        create(&store, "a", "queued", "q", "queued");
        create(&store, "a", "unknown", "u", "unknown");
        let mut accepted = create(&store, "a", "accepted", "a", "accepted");
        assert!(claim(&store, "a", "unknown"));
        assert!(claim(&store, "a", "accepted"));
        accepted.state = "running".into();
        accepted.result["remoteOutcome"] = serde_json::json!("accepted");
        store
            .save_message_recall_progress(&accepted, "accepted")
            .unwrap();
        drop(store);
        let store = Store::open_existing(&temporary.path().join("data")).unwrap();
        assert_eq!(store.recover_message_recalls("a").unwrap(), 3);
        assert_eq!(store.message_recall("a", "queued").unwrap().phase, "failed");
        assert_eq!(
            store.message_recall("a", "unknown").unwrap().phase,
            "needs_review"
        );
        let accepted = store.message_recall("a", "accepted").unwrap();
        assert_eq!(accepted.phase, "accepted");
        assert_eq!(accepted.operation.state, "needs_review");
        assert_eq!(accepted.operation.result["remoteOutcome"], "accepted");
        for id in ["queued", "unknown", "accepted"] {
            assert!(!claim(&store, "a", id));
        }
        assert_eq!(store.pending_message_recalls("a").unwrap(), 2);
        assert_eq!(store.accepted_message_recalls("a").unwrap().len(), 1);
    }
    #[test]
    fn v005_upgrade_preserves_existing_markers_and_runtime() {
        let (_temporary, store) = fixture();
        store
            .record_recall("a", "peer", "10", None, None, None, "synthetic")
            .unwrap();
        store
            .control("a", |db| {
                db.execute_batch(
                    "DROP TABLE recall_keys;DROP TABLE outgoing_recalls;PRAGMA user_version=5;",
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store.recall_marker("a", "peer", "10").unwrap().is_some());
        assert_eq!(
            store.runtime("a", "account-state").unwrap().unwrap(),
            b"cipher"
        );
        create(&store, "a", "after-upgrade", "key", "one");
        assert!(claim(&store, "a", "after-upgrade"));
    }
    #[test]
    fn incoming_confirmation_and_projection_finish_are_atomic_and_deduplicated_after_restart() {
        let (temporary, store) = fixture();
        create(&store, "a", "unknown", "key", "one");
        assert!(claim(&store, "a", "unknown"));
        drop(store);
        let store = Store::open_existing(&temporary.path().join("data")).unwrap();
        store.recover_message_recalls("a").unwrap();
        let count = |store: &Store| {
            store
                .events("a", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "operation.updated" && e.data["id"] == "unknown")
                .count()
        };
        let before = count(&store);
        store
            .record_recall(
                "a",
                "other",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        assert_eq!(
            store.operation("a", "unknown").unwrap().state,
            "needs_review"
        );
        assert_eq!(count(&store), before);
        store
            .record_recall(
                "a",
                "peer",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        let op = store.operation("a", "unknown").unwrap();
        assert_eq!(op.state, "succeeded");
        assert_eq!(op.result["status"], "already_recalled");
        assert_eq!(op.result["profileProjection"], "pending");
        assert_eq!(count(&store), before + 1);
        assert_eq!(store.pending_message_recalls("a").unwrap(), 0);
        store
            .record_recall(
                "a",
                "peer",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        assert_eq!(count(&store), before + 1);
        store
            .finish_recall_projection("a", "peer", &u64::MAX.to_string(), "applied")
            .unwrap();
        assert_eq!(
            store.operation("a", "unknown").unwrap().result["profileProjection"],
            "complete"
        );
        assert_eq!(count(&store), before + 2);
        drop(store);
        let store = Store::open_existing(&temporary.path().join("data")).unwrap();
        assert_eq!(store.recover_message_recalls("a").unwrap(), 0);
        store
            .record_recall(
                "a",
                "peer",
                &u64::MAX.to_string(),
                None,
                None,
                None,
                "synthetic",
            )
            .unwrap();
        store
            .finish_recall_projection("a", "peer", &u64::MAX.to_string(), "applied")
            .unwrap();
        assert_eq!(count(&store), before + 2);
        assert!(!claim(&store, "a", "unknown"));
    }
}
