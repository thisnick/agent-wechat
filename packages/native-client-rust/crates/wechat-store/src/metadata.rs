//! Recoverable encrypted metadata receipts and post-projection cursor commits.
use crate::*;
use rusqlite::OptionalExtension;
impl Store {
    pub fn stage_metadata(&self, account: &str, id: &str, sealed: &[u8]) -> Result<()> {
        self.control(account, |db| {
            if id.is_empty()
                || id.len() > 100
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                bail!("invalid metadata receipt")
            };
            let tx = db.transaction()?;
            let key = format!("metadata-receipt:{id}");
            let old: Option<Vec<u8>> = tx
                .query_row("SELECT value FROM runtime WHERE key=?1", [&key], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(old) = old {
                if old != sealed {
                    bail!("metadata receipt conflict");
                }
                return Ok(());
            }
            tx.execute(
                "INSERT OR IGNORE INTO runtime VALUES(?1,?2)",
                params![key, sealed],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO runtime VALUES(?1,?2)",
                params![format!("metadata-pending:{id}"), b"1".as_slice()],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn pending_metadata(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account,|db|{let mut q=db.prepare("SELECT substr(p.key,18),r.value FROM runtime p JOIN runtime r ON r.key='metadata-receipt:'||substr(p.key,18) WHERE p.key LIKE 'metadata-pending:%' ORDER BY p.key LIMIT 1000")?;let rows=q.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?;Ok(rows.collect::<rusqlite::Result<_>>()?)})
    }
    pub fn finish_metadata(
        &self,
        account: &str,
        id: &str,
        cursor: Option<(&str, &[u8])>,
        summary: &serde_json::Value,
    ) -> Result<()> {
        self.control(account,|db|{let tx=db.transaction()?;let pending=format!("metadata-pending:{id}");let n=tx.execute("DELETE FROM runtime WHERE key=?1",[&pending])?;if n==0{return Ok(())};if let Some((key,sealed))=cursor{if !matches!(key,"metadata-contacts-cursor"|"metadata-groups-cursor"){bail!("invalid metadata cursor key")};tx.execute("INSERT INTO runtime VALUES(?1,?2)ON CONFLICT(key)DO UPDATE SET value=excluded.value",params![key,sealed])?;}
   let seq:i64=tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events",[],|r|r.get(0))?;tx.execute("INSERT INTO events VALUES(?1,'metadata.changed',?2,?3)",params![seq,serde_json::to_string(summary)?,chrono::Utc::now().to_rfc3339()])?;tx.execute("DELETE FROM events WHERE sequence < ?1",[seq.saturating_sub(10_000)])?;tx.commit()?;Ok(())})
    }
}

impl Store {
    /// Stage a sticker profile write and supersede older unapplied snapshots in
    /// one transaction. Payloads are encrypted by Core before admission.
    pub fn stage_sticker_projection(&self, account: &str, id: &str, payload: &[u8]) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let old: Option<Vec<u8>> = tx.query_row("SELECT payload FROM apply_intents WHERE id=?1", [id], |r| r.get(0)).optional()?;
            if let Some(old) = old {
                if old != payload { bail!("sticker_projection_conflict"); }
                return Ok(());
            }
            tx.execute("INSERT INTO apply_intents(id,payload,state,created_at,stream) VALUES(?1,?2,'pending',?3,'stickers')", params![id,payload,chrono::Utc::now().to_rfc3339()])?;
            tx.execute("INSERT INTO runtime VALUES('sticker-projection-latest',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [id.as_bytes()])?;
            tx.commit()?;
            Ok(())
        })
    }
    /// Read retained sticker write intents without initiating any network work.
    pub fn pending_sticker_projections(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account, |db| {
            let mut query = db.prepare("SELECT id,payload FROM apply_intents WHERE stream='stickers' AND state!='projected' ORDER BY created_at,id LIMIT 1000")?;
            let rows = query.query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
    }
    /// Retire an applied or superseded write intent and emit its collection
    /// revision atomically. Replays do not emit duplicate events.
    pub fn finish_sticker_projection(&self, account: &str, id: &str, revision: &str) -> Result<()> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let changed = tx.execute("UPDATE apply_intents SET state='projected' WHERE id=?1 AND stream='stickers' AND state!='projected'", [id])?;
            if changed != 0 {
                tx.execute("INSERT INTO events(kind,data_json,created_at) VALUES('sticker-collections.changed',?1,?2)", params![serde_json::json!({"collectionId":"saved","revision":revision}).to_string(),chrono::Utc::now().to_rfc3339()])?;
                let sequence = tx.last_insert_rowid();
                tx.execute("DELETE FROM events WHERE sequence < ?1", [sequence.saturating_sub(10_000)])?;
            }
            tx.commit()?;
            Ok(())
        })
    }
}

impl Store {
    /// Stage a complete pack reconciliation without superseding pending Saved
    /// writes. The payload is encrypted by Core; no account request occurs here.
    pub fn stage_sticker_pack_projection(
        &self,
        account: &str,
        id: &str,
        payload: &[u8],
    ) -> Result<()> {
        self.control(account,|db| {
            let tx=db.transaction()?;
            let old: Option<Vec<u8>>=tx.query_row("SELECT payload FROM apply_intents WHERE id=?1",[id],|r|r.get(0)).optional()?;
            if let Some(old)=old {if old!=payload {bail!("sticker_projection_conflict");}return Ok(());}
            tx.execute("INSERT INTO apply_intents(id,payload,state,created_at,stream) VALUES(?1,?2,'pending',?3,'sticker_packs')",params![id,payload,chrono::Utc::now().to_rfc3339()])?;
            tx.execute("INSERT INTO runtime VALUES('sticker-pack-projection-latest',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[id.as_bytes()])?;
            tx.commit()?;Ok(())
        })
    }
    /// Read retained local pack write intents, without acquiring media or resubmitting membership.
    pub fn pending_sticker_pack_projections(
        &self,
        account: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account,|db| {
            let mut query=db.prepare("SELECT id,payload FROM apply_intents WHERE stream='sticker_packs' AND state!='projected' ORDER BY created_at,id LIMIT 1000")?;
            let rows=query.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;Ok(rows)
        })
    }
    /// Retire a pack receipt and emit one collection event in the same transaction.
    pub fn finish_sticker_pack_projection(
        &self,
        account: &str,
        id: &str,
        revision: &str,
    ) -> Result<()> {
        self.control(account,|db| {
            let tx=db.transaction()?;
            if tx.execute("UPDATE apply_intents SET state='projected' WHERE id=?1 AND stream='sticker_packs' AND state!='projected'",[id])? != 0 {
                tx.execute("INSERT INTO events(kind,data_json,created_at) VALUES('sticker-collections.changed',?1,?2)",params![serde_json::json!({"collectionId":"packs","revision":revision}).to_string(),chrono::Utc::now().to_rfc3339()])?;
                let sequence=tx.last_insert_rowid();tx.execute("DELETE FROM events WHERE sequence < ?1",[sequence.saturating_sub(10000)])?;
            }tx.commit()?;Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_metadata_and_applied_cursor_are_separate_atomic_and_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        store
            .stage_metadata("one", "receipt-test", b"ciphertext")
            .unwrap();
        assert_eq!(store.pending_metadata("one").unwrap().len(), 1);
        assert!(store
            .runtime("one", "metadata-groups-cursor")
            .unwrap()
            .is_none());
        store.control("one",|db|{db.execute_batch("CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")?;Ok(())}).unwrap();
        assert!(store
            .finish_metadata(
                "one",
                "receipt-test",
                Some(("metadata-groups-cursor", b"sealed-cursor")),
                &serde_json::json!({"recordCount":0})
            )
            .is_err());
        assert_eq!(store.pending_metadata("one").unwrap().len(), 1);
        assert!(store
            .runtime("one", "metadata-groups-cursor")
            .unwrap()
            .is_none());
        store
            .control("one", |db| {
                db.execute_batch("DROP TRIGGER fail_event")?;
                Ok(())
            })
            .unwrap();
        for _ in 0..2 {
            store
                .finish_metadata(
                    "one",
                    "receipt-test",
                    Some(("metadata-groups-cursor", b"sealed-cursor")),
                    &serde_json::json!({"recordCount":0}),
                )
                .unwrap();
        }
        assert!(store.pending_metadata("one").unwrap().is_empty());
        assert_eq!(
            store.runtime("one", "metadata-groups-cursor").unwrap(),
            Some(b"sealed-cursor".to_vec())
        );
        store
            .stage_metadata("one", "receipt-test", b"ciphertext")
            .unwrap();
        assert!(store.pending_metadata("one").unwrap().is_empty());
        assert_eq!(store.events("one", 0, 100).unwrap().len(), 1);
    }
}
