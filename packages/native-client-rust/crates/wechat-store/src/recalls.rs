//! Operational replay, pending-work and event-dedup tracking for recalls.
//! Settled message attributes remain in the official profile and retained evidence.
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallMarker {
    pub thread_id: String,
    pub server_id: String,
    pub message_id: Option<String>,
    pub source_created_at: Option<i64>,
    pub observed_at: String,
    pub recalled_at: Option<i64>,
    pub origin: String,
    pub projection_state: String,
}

fn marker(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecallMarker> {
    Ok(RecallMarker {
        thread_id: row.get(0)?,
        server_id: row.get(1)?,
        message_id: row.get(2)?,
        source_created_at: row.get(3)?,
        observed_at: row.get(4)?,
        recalled_at: row.get(5)?,
        origin: row.get(6)?,
        projection_state: row.get(7)?,
    })
}
const COLUMNS: &str = "thread_id,server_id,message_id,source_created_at,observed_at,recalled_at,origin,projection_state";

fn control_key(thread: &str, server: &str) -> Result<String> {
    anyhow::ensure!(
        !thread.is_empty()
            && thread.len() <= 512
            && !thread.chars().any(char::is_control)
            && server
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| n.to_string())
                .as_deref()
                == Some(server),
        "invalid_recall_identity"
    );
    Ok(format!(
        "recall-control:{:x}",
        Sha256::digest(serde_json::to_vec(&(thread, server))?)
    ))
}

impl Store {
    /// Look up only the requested page's recall identities in one account DB
    /// read. This is request-local operational evidence, not a new state cache.
    pub fn recalled_targets(
        &self,
        account: &str,
        targets: &[(String, String)],
    ) -> Result<BTreeSet<(String, String)>> {
        anyhow::ensure!(targets.len() <= 200, "invalid_recall_query");
        for (thread, server) in targets {
            control_key(thread, server)?;
        }
        if targets.is_empty() {
            return Ok(BTreeSet::new());
        }
        self.control(account, |db| {
            let placeholders = vec!["(?,?)"; targets.len()].join(",");
            let mut statement = db.prepare(&format!("SELECT thread_id,server_id FROM message_recalls WHERE (thread_id,server_id) IN (VALUES {placeholders})"))?;
            let rows = statement.query_map(rusqlite::params_from_iter(targets.iter().flat_map(|(thread, server)| [thread, server])), |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn recall_marker(
        &self,
        account: &str,
        thread: &str,
        server: &str,
    ) -> Result<Option<RecallMarker>> {
        self.control(account, |db| {
            Ok(db
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM message_recalls WHERE thread_id=?1 AND server_id=?2"
                    ),
                    params![thread, server],
                    marker,
                )
                .optional()?)
        })
    }
    pub fn recall_by_source(
        &self,
        account: &str,
        message: &str,
        created: i64,
    ) -> Result<Option<RecallMarker>> {
        self.control(account, |db| {
            let mut query = db.prepare(&format!("SELECT {COLUMNS} FROM message_recalls WHERE message_id=?1 AND source_created_at=?2 LIMIT 2"))?;
            let rows = query.query_map(params![message,created], marker)?.collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.len()>1 {bail!("recall_source_identity_ambiguous");}
            Ok(rows.into_iter().next())
        })
    }
    pub fn recall_markers(&self, account: &str) -> Result<Vec<RecallMarker>> {
        self.control(account, |db| {
            let mut query = db.prepare(&format!(
                "SELECT {COLUMNS} FROM message_recalls ORDER BY thread_id,server_id"
            ))?;
            let rows = query
                .query_map([], marker)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
    pub fn pending_recall_projections(&self, account: &str) -> Result<usize> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM message_recalls WHERE projection_state='pending'",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize)
        })
    }
    pub fn pending_recall_markers(&self, account: &str) -> Result<Vec<RecallMarker>> {
        self.control(account, |db| {
            let mut query = db.prepare(&format!(
                "SELECT {COLUMNS} FROM message_recalls WHERE projection_state='pending' ORDER BY thread_id,server_id"
            ))?;
            let rows = query.query_map([], marker)?.collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
    }
    /// Track an attempted local recall write until its profile changes finish.
    pub fn begin_recall_projection(&self, account: &str, thread: &str, server: &str) -> Result<()> {
        self.control(account, |db| {
            anyhow::ensure!(db.execute("UPDATE message_recalls SET projection_state='pending' WHERE thread_id=?1 AND server_id=?2",params![thread,server])? == 1,"recall_marker_missing");
            Ok(())
        })
    }
    #[allow(clippy::too_many_arguments)] // One exact target plus its source and observation metadata.
    pub fn record_recall(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        message_id: Option<&str>,
        created: Option<i64>,
        recalled_at: Option<i64>,
        origin: &str,
    ) -> Result<RecallMarker> {
        self.record_recall_with_control(
            account,
            thread,
            server,
            message_id,
            created,
            recalled_at,
            origin,
            None,
        )
    }
    /// Retain the first existing sealed control receipt for replay, not its attributes.
    #[allow(clippy::too_many_arguments)]
    pub fn record_recall_with_control(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        message_id: Option<&str>,
        created: Option<i64>,
        recalled_at: Option<i64>,
        origin: &str,
        control_receipt: Option<&str>,
    ) -> Result<RecallMarker> {
        if control_receipt.is_some_and(|receipt| {
            receipt.is_empty()
                || receipt.len() > 512
                || receipt.chars().any(|c| c.is_control() || c.is_whitespace())
        }) {
            bail!("invalid_recall_control_receipt");
        }
        if thread.is_empty()
            || thread.len() > 512
            || thread.chars().any(char::is_control)
            || server
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| n.to_string())
                .as_deref()
                != Some(server)
            || created.is_some_and(|time| time < 0)
            || recalled_at.is_some_and(|time| time <= 0)
            || origin.len() > 64
            || !origin
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("invalid_recall_identity");
        }
        self.control(account, |db| {
            let tx=db.transaction()?;
            if let Some(receipt) = control_receipt {
                let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key=?1 AND length(value)>0)", [format!("sync-control:{receipt}")], |row| row.get(0))?;
                anyhow::ensure!(exists, "recall_control_receipt_unavailable");
                tx.execute("INSERT INTO runtime(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING", params![control_key(thread, server)?, receipt.as_bytes()])?;
            }
            let now=chrono::Utc::now().to_rfc3339();
            let inserted=tx.execute("INSERT INTO message_recalls(thread_id,server_id,message_id,source_created_at,observed_at,recalled_at,origin) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(thread_id,server_id) DO NOTHING",
                params![thread,server,message_id,created,now,recalled_at,origin])?;
            // A control can precede its ordinary row. Binding that later source
            // does not re-emit the recall or change its observation timestamp.
            if message_id.is_some() {tx.execute("UPDATE message_recalls SET message_id=COALESCE(message_id,?3),source_created_at=COALESCE(source_created_at,?4) WHERE thread_id=?1 AND server_id=?2",params![thread,server,message_id,created])?;}
            let result=tx.query_row(&format!("SELECT {COLUMNS} FROM message_recalls WHERE thread_id=?1 AND server_id=?2"),params![thread,server],marker)?;
            if inserted==1 {
                let seq:i64=tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events",[],|r|r.get(0))?;
                let occurred=recalled_at.and_then(|t|chrono::DateTime::from_timestamp(t,0)).map(|t|t.to_rfc3339());
                let mut data=json!({"threadId":thread,"serverId":server,"observedAt":now,"origin":origin});
                if let Some(id)=message_id {data["messageId"]=json!(id);}
                if let Some(time)=occurred {data["recalledAt"]=json!(time);}
                tx.execute("INSERT INTO events VALUES(?1,'message.recalled',?2,?3)",params![seq,serde_json::to_string(&data)?,now])?;
                tx.execute("DELETE FROM events WHERE sequence<?1",[seq.saturating_sub(10_000)])?;
            }
            crate::outgoing_recall::confirm_recall_in_transaction(&tx, account, thread, server)?;
            tx.commit()?;
            Ok(result)
        })
    }
    /// Operational reference to retained raw bytes; no settled recall text is copied.
    pub fn recall_control_receipt(
        &self,
        account: &str,
        thread: &str,
        server: &str,
    ) -> Result<Option<String>> {
        self.runtime(account, &control_key(thread, server)?)?
            .map(|bytes| {
                let receipt = String::from_utf8(bytes)?;
                anyhow::ensure!(
                    !receipt.is_empty()
                        && receipt.len() <= 512
                        && !receipt.chars().any(|c| c.is_control() || c.is_whitespace()),
                    "invalid_recall_control_receipt"
                );
                Ok(receipt)
            })
            .transpose()
    }
    /// Read exact accepted text receipts; callers verify the sealed request identity.
    pub fn recall_text_send_candidates(
        &self,
        account: &str,
        thread: &str,
        server: &str,
    ) -> Result<Vec<(SendIntent, Vec<u8>)>> {
        control_key(thread, server)?;
        self.control(account, |db| {
            let mut query = db.prepare("SELECT id,length(payload),CASE WHEN length(payload)<=4194304 THEN payload ELSE NULL END FROM outbox WHERE state='accepted' AND destination=?1 AND server_id=?2 ORDER BY created_at DESC,id DESC LIMIT 2")?;
            let mut rows = query.query(params![thread,server])?;
            let mut result = Vec::new();
            let mut total = 0usize;
            while let Some(row) = rows.next()? {
                let length = row.get::<_, usize>(1)?;
                anyhow::ensure!(length <= 4 * 1024 * 1024 && length <= 8 * 1024 * 1024 - total, "recall_send_evidence_limit");
                let id: String = row.get(0)?;
                let projected_length: Option<usize> = db.query_row("SELECT length(value) FROM runtime WHERE key=?1", [format!("send-projected:{id}")], |row| row.get(0)).optional()?;
                anyhow::ensure!(projected_length.is_none_or(|length| length <= 8192), "recall_send_evidence_limit");
                let payload: Vec<u8> = row.get(2)?;
                total += payload.len();
                result.push((crate::operations::read_send(db, account, &id)?, payload));
            }
            Ok(result)
        })
    }
    /// Include completed receipts and accepted notes in partial voice jobs.
    pub fn recall_media_send_candidates(
        &self,
        account: &str,
        thread: &str,
        server: &str,
    ) -> Result<Vec<MediaJobRecord>> {
        control_key(thread, server)?;
        self.control(account, |db| {
            // Oversized or invalid metadata is unknown evidence. Select it as
            // a blocker without parsing or materializing it as a candidate.
            let mut query = db.prepare("SELECT id,length(payload),length(CAST(data_json AS BLOB)) FROM media_jobs WHERE kind='send' AND CASE WHEN length(CAST(data_json AS BLOB))>1048576 THEN 1 WHEN NOT json_valid(data_json) THEN 1 ELSE json_extract(data_json,'$.threadId')=?1 AND ((state='accepted' AND json_extract(data_json,'$.serverMessageId')=?2) OR EXISTS(SELECT 1 FROM json_each(data_json,'$.result.chunks') c WHERE json_extract(c.value,'$.state')='accepted' AND json_extract(c.value,'$.serverMessageId')=?2)) END ORDER BY created_at DESC,id DESC LIMIT 2")?;
            let mut rows = query.query(params![thread,server])?;
            let mut result = Vec::new();
            let mut total = 0usize;
            while let Some(row) = rows.next()? {
                let length = row.get::<_, usize>(1)?;
                anyhow::ensure!(length <= 4 * 1024 * 1024 && length <= 8 * 1024 * 1024 - total && row.get::<_, usize>(2)? <= 1024 * 1024, "recall_send_evidence_limit");
                let id: String = row.get(0)?;
                let job = crate::media::read_job(db, &id)?;
                total += job.payload.len();
                result.push(job);
            }
            Ok(result)
        })
    }
    /// Append revision for a bounded in-memory evidence cache; payloads are immutable.
    pub fn recall_sync_evidence_revision(&self, account: &str) -> Result<i64> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM apply_intents WHERE stream='message'",
                [],
                |row| row.get(0),
            )?)
        })
    }
    /// Inspect a bounded window of already-retained batches without adding retention.
    pub fn recall_sync_evidence(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        const MAX_BATCH: usize = 4 * 1024 * 1024;
        const MAX_TOTAL: usize = 16 * 1024 * 1024;
        self.control(account, |db| {
            let mut query = db.prepare("SELECT id,length(payload),CASE WHEN length(payload)<=4194304 THEN payload ELSE NULL END FROM apply_intents WHERE stream='message' ORDER BY created_at DESC,id DESC LIMIT 256")?;
            let mut rows = query.query([])?;
            let mut result = Vec::new();
            let mut total = 0usize;
            while let Some(row) = rows.next()? {
                let length = row.get::<_, usize>(1)?;
                if length > MAX_BATCH || length > MAX_TOTAL - total { continue; }
                let payload: Vec<u8> = row.get(2)?;
                total += payload.len();
                result.push((row.get(0)?, payload));
            }
            Ok(result)
        })
    }
    pub fn finish_recall_projection(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        state: &str,
    ) -> Result<()> {
        if !matches!(state, "applied" | "target_absent") {
            bail!("invalid_recall_projection_state");
        }
        self.control(account, |db| {
            let tx=db.transaction()?;
            anyhow::ensure!(tx.execute("UPDATE message_recalls SET projection_state=?3 WHERE thread_id=?1 AND server_id=?2",params![thread,server,state])?==1,"recall_marker_missing");
            crate::outgoing_recall::finish_projection_in_transaction(&tx,account,thread,server)?;
            tx.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_pointer_is_atomic_scoped_and_keeps_first_valid_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        for account in ["one", "two"] {
            store.create_account(account).unwrap();
        }
        for receipt in ["first", "later"] {
            store
                .put_runtime("one", &format!("sync-control:{receipt}"), b"sealed control")
                .unwrap();
        }
        store.control("one", |db| {
            db.execute_batch("CREATE TRIGGER fail_recall_event BEFORE INSERT ON events WHEN NEW.kind='message.recalled' BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(store
            .record_recall_with_control(
                "one",
                "peer",
                "12",
                None,
                None,
                None,
                "live",
                Some("first")
            )
            .is_err());
        assert!(store.recall_marker("one", "peer", "12").unwrap().is_none());
        assert!(store
            .recall_control_receipt("one", "peer", "12")
            .unwrap()
            .is_none());
        store
            .control("one", |db| {
                db.execute_batch("DROP TRIGGER fail_recall_event")?;
                Ok(())
            })
            .unwrap();
        store
            .record_recall("one", "peer", "12", None, None, None, "live")
            .unwrap();
        store
            .record_recall_with_control(
                "one",
                "peer",
                "12",
                Some("source"),
                Some(42),
                None,
                "catchup",
                Some("first"),
            )
            .unwrap();
        store
            .record_recall_with_control(
                "one",
                "peer",
                "12",
                None,
                None,
                None,
                "live",
                Some("later"),
            )
            .unwrap();
        assert_eq!(
            store
                .recall_control_receipt("one", "peer", "12")
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert!(store
            .recall_control_receipt("two", "peer", "12")
            .unwrap()
            .is_none());
        assert!(store
            .recall_control_receipt("one", "other", "12")
            .unwrap()
            .is_none());
        assert!(store
            .recall_control_receipt("one", "peer", "13")
            .unwrap()
            .is_none());
        assert_eq!(store.events("one", 0, 100).unwrap().len(), 1);
        let expected = format!("recall-control:{:x}", Sha256::digest(br#"["peer","12"]"#));
        assert_eq!(store.runtime("one", &expected).unwrap().unwrap(), b"first");
        let columns = store
            .control("one", |db| {
                let mut query = db.prepare("PRAGMA table_info(message_recalls)")?;
                let columns = query
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(columns)
            })
            .unwrap();
        assert_eq!(columns.len(), 8);
        assert!(!columns.iter().any(|field| field.contains("sender")
            || field.contains("direction")
            || field.contains("replacement")));
        assert_eq!(
            store
                .control("one", |db| Ok(db.pragma_query_value(
                    None,
                    "user_version",
                    |row| row.get::<_, i64>(0)
                )?))
                .unwrap(),
            10
        );
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        assert_eq!(
            store
                .recall_control_receipt("one", "peer", "12")
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert_eq!(store.events("one", 0, 100).unwrap().len(), 1);
    }
    #[test]
    fn missing_or_malformed_control_receipt_cannot_create_tracking() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "sync-control:empty", b"").unwrap();
        for receipt in [
            "",
            "two receipts",
            "nul\0receipt",
            &"x".repeat(513),
            "missing",
            "empty",
        ] {
            assert!(store
                .record_recall_with_control(
                    "one",
                    "peer",
                    "12",
                    None,
                    None,
                    None,
                    "live",
                    Some(receipt)
                )
                .is_err());
            assert!(store.recall_marker("one", "peer", "12").unwrap().is_none());
            assert!(store
                .recall_control_receipt("one", "peer", "12")
                .unwrap()
                .is_none());
        }
        assert!(store.events("one", 0, 100).unwrap().is_empty());
    }
    #[test]
    fn evidence_reads_only_the_latest_bounded_retained_bytes_and_append_revision() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        for account in ["window", "budget", "other"] {
            store.create_account(account).unwrap();
        }
        assert_eq!(store.recall_sync_evidence_revision("window").unwrap(), 0);
        store
            .control("window", |db| {
                let tx = db.transaction()?;
                for index in 0..258 {
                    tx.execute(
                        "INSERT INTO apply_intents(id,payload,state,created_at) VALUES(?1,?2,'projected','2026-10-23')",
                        params![format!("batch-{index:03}"), b"sealed".as_slice()],
                    )?;
                }
                tx.execute(
                    "UPDATE apply_intents SET payload=?1 WHERE id='batch-257'",
                    [vec![0u8; 4 * 1024 * 1024 + 1]],
                )?;
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        let revision = store.recall_sync_evidence_revision("window").unwrap();
        assert_eq!(revision, 258);
        let evidence = store.recall_sync_evidence("window").unwrap();
        assert_eq!(evidence.len(), 255);
        assert_eq!(evidence.first().unwrap().0, "batch-256");
        assert_eq!(evidence.last().unwrap().0, "batch-002");
        assert!(evidence.iter().all(|(_, payload)| payload == b"sealed"));
        assert!(store.recall_sync_evidence("other").unwrap().is_empty());
        store
            .stage_sync("window", "batch-256", b"replacement ignored", b"cursor")
            .unwrap();
        assert_eq!(
            store.recall_sync_evidence_revision("window").unwrap(),
            revision
        );
        store
            .stage_sync("window", "appended", b"new sealed", b"cursor")
            .unwrap();
        assert!(store.recall_sync_evidence_revision("window").unwrap() > revision);
        store
            .control("budget", |db| {
                let tx = db.transaction()?;
                let payload = vec![1u8; 4 * 1024 * 1024];
                for index in 0..5 {
                    tx.execute(
                        "INSERT INTO apply_intents(id,payload,state,created_at) VALUES(?1,?2,'projected','2026-10-23')",
                        params![format!("batch-{index}"), payload],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        let budget = store.recall_sync_evidence("budget").unwrap();
        assert_eq!(budget.len(), 4);
        assert_eq!(
            budget
                .iter()
                .map(|(_, payload)| payload.len())
                .sum::<usize>(),
            16 * 1024 * 1024
        );
    }
    #[test]
    fn accepted_text_candidates_are_exact_account_scoped_and_bounded_to_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        for account in ["one", "other"] {
            store.create_account(account).unwrap();
        }
        store.control("one", |db| {
            for (id, destination, state, server) in [("one","peer","accepted","42"),("two","peer","accepted","42"),("three","peer","accepted","42"),("foreign-thread","other","accepted","42"),("foreign-server","peer","accepted","43"),("uncertain","peer","needs_review","44")] {
                db.execute("INSERT INTO outbox VALUES(?1,?1,'hash',?2,?3,?4,'client',?5,NULL,'2026-10-23')", params![id,destination,b"sealed request".as_slice(),state,server])?;
            }
            db.execute("INSERT INTO runtime VALUES('send-projected:two',?1)", [b"completed source".as_slice()])?;
            Ok(())
        }).unwrap();
        let candidates = store
            .recall_text_send_candidates("one", "peer", "42")
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .all(|(row, payload)| row.account_id == "one"
                && row.destination == "peer"
                && row.server_id.as_deref() == Some("42")
                && row.state == "accepted"
                && payload == b"sealed request"));
        assert!(candidates
            .iter()
            .any(|(row, _)| row.local_message_id.as_deref() == Some("completed source")));
        assert!(store
            .recall_text_send_candidates("one", "peer", "44")
            .unwrap()
            .is_empty());
        assert!(store
            .recall_text_send_candidates("other", "peer", "42")
            .unwrap()
            .is_empty());
        store
            .put_runtime("one", "send-projected:two", &vec![0u8; 8193])
            .unwrap();
        assert!(store
            .recall_text_send_candidates("one", "peer", "42")
            .is_err());
    }
    fn insert_send_job(
        store: &Store,
        id: &str,
        kind: &str,
        state: &str,
        data: &str,
        payload: &[u8],
    ) {
        store.control("one", |db| {
            db.execute("INSERT INTO media_jobs(id,idempotency_key,request_hash,kind,state,phase,data_json,payload,created_at,updated_at) VALUES(?1,?1,'hash',?2,?3,'completed',?4,?5,'2026-10-23','2026-10-23')", params![id,kind,state,data,payload])?;
            Ok(())
        }).unwrap();
    }
    #[test]
    fn media_candidates_include_completed_and_partial_voice_acceptance_only_in_exact_thread() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        for account in ["one", "other"] {
            store.create_account(account).unwrap();
        }
        insert_send_job(
            &store,
            "completed",
            "send",
            "accepted",
            r#"{"threadId":"peer","serverMessageId":"42","result":{"profileProjection":"complete"}}"#,
            b"sealed plan",
        );
        insert_send_job(
            &store,
            "partial-voice",
            "send",
            "needs_review",
            r#"{"threadId":"peer","result":{"chunks":[{"index":0,"state":"accepted","serverMessageId":"43"},{"index":1,"state":"needs_review","serverMessageId":"44"}]}}"#,
            b"sealed voice plan",
        );
        insert_send_job(
            &store,
            "rejected",
            "send",
            "failed",
            r#"{"threadId":"peer","serverMessageId":"44"}"#,
            b"rejected",
        );
        insert_send_job(
            &store,
            "other-thread",
            "send",
            "accepted",
            r#"{"threadId":"elsewhere","serverMessageId":"42"}"#,
            b"foreign",
        );
        insert_send_job(
            &store,
            "download",
            "download",
            "accepted",
            r#"{"threadId":"peer","serverMessageId":"42"}"#,
            b"download",
        );
        let completed = store
            .recall_media_send_candidates("one", "peer", "42")
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, "completed");
        assert_eq!(completed[0].payload, b"sealed plan");
        let voice = store
            .recall_media_send_candidates("one", "peer", "43")
            .unwrap();
        assert_eq!(voice.len(), 1);
        assert_eq!(voice[0].id, "partial-voice");
        assert!(store
            .recall_media_send_candidates("one", "peer", "44")
            .unwrap()
            .is_empty());
        assert!(store
            .recall_media_send_candidates("other", "peer", "42")
            .unwrap()
            .is_empty());
        for id in ["duplicate-a", "duplicate-b"] {
            insert_send_job(
                &store,
                id,
                "send",
                "accepted",
                r#"{"threadId":"peer","serverMessageId":"42"}"#,
                b"duplicate",
            );
        }
        assert_eq!(
            store
                .recall_media_send_candidates("one", "peer", "42")
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn send_evidence_limits_fail_without_loading_oversized_rows() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        let max = vec![0u8; 4 * 1024 * 1024];
        store.control("one",|db| {
            for id in ["text-a","text-b"] {db.execute("INSERT INTO outbox VALUES(?1,?1,'hash','peer',?2,'accepted','client','42',NULL,'2026-10-23')",params![id,max])?;}
            Ok(())
        }).unwrap();
        assert_eq!(
            store
                .recall_text_send_candidates("one", "peer", "42")
                .unwrap()
                .iter()
                .map(|(_, payload)| payload.len())
                .sum::<usize>(),
            8 * 1024 * 1024
        );
        store
            .control("one", |db| {
                db.execute(
                    "UPDATE outbox SET payload=?1 WHERE id='text-a'",
                    [vec![0u8; 4 * 1024 * 1024 + 1]],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(store
            .recall_text_send_candidates("one", "peer", "42")
            .is_err());
        insert_send_job(
            &store,
            "large-payload",
            "send",
            "accepted",
            r#"{"threadId":"peer","serverMessageId":"42"}"#,
            &vec![0u8; 4 * 1024 * 1024 + 1],
        );
        assert!(store
            .recall_media_send_candidates("one", "peer", "42")
            .is_err());
        store
            .control("one", |db| {
                db.execute("DELETE FROM media_jobs", [])?;
                Ok(())
            })
            .unwrap();
        let oversized = serde_json::to_string(
            &json!({"threadId":"peer","serverMessageId":"42","padding":"x".repeat(1024*1024)}),
        )
        .unwrap();
        insert_send_job(
            &store,
            "large-metadata",
            "send",
            "accepted",
            &oversized,
            b"sealed",
        );
        assert!(store
            .recall_media_send_candidates("one", "peer", "42")
            .is_err());
        store
            .control("one", |db| {
                db.execute("UPDATE media_jobs SET data_json='invalid JSON'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(store
            .recall_media_send_candidates("one", "peer", "42")
            .is_err());
    }
    #[test]
    fn marker_and_event_are_atomic_and_reject_noncanonical_server_identities() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        for server in ["0", "01", "-1", "18446744073709551616"] {
            assert!(store
                .record_recall("one", "peer", server, None, None, None, "live")
                .is_err());
        }
        store.control("one", |db| {
            db.execute_batch("CREATE TRIGGER fail_recall_event BEFORE INSERT ON events WHEN NEW.kind='message.recalled' BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(store
            .record_recall("one", "peer", "1", None, None, None, "live")
            .is_err());
        assert!(store.recall_marker("one", "peer", "1").unwrap().is_none());
        assert!(store.events("one", 0, 100).unwrap().is_empty());
        store
            .control("one", |db| {
                db.execute_batch("DROP TRIGGER fail_recall_event")?;
                Ok(())
            })
            .unwrap();
        store
            .record_recall("one", "peer", "1", None, None, None, "live")
            .unwrap();
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].data.get("messageId").is_none());
        assert!(events[0].data.get("recalledAt").is_none());
        store
            .record_recall(
                "one",
                "peer",
                "2",
                Some("opaque-source"),
                Some(42),
                None,
                "live",
            )
            .unwrap();
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(events[1].data["messageId"], "opaque-source");
        assert!(events[1].data.get("recalledAt").is_none());
    }
    #[test]
    fn recall_survives_restart_is_account_scoped_and_deduplicates_early_controls() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        let first = store
            .record_recall(
                "a",
                "peer",
                "18446744073709551615",
                None,
                None,
                None,
                "live",
            )
            .unwrap();
        let bound = store
            .record_recall(
                "a",
                "peer",
                "18446744073709551615",
                Some("opaque-source"),
                Some(42),
                Some(55),
                "catchup",
            )
            .unwrap();
        assert_eq!(bound.observed_at, first.observed_at);
        assert_eq!(bound.message_id.as_deref(), Some("opaque-source"));
        assert!(store
            .recall_marker("b", "peer", "18446744073709551615")
            .unwrap()
            .is_none());
        assert!(store
            .recall_marker("a", "other", "18446744073709551615")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .events("a", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "message.recalled")
                .count(),
            1
        );
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        assert!(store
            .recall_by_source("a", "opaque-source", 42)
            .unwrap()
            .is_some());
        assert!(store
            .recall_by_source("a", "opaque-source", 43)
            .unwrap()
            .is_none());
        assert_eq!(store.recall_markers("a").unwrap().len(), 1);
    }
}
#[test]
fn page_recall_targets_are_account_and_conversation_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(&temp.path().join("data")).unwrap();
    for account in ["one", "two"] {
        store.create_account(account).unwrap();
    }
    for (account, thread, server) in [
        ("one", "peer", "42"),
        ("one", "other", "43"),
        ("two", "peer", "44"),
    ] {
        store
            .record_recall(account, thread, server, None, None, None, "live")
            .unwrap();
    }
    let targets = vec![
        ("peer".into(), "42".into()),
        ("peer".into(), "43".into()),
        ("peer".into(), "44".into()),
    ];
    assert_eq!(
        store.recalled_targets("one", &targets).unwrap(),
        BTreeSet::from([("peer".into(), "42".into())])
    );
    assert_eq!(
        store.recalled_targets("two", &targets).unwrap(),
        BTreeSet::from([("peer".into(), "44".into())])
    );
    assert!(store.recalled_targets("one", &[]).unwrap().is_empty());
    assert!(store
        .recalled_targets("one", &vec![("peer".into(), "42".into()); 201])
        .is_err());
    assert!(store
        .recalled_targets("one", &[("peer".into(), "01".into())])
        .is_err());
}
