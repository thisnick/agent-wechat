//! Per-step admission and sealed receipts for one-attempt account mutations.
use super::*;
use rusqlite::OptionalExtension;

/// One protected contact intent and its progress, separate from canonical contact state.
pub struct StoredContactMutation {
    /// Public operation status and sanitized outcome.
    pub operation: Operation,
    /// Internal admission, receipt and projection phase.
    pub phase: String,
    /// Monotonic submission step.
    pub step: usize,
    /// Sealed input and exact prepared request.
    pub encrypted_request: Vec<u8>,
    /// Sealed authenticated reply and prepared local write.
    pub encrypted_receipts: Vec<u8>,
}
fn read(db: &Connection, account: &str, id: &str) -> Result<StoredContactMutation> {
    let (kind,state,result,created,updated,phase,step,request,receipts):(String,String,String,String,String,String,i64,Vec<u8>,Vec<u8>)=db.query_row(
        "SELECT o.kind,o.state,o.result_json,o.created_at,o.updated_at,g.phase,g.step,g.encrypted_request,g.encrypted_receipts FROM operations o JOIN contact_mutations g ON o.id=g.operation_id WHERE o.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
    if kind != "contact_mutation" {
        bail!("contact_operation_identity_conflict");
    }
    Ok(StoredContactMutation {
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
    tx.execute("UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1 AND kind='contact_mutation'",params![op.id,op.state,serde_json::to_string(&op.result)?,op.updated_at])?;
    tx.execute(
        "INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)",
        params![
            serde_json::to_string(
                &serde_json::json!({"id":op.id,"kind":op.kind,"state":op.state})
            )?,
            op.updated_at
        ],
    )?;
    crate::trim_events(tx)?;
    Ok(())
}
fn key_hash(key: &str) -> Result<String> {
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        bail!("invalid_contact_idempotency_key");
    }
    Ok(format!("{:x}", Sha256::digest(key.as_bytes())))
}
impl Store {
    /// Accepted or partly accepted submissions awaiting canonical contact sync, never resubmitted.
    pub fn pending_contact_relationships(
        &self,
        account: &str,
    ) -> Result<Vec<StoredContactMutation>> {
        self.control(account,|db| {let ids={let mut q=db.prepare("SELECT operation_id FROM contact_mutations WHERE (phase='succeeded' OR (phase='failed' AND json_extract((SELECT result_json FROM operations WHERE id=operation_id),'$.partial')=1)) AND json_extract((SELECT result_json FROM operations WHERE id=operation_id),'$.profileProjection')='awaiting_contact_sync' LIMIT 2000")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;rows.collect::<rusqlite::Result<Vec<_>>>()?};ids.iter().map(|id|read(db,account,id)).collect()})
    }

    /// Unsettled intents, including unknown outcomes, used to guard a new mutation.
    /// Callers inspect the sealed action and exclude their own operation before submission.
    pub fn active_contact_mutations(
        &self,
        account: &str,
        own_operation: Option<&str>,
    ) -> Result<Vec<StoredContactMutation>> {
        self.control(account, |db| {
            let ids = {
                let mut query = db.prepare("SELECT operation_id FROM contact_mutations WHERE phase IN ('queued','submitting','received','projecting','needs_review') AND length(encrypted_request)>0 AND (?1 IS NULL OR operation_id!=?1) LIMIT 2000")?;
                let rows = query.query_map([own_operation], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ids.iter().map(|id| read(db, account, id)).collect()
        })
    }

    /// Detect whether bounded privacy guards could omit an unsettled contact intent.
    /// Includes all candidate actions because optional privacy is in the sealed request.
    /// This admission check does not restrict settlement's existing batch enumeration.
    pub fn contact_privacy_guard_overflow(
        &self,
        account: &str,
        own_operation: Option<&str>,
    ) -> Result<bool> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM contact_mutations c JOIN operations o ON o.id=c.operation_id WHERE (?1 IS NULL OR c.operation_id!=?1) AND ((c.phase IN ('queued','submitting','received','projecting','needs_review') AND length(c.encrypted_request)>0) OR ((c.phase='succeeded' OR (c.phase='failed' AND json_extract(o.result_json,'$.partial')=1)) AND json_extract(o.result_json,'$.profileProjection')='awaiting_contact_sync')) LIMIT 1 OFFSET 2000)",
                [own_operation],
                |row| row.get(0),
            )?)
        })
    }

    /// Save a prepared canonical stage before applying it; no network submission.
    pub fn update_contact_receipts(&self, account: &str, id: &str, receipts: &[u8]) -> Result<()> {
        self.control(account,|db| {let n=db.execute("UPDATE contact_mutations SET encrypted_receipts=?2 WHERE operation_id=?1 AND phase IN ('received','projecting','needs_review')",params![id,receipts])?;if n!=1 {bail!("contact_step_conflict");}Ok(())})
    }

    /// Read one exact operation on its owning account.
    pub fn contact_mutation(&self, account: &str, id: &str) -> Result<StoredContactMutation> {
        self.control(account, |db| read(db, account, id))
    }
    /// Resolve an original key, optionally enforcing the same input hash.
    pub fn contact_mutation_by_key(
        &self,
        account: &str,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Option<StoredContactMutation>> {
        let key = key_hash(key)?;
        self.control(account, |db| {
            let row: Option<(String, String)> = db
                .query_row(
                    "SELECT operation_id,request_hash FROM contact_mutations WHERE key_hash=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, saved)) = row else {
                crate::retention::ensure_key_live(db, crate::retention::CONTACT_KEY, &key)?;
                return Ok(None);
            };
            if hash.is_some_and(|hash| hash != saved) {
                bail!("idempotency_conflict");
            }
            Ok(Some(read(db, account, &id)?))
        })
    }
    /// Atomically save one original intent and its idempotency key.
    pub fn create_contact_mutation(
        &self,
        op: &Operation,
        key: &str,
        hash: &str,
        request: &[u8],
        receipts: &[u8],
    ) -> Result<(Operation, bool)> {
        let key = key_hash(key)?;
        if op.kind != "contact_mutation"
            || op.state != "queued"
            || hash.len() != 64
            || request.is_empty()
            || receipts.is_empty()
        {
            bail!("invalid_contact_operation");
        }
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT operation_id,request_hash FROM contact_mutations WHERE key_hash=?1",
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
            crate::retention::ensure_key_live(&tx, crate::retention::CONTACT_KEY, &key)?;
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
                "INSERT INTO contact_mutations VALUES(?1,?2,?3,'queued',0,?4,?5)",
                params![op.id, key, hash, request, receipts],
            )?;
            write(&tx, op)?;
            tx.commit()?;
            Ok((op.clone(), true))
        })
    }
    /// Atomic network admission: the account actor is held, but independent
    /// disconnect/logout/credential transitions must still win this transaction.
    /// Fence account authorization and mark the single submission before network I/O.
    pub fn begin_contact_step(
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
                "UPDATE contact_mutations SET phase='submitting' WHERE operation_id=?1",
                [id],
            )?;
            write(&tx, &op)?;
            tx.commit()?;
            Ok(true)
        })
    }
    /// The complete encrypted response ledger is durable BEFORE strict decode.
    /// Persist the sealed reply before any decode or canonical write.
    pub fn receive_contact_step(
        &self,
        account: &str,
        id: &str,
        step: usize,
        receipts: &[u8],
    ) -> Result<()> {
        self.control(account,|db|{
            let n=db.execute("UPDATE contact_mutations SET phase='received',encrypted_receipts=?3 WHERE operation_id=?1 AND phase='submitting' AND step=?2",params![id,step,receipts])?;
            if n!=1 {bail!("contact_step_conflict");}Ok(())
        })
    }
    /// Advance only permitted phases and publish an operation event.
    pub fn advance_contact_mutation(&self, op: &Operation, phase: &str, step: usize) -> Result<()> {
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
                bail!("invalid_contact_transition");
            }
            tx.execute(
                "UPDATE contact_mutations SET phase=?2,step=?3 WHERE operation_id=?1",
                params![op.id, phase, step],
            )?;
            write(&tx, op)?;
            tx.commit()?;
            Ok(())
        })
    }
    /// Cancel an unsubmitted queued intent; never cancel a server request.
    pub fn cancel_contact_mutation(&self, account: &str, id: &str) -> Result<Operation> {
        let mut record = self.contact_mutation(account, id)?;
        if record.phase != "queued" || record.step != 0 {
            bail!("operation_cannot_cancel");
        }
        record.operation.state = "cancelled".into();
        record.operation.updated_at = chrono::Utc::now().to_rfc3339();
        self.advance_contact_mutation(&record.operation, "cancelled", record.step)?;
        Ok(record.operation)
    }
    /// Fail unsubmitted work and hold uncertain work without resending.
    pub fn recover_contact_mutations(&self, account: &str) -> Result<usize> {
        let ids=self.control(account,|db|{let mut q=db.prepare("SELECT operation_id FROM contact_mutations WHERE phase IN ('queued','submitting','received')")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)})?;
        for id in &ids {
            let mut record = self.contact_mutation(account, id)?;
            let unstarted = record.phase == "queued" && record.step == 0;
            record.operation.state = if unstarted { "failed" } else { "needs_review" }.into();
            record.operation.result["remoteOutcome"] = serde_json::json!(if unstarted {
                "not_attempted"
            } else {
                "unknown"
            });
            record.operation.result["code"] = serde_json::json!("operation_interrupted");
            record.operation.updated_at = chrono::Utc::now().to_rfc3339();
            self.advance_contact_mutation(
                &record.operation,
                if unstarted { "failed" } else { "needs_review" },
                record.step,
            )?;
        }
        Ok(ids.len())
    }
    /// Find owned receipts eligible for local recovery, without network retries.
    pub fn projecting_contact_mutations(
        &self,
        account: &str,
    ) -> Result<Vec<StoredContactMutation>> {
        self.control(account, |db| {
            let ids = {
                let mut q = db
                .prepare("SELECT operation_id FROM contact_mutations WHERE (phase='needs_review' AND length(encrypted_request)>0) OR (phase IN ('projecting','cancelled','failed','succeeded') AND json_extract((SELECT result_json FROM operations WHERE id=operation_id),'$.profileProjection')='pending')")?;
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
                kind: "contact_mutation".into(),
                state: "queued".into(),
                result: serde_json::json!({"profileProjection":"pending"}),
                created_at: "now".into(),
                updated_at: "now".into(),
            };
            store
                .create_contact_mutation(
                    &op,
                    id,
                    &"a".repeat(64),
                    b"sealed request",
                    b"sealed receipts",
                )
                .unwrap();
            if step == 0 {
                assert_eq!(
                    store.cancel_contact_mutation("one", id).unwrap().state,
                    "cancelled"
                );
            } else {
                store
                    .control("one", |db| {
                        db.execute(
                            "UPDATE contact_mutations SET phase='received' WHERE operation_id=?1",
                            [id],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                store.advance_contact_mutation(&op, "queued", 1).unwrap();
                assert!(store.cancel_contact_mutation("one", id).is_err());
                store.recover_contact_mutations("one").unwrap();
                let held = store.contact_mutation("one", id).unwrap();
                assert_eq!(held.phase, "needs_review");
                assert_eq!(held.step, 1);
                assert_eq!(held.operation.result["remoteOutcome"], "unknown");
            }
        }
    }
}

#[cfg(test)]
mod privacy_pending_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn queued_and_unknown_intents_are_guarded_separately_from_settlement() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        for id in ["queued", "unknown", "failed"] {
            let mut operation = Operation {
                id: id.into(),
                account_id: "one".into(),
                kind: "contact_mutation".into(),
                state: "queued".into(),
                result: serde_json::json!({"action":"update_privacy","remoteOutcome":"unknown"}),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            };
            store
                .create_contact_mutation(&operation, id, &"a".repeat(64), &[1], &[2])
                .unwrap();
            if id != "queued" {
                let phase = if id == "unknown" {
                    "needs_review"
                } else {
                    "failed"
                };
                operation.state = phase.into();
                store
                    .advance_contact_mutation(&operation, phase, 0)
                    .unwrap();
            }
        }
        let active = store
            .active_contact_mutations("one", None)
            .unwrap()
            .into_iter()
            .map(|row| row.operation.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(active, BTreeSet::from(["queued".into(), "unknown".into()]));
        assert!(store
            .pending_contact_relationships("one")
            .unwrap()
            .is_empty());
        assert!(store
            .active_contact_mutations("one", None)
            .unwrap()
            .iter()
            .any(|row| row.operation.id == "unknown" && row.phase == "needs_review"));
    }

    #[test]
    fn privacy_capacity_uses_combined_candidates_excludes_self_and_leaves_settlement_batched() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        let seed = |start: usize, end: usize, phase: &str| {
            store.control("one", |db| {
                let tx = db.transaction()?;
                for index in start..end {
                    let id = format!("capacity-{index:04}");
                    let result = serde_json::json!({"action":"update_privacy","profileProjection":"awaiting_contact_sync"}).to_string();
                    tx.execute("INSERT INTO operations VALUES(?1,'contact_mutation',?2,?3,'now','now')", params![id, phase, result])?;
                    tx.execute("INSERT INTO contact_mutations VALUES(?1,?2,?3,?4,0,?5,?6)", params![id, format!("key-{index}"), "a".repeat(64), phase, &[1u8][..], &[2u8][..]])?;
                }
                tx.commit()?;
                Ok(())
            }).unwrap();
        };
        seed(0, 1000, "queued");
        seed(1000, 2000, "succeeded");
        assert!(!store.contact_privacy_guard_overflow("one", None).unwrap());
        seed(2000, 2001, "queued");
        assert!(store.contact_privacy_guard_overflow("one", None).unwrap());
        assert!(!store
            .contact_privacy_guard_overflow("one", Some("capacity-2000"))
            .unwrap());
        assert!(!store
            .active_contact_mutations("one", Some("capacity-2000"))
            .unwrap()
            .iter()
            .any(|record| record.operation.id == "capacity-2000"));
        seed(2001, 3002, "succeeded");
        assert!(store
            .contact_privacy_guard_overflow("one", Some("capacity-2000"))
            .unwrap());
        let first = store.pending_contact_relationships("one").unwrap();
        assert_eq!(first.len(), 2000);
        store.control("one", |db| {
            let tx = db.transaction()?;
            for record in first {
                tx.execute("UPDATE operations SET result_json=json_set(result_json,'$.profileProjection','complete') WHERE id=?1", [record.operation.id])?;
            }
            tx.commit()?;
            Ok(())
        }).unwrap();
        assert_eq!(store.pending_contact_relationships("one").unwrap().len(), 1);
        assert!(!store.contact_privacy_guard_overflow("one", None).unwrap());
    }

    #[test]
    fn partial_failures_are_listed_until_their_canonical_snapshot_is_observed() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(&temporary.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        for (id, partial, succeeded) in [
            ("accepted", false, true),
            ("partial", true, false),
            ("rejected", false, false),
        ] {
            let mut operation = Operation {
                id: id.into(),
                account_id: "one".into(),
                kind: "contact_mutation".into(),
                state: "queued".into(),
                result: serde_json::json!({"action":"update_privacy","partial":partial,"profileProjection":"awaiting_contact_sync"}),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            };
            store
                .create_contact_mutation(&operation, id, &"a".repeat(64), &[1], &[2])
                .unwrap();
            if succeeded {
                operation.state = "needs_review".into();
                store
                    .advance_contact_mutation(&operation, "needs_review", 0)
                    .unwrap();
                operation.state = "running".into();
                store
                    .advance_contact_mutation(&operation, "projecting", 0)
                    .unwrap();
            }
            operation.state = if succeeded { "succeeded" } else { "failed" }.into();
            store
                .advance_contact_mutation(&operation, &operation.state, 0)
                .unwrap();
        }
        assert!(store
            .active_contact_mutations("one", None)
            .unwrap()
            .is_empty());
        let ids = |store: &Store| {
            store
                .pending_contact_relationships("one")
                .unwrap()
                .into_iter()
                .map(|row| row.operation.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            ids(&store),
            BTreeSet::from(["accepted".into(), "partial".into()])
        );
        let mut partial = store.contact_mutation("one", "partial").unwrap().operation;
        partial.result["profileProjection"] = serde_json::json!("complete");
        store
            .advance_contact_mutation(&partial, "failed", 0)
            .unwrap();
        assert_eq!(ids(&store), BTreeSet::from(["accepted".into()]));
        let partial = store.contact_mutation("one", "partial").unwrap();
        assert_eq!(partial.phase, "failed");
        assert_eq!(partial.operation.state, "failed");
        assert_eq!(partial.operation.result["partial"], true);
    }
}
