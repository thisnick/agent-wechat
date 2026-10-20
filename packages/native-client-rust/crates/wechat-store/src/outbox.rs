//! Submission and cancellation compete in one transaction before network I/O.
use super::*;
use rusqlite::OptionalExtension;
fn event(tx: &rusqlite::Transaction<'_>, intent: &SendIntent) -> Result<()> {
    tx.execute(
        "INSERT INTO events(kind,data_json,created_at)VALUES('send.updated',?1,?2)",
        params![
            serde_json::to_string(intent)?,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM events WHERE sequence<=?1",
        [seq.saturating_sub(10_000)],
    )?;
    Ok(())
}
impl Store {
    /// Retire the read recovery intent only together with its public event.
    pub fn finish_read_projection(
        &self,
        account: &str,
        thread: &str,
        next: &str,
        intent: &str,
        data: &serde_json::Value,
    ) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let version_key = format!("read-version:{thread}");
            let pending_key = format!("read-pending:{thread}");
            let receipt = format!("event:{intent}");
            let old: Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key=?1",[&version_key],|r|r.get(0)).optional()?;
            let old = old.map(String::from_utf8).transpose()?.unwrap_or_else(||"0".into());
            let completed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key=?1)",[&receipt],|r|r.get(0))?;
            if completed && old == next {return Ok(());}
            let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key=?1)",[&pending_key],|r|r.get(0))?;
            if !pending || old.parse::<u64>()?.checked_add(1).map(|v|v.to_string()).as_deref()!=Some(next) {bail!("read_state_conflict");}
            tx.execute("INSERT INTO runtime VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![version_key,next.as_bytes()])?;
            tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('thread.read-state',?1,?2)",params![serde_json::to_string(data)?,chrono::Utc::now().to_rfc3339()])?;
            let seq=tx.last_insert_rowid();
            tx.execute("INSERT INTO runtime VALUES(?1,?2)",params![receipt,seq.to_string().as_bytes()])?;
            tx.execute("DELETE FROM runtime WHERE key=?1",[pending_key])?;
            tx.execute("DELETE FROM events WHERE sequence<=?1",[seq.saturating_sub(10_000)])?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn pending_read_intents(&self, account: &str) -> Result<Vec<(String, serde_json::Value)>> {
        self.control(account,|db|{
            let mut statement=db.prepare("SELECT substr(key,14),value FROM runtime WHERE key LIKE 'read-pending:%' ORDER BY key LIMIT 100")?;
            let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(|(thread,bytes)|Ok((thread,serde_json::from_slice(&bytes)?))).collect()
        })
    }
    pub fn pending_text_projections(&self, account: &str) -> Result<Vec<String>> {
        self.control(account,|db|{
            let mut statement=db.prepare("SELECT id FROM outbox WHERE state='accepted' AND server_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM runtime WHERE key='send-projected:'||outbox.id) ORDER BY created_at,id LIMIT 100")?;
            let rows=statement.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
    }
    pub fn finish_text_projection(&self, account: &str, id: &str, message_id: &str) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let changed = tx.execute(
                "INSERT OR IGNORE INTO runtime VALUES(?1,?2)",
                params![format!("send-projected:{id}"), message_id.as_bytes()],
            )?;
            if changed == 1 {
                event(&tx, &super::operations::read_send(&tx, account, id)?)?;
            }
            tx.commit()?;
            Ok(())
        })
    }
    pub fn send_by_key(&self, account: &str, key: &str) -> Result<SendIntent> {
        self.control(account, |db| {
            let id: String = db.query_row(
                "SELECT id FROM outbox WHERE idempotency_key=?1",
                [key],
                |r| r.get(0),
            )?;
            super::operations::read_send(db, account, &id)
        })
    }
    pub fn claim_send(&self, account: &str, id: &str) -> Result<bool> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            let changed=tx.execute("UPDATE outbox SET state='submitting' WHERE id=?1 AND state='queued' AND EXISTS(SELECT 1 FROM runtime WHERE key='desired' AND value=CAST('connected' AS BLOB)) AND NOT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')",[id])?;
            if changed==1 {event(&tx,&super::operations::read_send(&tx,account,id)?)?;}
            tx.commit()?;Ok(changed==1)
        })
    }
    pub fn cancel_send(&self, account: &str, id: &str) -> Result<SendIntent> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            let changed=tx.execute("UPDATE outbox SET state='cancelled',error_code=NULL WHERE id=?1 AND state='queued'",[id])?;
            let intent=super::operations::read_send(&tx,account,id)?;
            if changed==1 {event(&tx,&intent)?;}
            tx.commit()?;Ok(intent)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_completion_keeps_intent_until_event_commit_and_replay_is_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        let intent =
            serde_json::json!({"messageIds":["message"],"expectedVersion":"source-version"});
        let next = store.begin_read("a", "peer", "0", &intent).unwrap();
        store.control("a",|db|{db.execute_batch("CREATE TRIGGER reject_read_event BEFORE INSERT ON events WHEN NEW.kind='thread.read-state' BEGIN SELECT RAISE(ABORT,'event unavailable');END;")?;Ok(())}).unwrap();
        assert!(store
            .finish_read_projection(
                "a",
                "peer",
                &next,
                "read:test",
                &serde_json::json!({"messageIds":["message"]})
            )
            .is_err());
        assert_eq!(store.read_version("a", "peer").unwrap(), "0");
        assert_eq!(store.pending_read_intents("a").unwrap()[0].0, "peer");
        store
            .control("a", |db| {
                db.execute_batch("DROP TRIGGER reject_read_event;")?;
                Ok(())
            })
            .unwrap();
        store
            .finish_read_projection("a", "peer", &next, "read:test", &intent)
            .unwrap();
        store
            .finish_read_projection("a", "peer", &next, "read:test", &intent)
            .unwrap();
        assert_eq!(store.read_version("a", "peer").unwrap(), "1");
        assert!(store.pending_read_intents("a").unwrap().is_empty());
        assert_eq!(
            store
                .events("a", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "thread.read-state")
                .count(),
            1
        );
    }
    #[test]
    fn cancellation_and_submission_are_exclusive_and_logout_fences_claim() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        store.put_runtime("a", "desired", b"connected").unwrap();
        let (a, _) = store
            .create_send("a", "first", "filehelper", "hash", b"sealed", "1")
            .unwrap();
        assert_eq!(store.cancel_send("a", &a.id).unwrap().state, "cancelled");
        assert!(!store.claim_send("a", &a.id).unwrap());
        let (b, _) = store
            .create_send("a", "second", "filehelper", "hash", b"sealed", "2")
            .unwrap();
        assert!(store.claim_send("a", &b.id).unwrap());
        assert_eq!(store.cancel_send("a", &b.id).unwrap().state, "submitting");
        assert!(!store.claim_send("a", &b.id).unwrap());
        let (c, _) = store
            .create_send("a", "third", "filehelper", "hash", b"sealed", "3")
            .unwrap();
        store.put_runtime("a", "logged-out", b"1").unwrap();
        assert!(!store.claim_send("a", &c.id).unwrap());
        assert_eq!(store.send_by_key("a", "second").unwrap().id, b.id);
        assert!(store.send_by_key("a", "absent").is_err());
    }
}
