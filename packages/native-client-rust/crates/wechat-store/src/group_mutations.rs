//! Per-step admission and sealed receipts for one-attempt account mutations.
use super::*;
use rusqlite::OptionalExtension;

pub struct StoredGroupMutation {
    pub operation: Operation,
    pub phase: String,
    pub step: usize,
    pub encrypted_request: Vec<u8>,
    pub encrypted_receipts: Vec<u8>,
}
fn read(db: &Connection, account: &str, id: &str) -> Result<StoredGroupMutation> {
    let (kind,state,result,created,updated,phase,step,request,receipts):(String,String,String,String,String,String,i64,Vec<u8>,Vec<u8>)=db.query_row(
        "SELECT o.kind,o.state,o.result_json,o.created_at,o.updated_at,g.phase,g.step,g.encrypted_request,g.encrypted_receipts FROM operations o JOIN group_mutations g ON o.id=g.operation_id WHERE o.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
    if kind != "group_mutation" {
        bail!("group_operation_identity_conflict");
    }
    Ok(StoredGroupMutation {
        operation: Operation {
            id: id.into(),
            account_id: account.into(),
            kind,
            state,
            result: serde_json::from_str(&result)?,
            created_at: created,
            updated_at: updated,
        },
        phase,
        step: usize::try_from(step)?,
        encrypted_request: request,
        encrypted_receipts: receipts,
    })
}
fn write(tx: &rusqlite::Transaction<'_>, op: &Operation) -> Result<()> {
    tx.execute("UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1 AND kind='group_mutation'",params![op.id,op.state,serde_json::to_string(&op.result)?,op.updated_at])?;
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
fn key_hash(key: &str) -> Result<String> {
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        bail!("invalid_group_idempotency_key");
    }
    Ok(format!("{:x}", Sha256::digest(key.as_bytes())))
}
impl Store {
    pub fn incoming_group_refreshes(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account,|db| {
            let mut q=db.prepare("SELECT key,value FROM runtime WHERE key GLOB 'incoming-group-pending:*' ORDER BY rowid LIMIT 4")?;
            let rows=q.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
    pub fn group_mutation(&self, account: &str, id: &str) -> Result<StoredGroupMutation> {
        self.control(account, |db| read(db, account, id))
    }
    pub fn group_mutation_by_key(
        &self,
        account: &str,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Option<StoredGroupMutation>> {
        let key = key_hash(key)?;
        self.control(account, |db| {
            let row: Option<(String, String)> = db
                .query_row(
                    "SELECT operation_id,request_hash FROM group_mutations WHERE key_hash=?1",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, saved)) = row else {
                return Ok(None);
            };
            if hash.is_some_and(|hash| hash != saved) {
                bail!("idempotency_conflict");
            }
            Ok(Some(read(db, account, &id)?))
        })
    }
    pub fn create_group_mutation(
        &self,
        op: &Operation,
        key: &str,
        hash: &str,
        request: &[u8],
        receipts: &[u8],
    ) -> Result<(Operation, bool)> {
        let key = key_hash(key)?;
        if op.kind != "group_mutation"
            || op.state != "queued"
            || hash.len() != 64
            || request.is_empty()
            || receipts.is_empty()
        {
            bail!("invalid_group_operation");
        }
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT operation_id,request_hash FROM group_mutations WHERE key_hash=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, saved)) = prior {
                if saved != hash {
                    bail!("idempotency_conflict");
                }
                return Ok((read(&tx, &op.account_id, &id)?.operation, false));
            }
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
                "INSERT INTO group_mutations VALUES(?1,?2,?3,'queued',0,?4,?5)",
                params![op.id, key, hash, request, receipts],
            )?;
            write(&tx, op)?;
            tx.commit()?;
            Ok((op.clone(), true))
        })
    }
    /// Atomic network admission: the account actor is held, but independent
    /// disconnect/logout/credential transitions must still win this transaction.
    pub fn begin_group_step(
        &self,
        account: &str,
        id: &str,
        step: usize,
        expected: &[u8],
        epoch: Option<&[u8]>,
        owner: &str,
    ) -> Result<bool> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let record = read(&tx, account, id)?;
            if record.phase != "queued" || record.step != step {
                return Ok(false);
            }
            let value = |k: &str| -> Result<Option<Vec<u8>>> {
                Ok(tx
                    .query_row("SELECT value FROM runtime WHERE key=?1", [k], |r| r.get(0))
                    .optional()?)
            };
            let generation = format!("{:x}", Sha256::digest(expected));
            if value("logged-out")?.is_some()
                || value("desired")?.as_deref() != Some(b"connected")
                || value("validated-generation")?.as_deref() != Some(owner.as_bytes())
                || value("account-state")?.as_deref() != Some(expected)
                || value("saved-login-accepted-state-generation")?.as_deref()
                    != Some(generation.as_bytes())
                || value("logout-epoch")?.as_deref() != epoch
            {
                bail!("authorization_generation_changed");
            }
            let mut op = record.operation;
            op.state = "running".into();
            op.updated_at = chrono::Utc::now().to_rfc3339();
            op.result["remoteOutcome"] = serde_json::json!("unknown");
            tx.execute(
                "UPDATE group_mutations SET phase='submitting' WHERE operation_id=?1",
                [id],
            )?;
            write(&tx, &op)?;
            tx.commit()?;
            Ok(true)
        })
    }
    /// The complete encrypted response ledger is durable BEFORE strict decode.
    pub fn receive_group_step(
        &self,
        account: &str,
        id: &str,
        step: usize,
        receipts: &[u8],
    ) -> Result<()> {
        self.control(account,|db|{
            let n=db.execute("UPDATE group_mutations SET phase='received',encrypted_receipts=?3 WHERE operation_id=?1 AND phase='submitting' AND step=?2",params![id,step,receipts])?;
            if n!=1 {bail!("group_step_conflict");}Ok(())
        })
    }
    pub fn advance_group_mutation(&self, op: &Operation, phase: &str, step: usize) -> Result<()> {
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            let prior = read(&tx, &op.account_id, &op.id)?;
            let allowed = match prior.phase.as_str() {
                "queued" => matches!(phase, "failed" | "cancelled" | "needs_review"),
                "submitting" => phase == "needs_review",
                "received" => matches!(phase, "queued" | "projecting" | "failed" | "needs_review"),
                "projecting" => matches!(
                    phase,
                    "succeeded" | "failed" | "projecting" | "needs_review"
                ),
                "needs_review" => matches!(phase, "needs_review" | "projecting"),
                "succeeded" => phase == "succeeded",
                "failed" => phase == "failed",
                "cancelled" => phase == "cancelled",
                _ => false,
            };
            if !allowed
                || op.kind != prior.operation.kind
                || step < prior.step
                || step > prior.step + 1
            {
                bail!("invalid_group_transition");
            }
            tx.execute(
                "UPDATE group_mutations SET phase=?2,step=?3 WHERE operation_id=?1",
                params![op.id, phase, step],
            )?;
            write(&tx, op)?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn cancel_group_mutation(&self, account: &str, id: &str) -> Result<Operation> {
        let mut record = self.group_mutation(account, id)?;
        if record.phase != "queued" || record.step != 0 {
            bail!("operation_cannot_cancel");
        }
        record.operation.state = "cancelled".into();
        record.operation.updated_at = chrono::Utc::now().to_rfc3339();
        self.advance_group_mutation(&record.operation, "cancelled", record.step)?;
        Ok(record.operation)
    }
    pub fn recover_group_mutations(&self, account: &str) -> Result<usize> {
        let ids=self.control(account,|db|{let mut q=db.prepare("SELECT operation_id FROM group_mutations WHERE phase IN ('queued','submitting','received')")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)})?;
        for id in &ids {
            let mut record = self.group_mutation(account, id)?;
            let unstarted = record.phase == "queued" && record.step == 0;
            record.operation.state = if unstarted { "failed" } else { "needs_review" }.into();
            record.operation.result["remoteOutcome"] = serde_json::json!(if unstarted {
                "not_attempted"
            } else {
                "unknown"
            });
            record.operation.result["code"] = serde_json::json!("operation_interrupted");
            record.operation.updated_at = chrono::Utc::now().to_rfc3339();
            self.advance_group_mutation(
                &record.operation,
                if unstarted { "failed" } else { "needs_review" },
                record.step,
            )?;
        }
        Ok(ids.len())
    }
    pub fn projecting_group_mutations(&self, account: &str) -> Result<Vec<StoredGroupMutation>> {
        self.control(account, |db| {
            let ids = {
                let mut q = db
                .prepare("SELECT operation_id FROM group_mutations WHERE phase='needs_review' OR (phase IN ('projecting','cancelled','failed','succeeded') AND json_extract((SELECT result_json FROM operations WHERE id=operation_id),'$.profileProjection')='pending')")?;
                let rows = q.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ids.iter().map(|id| read(db, account, id)).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unsubmitted_intents_can_cancel_and_recovery_keeps_partial_steps_uncertain() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        for (id, step) in [("unstarted", 0), ("partly-submitted", 1)] {
            let op = Operation {
                id: id.into(),
                account_id: "one".into(),
                kind: "group_mutation".into(),
                state: "queued".into(),
                result: serde_json::json!({"profileProjection":"pending"}),
                created_at: "now".into(),
                updated_at: "now".into(),
            };
            store
                .create_group_mutation(
                    &op,
                    id,
                    &"a".repeat(64),
                    b"sealed request",
                    b"sealed receipts",
                )
                .unwrap();
            if step == 0 {
                assert_eq!(
                    store.cancel_group_mutation("one", id).unwrap().state,
                    "cancelled"
                );
            } else {
                store
                    .control("one", |db| {
                        db.execute(
                            "UPDATE group_mutations SET phase='received' WHERE operation_id=?1",
                            [id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                store.advance_group_mutation(&op, "queued", 1).unwrap();
                assert!(store.cancel_group_mutation("one", id).is_err());
                store.recover_group_mutations("one").unwrap();
                let held = store.group_mutation("one", id).unwrap();
                assert_eq!(held.phase, "needs_review");
                assert_eq!(held.step, 1);
                assert_eq!(held.operation.result["remoteOutcome"], "unknown");
            }
        }
    }
}
