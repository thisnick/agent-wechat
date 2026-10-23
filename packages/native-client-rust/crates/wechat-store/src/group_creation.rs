//! One group-creation attempt; recovery never repeats a possibly accepted request.
use super::*;
use rusqlite::OptionalExtension;

fn authorized(
    tx: &rusqlite::Transaction<'_>,
    authorization: &[u8],
    epoch: Option<&[u8]>,
) -> Result<()> {
    let read = |key: &str| -> Result<Option<Vec<u8>>> {
        Ok(tx
            .query_row("SELECT value FROM runtime WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    };
    if read("logged-out")?.is_some()
        || read("desired")?.as_deref() != Some(b"connected")
        || read("account-state")?.as_deref() != Some(authorization)
        || read("logout-epoch")?.as_deref() != epoch
    {
        bail!("auth_required");
    }
    Ok(())
}
impl Store {
    pub fn admit_group_creation(
        &self,
        operation: &Operation,
        authorization: &[u8],
        epoch: Option<&[u8]>,
    ) -> Result<(Operation, bool)> {
        self.control(&operation.account_id, |db| {
            let tx = db.transaction()?;
            let previous: Option<(String, String, String, String, String)> = tx.query_row(
                "SELECT kind,state,result_json,created_at,updated_at FROM operations WHERE id=?1",
                [&operation.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
            ).optional()?;
            if let Some((kind, state, result, created_at, updated_at)) = previous {
                let result: serde_json::Value = serde_json::from_str(&result)?;
                if kind != "group_create" || result["requestHash"] != operation.result["requestHash"] { bail!("idempotency_conflict"); }
                return Ok((Operation { kind, state, result, created_at, updated_at, ..operation.clone() }, false));
            }
            authorized(&tx, authorization, epoch)?;
            tx.execute("INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6)", params![operation.id,operation.kind,operation.state,serde_json::to_string(&operation.result)?,operation.created_at,operation.updated_at])?;
            tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)", params![serde_json::to_string(&serde_json::json!({"id":operation.id,"kind":operation.kind,"state":operation.state}))?,operation.updated_at])?;
            tx.commit()?;
            Ok((operation.clone(), true))
        })
    }
    pub fn begin_group_submission(
        &self,
        operation: &Operation,
        authorization: &[u8],
        epoch: Option<&[u8]>,
    ) -> Result<()> {
        self.control(&operation.account_id, |db| {
            let tx = db.transaction()?;
            authorized(&tx, authorization, epoch)?;
            if tx.execute("UPDATE operations SET state='submitting',updated_at=?1 WHERE id=?2 AND kind='group_create' AND state='queued'", params![operation.updated_at,operation.id])? != 1 { bail!("group_creation_already_attempted"); }
            tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)", params![serde_json::to_string(&serde_json::json!({"id":operation.id,"kind":"group_create","state":"submitting"}))?,operation.updated_at])?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn recover_group_creations(&self, account: &str) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let pending = {
                let mut query = tx.prepare("SELECT id,state FROM operations WHERE kind='group_create' AND state IN ('queued','submitting')")?;
                let rows = query.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (id, previous) in pending {
                let state = if previous == "submitting" { "needs_review" } else { "failed" };
                let timestamp = chrono::Utc::now().to_rfc3339();
                tx.execute("UPDATE operations SET state=?1,result_json=json_set(result_json,'$.code','operation_interrupted','$.retryable',json('false')),updated_at=?2 WHERE id=?3", params![state,timestamp,id])?;
                tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)", params![serde_json::to_string(&serde_json::json!({"id":id,"kind":"group_create","state":state}))?,timestamp])?;
            }
            tx.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn group_attempt_is_idempotent_fenced_and_uncertain_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        store
            .put_runtime("a", "account-state", b"generation-one")
            .unwrap();
        store.put_runtime("a", "desired", b"connected").unwrap();
        let mut op = Operation {
            id: "group-attempt".into(),
            account_id: "a".into(),
            kind: "group_create".into(),
            state: "queued".into(),
            result: json!({"requestHash":"same-members","retryable":false}),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        assert!(
            store
                .admit_group_creation(&op, b"generation-one", None)
                .unwrap()
                .1
        );
        assert!(
            !store
                .admit_group_creation(&op, b"generation-one", None)
                .unwrap()
                .1
        );
        let mut conflict = op.clone();
        conflict.result["requestHash"] = json!("other-members");
        assert!(store
            .admit_group_creation(&conflict, b"generation-one", None)
            .is_err());
        assert!(store.operation("b", &op.id).is_err());
        store
            .put_runtime("a", "logout-epoch", b"new-epoch")
            .unwrap();
        assert!(store
            .begin_group_submission(&op, b"generation-one", None)
            .is_err());
        assert_eq!(store.operation("a", &op.id).unwrap().state, "queued");
        store.clear_runtime("a", "logout-epoch").unwrap();
        store
            .begin_group_submission(&op, b"generation-one", None)
            .unwrap();
        assert!(store
            .begin_group_submission(&op, b"generation-one", None)
            .is_err());
        store.recover_group_creations("a").unwrap();
        let recovered = store.operation("a", &op.id).unwrap();
        assert_eq!(recovered.state, "needs_review");
        assert_eq!(recovered.result["requestHash"], "same-members");
        assert_eq!(
            store
                .admit_group_creation(&op, b"generation-one", None)
                .unwrap()
                .0
                .state,
            "needs_review"
        );
        op.id = "never-attempted".into();
        store
            .admit_group_creation(&op, b"generation-one", None)
            .unwrap();
        store.recover_group_creations("a").unwrap();
        assert_eq!(store.operation("a", &op.id).unwrap().state, "failed");
    }
}
