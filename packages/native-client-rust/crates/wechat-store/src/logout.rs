use super::*;

fn event(tx: &rusqlite::Transaction<'_>, kind: &str, value: &Value) -> Result<()> {
    let sequence: i64 =
        tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events", [], |r| {
            r.get(0)
        })?;
    tx.execute(
        "INSERT INTO events VALUES(?1,?2,?3,?4)",
        params![
            sequence,
            kind,
            serde_json::to_string(value)?,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    tx.execute(
        "DELETE FROM events WHERE sequence < ?1",
        [sequence.saturating_sub(10_000)],
    )?;
    Ok(())
}

impl Store {
    /// Commit local suppression and durable operation before any remote action.
    /// Repeating an operation ID never changes the fence or schedules a retry.
    pub fn begin_logout(&self, op: &Operation, epoch: &[u8]) -> Result<(Operation, bool)> {
        if !matches!(op.kind.as_str(), "logout" | "forget_credentials")
            || epoch.is_empty()
            || epoch.len() > 128
        {
            bail!("invalid logout operation");
        }
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            let existing: Option<(String,String,String,String,String)> = tx.query_row("SELECT kind,state,result_json,created_at,updated_at FROM operations WHERE id=?1", [&op.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            if let Some((kind,state,result,created_at,updated_at)) = existing {
                if kind != op.kind { bail!("idempotency_conflict"); }
                return Ok((Operation { id: op.id.clone(), account_id: op.account_id.clone(),kind,state,result:serde_json::from_str(&result)?,created_at,updated_at }, false));
            }
            let pending: Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key='logout-pending'", [], |r| r.get(0)).optional()?;
            if pending.is_some() { bail!("operation_in_progress"); }
            for (key,value) in [("logged-out",epoch),("logout-epoch",epoch),("desired",b"disconnected".as_slice()),("logout-pending",op.id.as_bytes())] {
                tx.execute("INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
            }
            tx.execute("DELETE FROM runtime WHERE key='validated-generation'", [])?;
            let queued = {
                let mut query=tx.prepare("SELECT id FROM outbox WHERE state='queued' ORDER BY created_at,id")?;
                let rows=query.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            tx.execute("UPDATE outbox SET state='needs_review',error_code='logout_before_submission' WHERE state='queued'", [])?;
            for id in queued {crate::outbox::event(&tx,&super::read_send(&tx,&op.account_id,&id)?)?;}
            crate::media::cancel_queued_media_for_logout(&tx,&op.account_id)?;
            tx.execute("INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6)", params![op.id,op.kind,op.state,serde_json::to_string(&op.result)?,op.created_at,op.updated_at])?;
            event(&tx,"account.updated",&serde_json::json!({"accountId":op.account_id,"authentication":"logged_out","autoLoginSuppressed":true}))?;
            event(&tx,"operation.updated",&serde_json::json!({"id":op.id,"kind":op.kind,"state":op.state}))?;
            tx.commit()?;
            Ok((op.clone(),true))
        })
    }
    pub fn forget_authorization(&self, account: &str) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let fenced: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')", [], |r| r.get(0))?;
            if !fenced { bail!("logout fence required before forgetting credentials"); }
            // Device identity, history/cursors and SQLCipher/image references are
            // independent from native authorization and intentionally retained.
            tx.execute("DELETE FROM runtime WHERE key IN ('account-state','validated-generation') OR key LIKE 'qr:%'", [])?;
            tx.commit()?; Ok(())
        })
    }
    pub fn finish_logout(&self, op: &Operation) -> Result<()> {
        self.save_logout_progress(op, true)
    }
    pub fn advance_logout(&self, op: &Operation) -> Result<()> {
        self.save_logout_progress(op, false)
    }
    fn save_logout_progress(&self, op: &Operation, terminal: bool) -> Result<()> {
        self.control(&op.account_id, |db| {
            let tx = db.transaction()?;
            tx.execute(
                "UPDATE operations SET state=?2,result_json=?3,updated_at=?4 WHERE id=?1",
                params![
                    op.id,
                    op.state,
                    serde_json::to_string(&op.result)?,
                    op.updated_at
                ],
            )?;
            if terminal {
                tx.execute(
                    "DELETE FROM runtime WHERE key='logout-pending' AND value=?1",
                    [op.id.as_bytes()],
                )?;
            }
            event(
                &tx,
                "operation.updated",
                &serde_json::json!({"id":op.id,"kind":op.kind,"state":op.state}),
            )?;
            tx.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation(account: &str, id: &str) -> Operation {
        Operation {
            id: id.into(),
            account_id: account.into(),
            kind: "logout".into(),
            state: "queued".into(),
            result: Value::Null,
            created_at: "2026-10-20T00:00:00Z".into(),
            updated_at: "2026-10-20T00:00:00Z".into(),
        }
    }
    #[test]
    fn suppression_epoch_operation_and_outbox_hold_commit_together_and_are_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        for account in ["a", "b"] {
            store
                .put_runtime(account, "account-state", b"synthetic")
                .unwrap();
            store
                .put_runtime(account, "validated-generation", b"generation")
                .unwrap();
        }
        let (send, _) = store
            .create_send("a", "key", "filehelper", "hash", b"synthetic", "1")
            .unwrap();
        let op = operation("a", "logout_1");
        assert!(store.begin_logout(&op, b"epoch1").unwrap().1);
        assert!(!store.begin_logout(&op, b"epoch2").unwrap().1);
        assert_eq!(
            store.runtime("a", "logout-epoch").unwrap().unwrap(),
            b"epoch1"
        );
        assert!(store
            .runtime("a", "validated-generation")
            .unwrap()
            .is_none());
        assert!(store
            .runtime("b", "validated-generation")
            .unwrap()
            .is_some());
        assert_eq!(store.send("a", &send.id).unwrap().state, "needs_review");
        let events = store.events("a", 0, 200).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "send.updated",
                "send.updated",
                "account.updated",
                "operation.updated"
            ]
        );
        assert_eq!(events[0].data["state"], "queued");
        assert_eq!(events[1].data["state"], "needs_review");
    }
    #[test]
    fn logout_transaction_cancels_queued_media_without_changing_inflight_or_other_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        for a in ["a", "b"] {
            store.create_account(a).unwrap();
        }
        let hash = "a".repeat(64);
        for (a, id, key) in [
            ("a", "queued", "send:q"),
            ("a", "inflight", "send:i"),
            ("b", "other", "send:o"),
        ] {
            store
                .create_media_job(a, id, key, &hash, "send", &Value::Null, b"synthetic")
                .unwrap();
        }
        assert!(store.claim_media_job("a", "inflight").unwrap());
        store
            .begin_logout(&operation("a", "logout_1"), b"epoch")
            .unwrap();
        let queued = store.media_job("a", "queued").unwrap();
        assert_eq!(queued.state, "cancelled");
        assert!(queued.cancel_requested);
        assert!(!store.claim_media_job("a", "queued").unwrap());
        assert_eq!(store.media_job("a", "inflight").unwrap().state, "running");
        assert_eq!(store.media_job("b", "other").unwrap().state, "queued");
        assert!(store
            .create_media_job(
                "a",
                "late",
                "send:late",
                &hash,
                "send",
                &Value::Null,
                b"synthetic"
            )
            .is_err());
    }
    #[test]
    fn forget_retains_device_history_cursor_and_uncertain_or_accepted_sends() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        assert!(store.forget_authorization("a").is_err());
        for key in ["device", "account-state", "applied-cursor", "qr:attempt"] {
            store.put_runtime("a", key, b"synthetic").unwrap();
        }
        let (send, _) = store
            .create_send("a", "key", "filehelper", "hash", b"synthetic", "1")
            .unwrap();
        store
            .update_send(
                "a",
                &send.id,
                "accepted",
                Some("18446744073709551614"),
                None,
            )
            .unwrap();
        store
            .begin_logout(&operation("a", "logout_1"), b"epoch")
            .unwrap();
        store.forget_authorization("a").unwrap();
        assert!(store.runtime("a", "account-state").unwrap().is_none());
        assert!(store.runtime("a", "qr:attempt").unwrap().is_none());
        assert!(store.runtime("a", "device").unwrap().is_some());
        assert!(store.runtime("a", "applied-cursor").unwrap().is_some());
        assert_eq!(
            store.send("a", &send.id).unwrap().server_id.as_deref(),
            Some("18446744073709551614")
        );
    }
}
