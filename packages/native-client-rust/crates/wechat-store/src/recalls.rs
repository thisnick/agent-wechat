//! Durable account-local recall identities and their single committed event.
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::json;

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

impl Store {
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
    /// Rechecking a restored official generation is a new local projection
    /// obligation, even when an older generation had completed this marker.
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
