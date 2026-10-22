//! Cursor publication receipts belong to the same durable sidecar transaction as
//! their commit event. Original commands and received cursors are separate from
//! the checkpoint already projected into official files.
use super::*;
use rusqlite::OptionalExtension;
impl Store {
    pub fn native_profile_creation_receipt(&self, account: &str) -> Result<bool> {
        self.control(account, |db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='native-profile-origin') OR EXISTS(SELECT 1 FROM operations WHERE kind='login' AND state='succeeded' AND json_extract(result_json,'$.profile.state')='created')", [], |r| r.get(0))?))
    }
    pub fn pending_cursor_projection(&self, account: &str) -> Result<bool> {
        self.control(account, |db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key IN ('cursor-projection-needed','cursor-publication'))", [], |r| r.get(0))?))
    }
    /// A restored received cursor still requires an ordinary-file checkpoint.
    /// The caller seals and validates the cursor before this atomic sidecar step.
    pub fn restore_received_cursor(&self, account: &str, sealed: &[u8]) -> Result<()> {
        if sealed.is_empty() {
            bail!("invalid received cursor record");
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO runtime VALUES('received-cursor',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value", [sealed])?;
            tx.execute("INSERT INTO runtime VALUES('cursor-projection-needed',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value", [b"1".as_slice()])?;
            tx.commit()?;
            Ok(())
        })
    }
    /// Journal a complete generation only if there are no unapplied/blocked
    /// batches and the received generation has not changed since it was read.
    /// False requests a fresh snapshot; it never overwrites another journal.
    pub fn stage_cursor_projection(
        &self,
        account: &str,
        expected_received: &[u8],
        journal: &[u8],
    ) -> Result<bool> {
        if expected_received.is_empty() || journal.is_empty() {
            bail!("invalid cursor publication");
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            let pending: i64 = tx.query_row(
                "SELECT (SELECT count(*) FROM apply_intents WHERE state!='projected')+(SELECT count(*) FROM message_recalls WHERE projection_state='pending')+(SELECT count(*) FROM outgoing_recalls WHERE phase IN ('queued','submitting','accepted'))",
                [],
                |r| r.get(0),
            )?;
            let received: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT value FROM runtime WHERE key='received-cursor'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime WHERE key='cursor-publication')",
                [],
                |r| r.get(0),
            )?;
            if pending != 0 || received.as_deref() != Some(expected_received) || existing {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO runtime VALUES('cursor-publication',?1)",
                [journal],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }
    /// Retire the exact published journal, checkpoint receipt and commit event
    /// together. Newer received work keeps its own pending marker. A matching
    /// retry after retirement is a no-op, never a second event.
    pub fn finish_cursor_projection(
        &self,
        account: &str,
        hash: &str,
        sealed: &[u8],
        expected_journal: &[u8],
        expected_received: &[u8],
    ) -> Result<bool> {
        if hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || sealed.is_empty()
            || expected_journal.is_empty()
            || expected_received.is_empty()
        {
            bail!("invalid cursor publication");
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            let journal:Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key='cursor-publication'", [], |r| r.get(0)).optional()?;
            if journal.as_deref() != Some(expected_journal) {
                let prior:Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key='projected-cursor-hash'", [], |r| r.get(0)).optional()?;
                if journal.is_none() && prior.as_deref()==Some(hash.as_bytes()) { return Ok(false); }
                bail!("cursor publication generation changed");
            }
            let pending:i64 = tx.query_row("SELECT (SELECT count(*) FROM apply_intents WHERE state!='projected')+(SELECT count(*) FROM message_recalls WHERE projection_state='pending')+(SELECT count(*) FROM outgoing_recalls WHERE phase IN ('queued','submitting','accepted'))", [], |r| r.get(0))?;
            if pending != 0 { bail!("cursor publication has unapplied commands"); }
            let received:Option<Vec<u8>> = tx.query_row("SELECT value FROM runtime WHERE key='received-cursor'", [], |r| r.get(0)).optional()?;
            let current = received.as_deref()==Some(expected_received);
            let received_generation=format!("{:x}",Sha256::digest(expected_received));
            for (key,value) in [("projected-cursor-hash",hash.as_bytes()),("projected-cursor",sealed),("projected-received-generation",received_generation.as_bytes())] {
                tx.execute("INSERT INTO runtime VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value", params![key,value])?;
            }
            tx.execute("DELETE FROM runtime WHERE key='cursor-publication'", [])?;
            if current { tx.execute("DELETE FROM runtime WHERE key='cursor-projection-needed'", [])?; }
            else { tx.execute("INSERT INTO runtime VALUES('cursor-projection-needed',?1)ON CONFLICT(key)DO UPDATE SET value=excluded.value", [b"1".as_slice()])?; }
            tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('sync.committed',?1,?2)", params![serde_json::to_string(&serde_json::json!({"profileCheckpoint":"complete","currentReceivedCheckpoint":current}))?,chrono::Utc::now().to_rfc3339()])?;
            // Match the common event retention window in the same transaction.
            let sequence=tx.last_insert_rowid();
            tx.execute("DELETE FROM events WHERE sequence < ?1",[sequence.saturating_sub(10_000)])?;
            tx.commit()?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        (dir, store)
    }
    fn hash() -> String {
        "ab".repeat(32)
    }
    #[test]
    fn recall_obligations_block_both_cursor_admission_and_retirement() {
        let (_dir, store) = fixture();
        store.restore_received_cursor("one", b"received").unwrap();
        store
            .record_recall("one", "peer", "1", None, None, None, "live")
            .unwrap();
        assert!(!store
            .stage_cursor_projection("one", b"received", b"journal")
            .unwrap());
        store
            .finish_recall_projection("one", "peer", "1", "target_absent")
            .unwrap();
        assert!(store
            .stage_cursor_projection("one", b"received", b"journal")
            .unwrap());
        store.begin_recall_projection("one", "peer", "1").unwrap();
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
            .is_err());
        store
            .finish_recall_projection("one", "peer", "1", "applied")
            .unwrap();
        store.control("one", |db| {
            db.execute_batch("INSERT INTO operations VALUES('recall','message_recall','queued','{}','now','now'); INSERT INTO outgoing_recalls VALUES('recall','target','source','queued',x'01');")?;
            Ok(())
        }).unwrap();
        for phase in ["queued", "submitting", "accepted"] {
            store
                .control("one", |db| {
                    db.execute("UPDATE outgoing_recalls SET phase=?1", [phase])?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(store.pending_message_recalls("one").unwrap(), 1);
            assert!(store
                .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
                .is_err());
            assert_eq!(events(&store), 0);
        }
        store
            .control("one", |db| {
                db.execute("UPDATE outgoing_recalls SET phase='failed'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
            .unwrap());
        assert_eq!(events(&store), 1);
        // A new publication is also refused before it creates a journal.
        store
            .control("one", |db| {
                db.execute("UPDATE outgoing_recalls SET phase='accepted'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(!store
            .stage_cursor_projection("one", b"received", b"next journal")
            .unwrap());
    }
    fn events(store: &Store) -> usize {
        store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "sync.committed")
            .count()
    }
    #[test]
    fn restored_received_cursor_and_pending_marker_survive_restart_together() {
        let (dir, store) = fixture();
        assert!(store.restore_received_cursor("one", b"").is_err());
        store
            .restore_received_cursor("one", b"synthetic sealed received")
            .unwrap();
        drop(store);
        let store = Store::open_existing(&dir.path().join("data")).unwrap();
        assert_eq!(
            store.runtime("one", "received-cursor").unwrap().unwrap(),
            b"synthetic sealed received"
        );
        assert!(store.pending_cursor_projection("one").unwrap());
        assert_eq!(events(&store), 0);
    }
    #[test]
    fn blocked_or_beyond_page_limit_intents_prevent_cursor_publication() {
        let (_dir, store) = fixture();
        store.restore_received_cursor("one", b"received").unwrap();
        for i in 0..101 {
            store
                .stage_sync(
                    "one",
                    &format!("batch-{i:03}"),
                    b"sealed commands",
                    b"received",
                )
                .unwrap();
        }
        for i in 0..100 {
            store.finish_sync("one", &format!("batch-{i:03}")).unwrap();
        }
        store.block_sync("one", "batch-100").unwrap();
        assert!(store.pending_sync("one").unwrap().is_empty());
        assert!(!store
            .stage_cursor_projection("one", b"received", b"journal")
            .unwrap());
        assert!(store
            .runtime("one", "cursor-publication")
            .unwrap()
            .is_none());
        store.finish_sync("one", "batch-100").unwrap();
        assert!(store
            .stage_cursor_projection("one", b"received", b"journal")
            .unwrap());
        // Work arriving after journal creation is also part of the final barrier.
        store
            .stage_sync("one", "later", b"sealed commands", b"later cursor")
            .unwrap();
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
            .is_err());
        assert_eq!(events(&store), 0);
        assert_eq!(
            store.runtime("one", "cursor-publication").unwrap().unwrap(),
            b"journal"
        );
    }
    #[test]
    fn commit_event_and_checkpoint_receipts_roll_back_and_replay_atomically() {
        let (_dir, store) = fixture();
        store.restore_received_cursor("one", b"received").unwrap();
        assert!(store
            .stage_cursor_projection("one", b"received", b"journal")
            .unwrap());
        assert!(!store
            .stage_cursor_projection("one", b"received", b"replacement")
            .unwrap());
        store.control("one",|db|{db.execute_batch("CREATE TRIGGER synthetic_fail_checkpoint BEFORE INSERT ON events WHEN NEW.kind='sync.committed' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")?;Ok(())}).unwrap();
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
            .is_err());
        assert!(store.runtime("one", "projected-cursor").unwrap().is_none());
        assert!(store
            .runtime("one", "projected-received-generation")
            .unwrap()
            .is_none());
        assert!(store
            .runtime("one", "projected-cursor-hash")
            .unwrap()
            .is_none());
        assert!(store.pending_cursor_projection("one").unwrap());
        assert_eq!(events(&store), 0);
        store
            .control("one", |db| {
                db.execute_batch("DROP TRIGGER synthetic_fail_checkpoint")?;
                Ok(())
            })
            .unwrap();
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"wrong journal", b"received")
            .is_err());
        assert!(store
            .finish_cursor_projection("one", &hash(), b"projected", b"journal", b"received")
            .unwrap());
        assert!(!store.pending_cursor_projection("one").unwrap());
        assert_eq!(events(&store), 1);
        assert_eq!(
            store
                .runtime("one", "projected-received-generation")
                .unwrap()
                .unwrap(),
            format!("{:x}", Sha256::digest(b"received")).as_bytes()
        );
        assert!(!store
            .finish_cursor_projection("one", &hash(), b"different nonce", b"journal", b"received")
            .unwrap());
        assert_eq!(
            store.runtime("one", "projected-cursor").unwrap().unwrap(),
            b"projected"
        );
        assert_eq!(events(&store), 1);
    }
    #[test]
    fn generation_cas_keeps_newer_received_checkpoint_pending() {
        let (_dir, store) = fixture();
        store
            .restore_received_cursor("one", b"received-old")
            .unwrap();
        assert!(!store
            .stage_cursor_projection("one", b"stale", b"journal-old")
            .unwrap());
        assert!(store
            .stage_cursor_projection("one", b"received-old", b"journal-old")
            .unwrap());
        store
            .restore_received_cursor("one", b"received-new")
            .unwrap();
        assert!(store
            .finish_cursor_projection(
                "one",
                &hash(),
                b"projected-old",
                b"journal-old",
                b"received-old"
            )
            .unwrap());
        assert!(store.pending_cursor_projection("one").unwrap());
        let event = store
            .events("one", 0, 10)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "sync.committed")
            .unwrap();
        assert_eq!(event.data["currentReceivedCheckpoint"], false);
        assert!(store
            .stage_cursor_projection("one", b"received-new", b"journal-new")
            .unwrap());
        assert!(store
            .finish_cursor_projection(
                "one",
                &hash(),
                b"projected-new",
                b"journal-old",
                b"received-old"
            )
            .is_err());
        assert_eq!(
            store.runtime("one", "cursor-publication").unwrap().unwrap(),
            b"journal-new"
        );
        assert!(store
            .finish_cursor_projection(
                "one",
                &"cd".repeat(32),
                b"projected-new",
                b"journal-new",
                b"received-new"
            )
            .unwrap());
        assert!(!store.pending_cursor_projection("one").unwrap());
        assert_eq!(events(&store), 2);
    }
}
