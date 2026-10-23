//! Durable account-local recall identities and their single committed event.
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::json;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecallMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_text: Option<String>,
}
impl RecallMetadata {
    pub fn validate(&self) -> Result<()> {
        if self.original_sender_id.as_deref().is_some_and(|sender| {
            sender.is_empty()
                || sender.len() > 512
                || sender.chars().any(|c| c.is_control() || c.is_whitespace())
        }) || self
            .original_direction
            .as_deref()
            .is_some_and(|value| !matches!(value, "incoming" | "outgoing"))
            || self.replacement_text.as_deref().is_some_and(|text| {
                text.is_empty()
                    || text.len() > 16 * 1024
                    || text.chars().any(|c| {
                        (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                            || matches!(c, '\u{fffe}' | '\u{ffff}')
                    })
            })
        {
            bail!("invalid_recall_metadata");
        }
        Ok(())
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_sender_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_text: Option<String>,
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
        original_sender_id: row.get(8)?,
        original_direction: row.get(9)?,
        replacement_text: row.get(10)?,
    })
}
const COLUMNS: &str = "thread_id,server_id,message_id,source_created_at,observed_at,recalled_at,origin,projection_state,original_sender_id,original_direction,replacement_text";

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
        self.record_recall_details(
            account,
            thread,
            server,
            message_id,
            created,
            recalled_at,
            origin,
            &RecallMetadata::default(),
        )
    }
    /// Enrich an exact recall without changing its first observation or notice.
    #[allow(clippy::too_many_arguments)]
    pub fn record_recall_details(
        &self,
        account: &str,
        thread: &str,
        server: &str,
        message_id: Option<&str>,
        created: Option<i64>,
        recalled_at: Option<i64>,
        origin: &str,
        metadata: &RecallMetadata,
    ) -> Result<RecallMarker> {
        metadata.validate()?;
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
            let old=tx.query_row(&format!("SELECT {COLUMNS} FROM message_recalls WHERE thread_id=?1 AND server_id=?2"),params![thread,server],marker)?;
            for (known, supplied) in [
                (&old.original_sender_id, &metadata.original_sender_id),
                (&old.original_direction, &metadata.original_direction),
            ] {
                if known.as_ref().zip(supplied.as_ref()).is_some_and(|(a,b)| a != b) {
                    bail!("recall_author_conflict");
                }
            }
            tx.execute("UPDATE message_recalls SET original_sender_id=COALESCE(original_sender_id,?3),original_direction=COALESCE(original_direction,?4),replacement_text=COALESCE(replacement_text,?5) WHERE thread_id=?1 AND server_id=?2",
                params![thread,server,metadata.original_sender_id,metadata.original_direction,metadata.replacement_text])?;
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
                if let Some(sender)=&result.original_sender_id {data["originalSenderId"]=json!(sender);}
                if let Some(direction)=&result.original_direction {data["originalDirection"]=json!(direction);}
                if let Some(text)=&result.replacement_text {data["replacementText"]=json!(text);}
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
    fn v007_upgrade_keeps_existing_unknown_metadata_and_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        let old = store
            .record_recall("one", "peer", "12", Some("source"), Some(42), None, "live")
            .unwrap();
        store.put_runtime("one", "retained", b"opaque").unwrap();
        store.control("one", |db| {
            db.execute_batch("ALTER TABLE message_recalls DROP COLUMN original_sender_id;ALTER TABLE message_recalls DROP COLUMN original_direction;ALTER TABLE message_recalls DROP COLUMN replacement_text;PRAGMA user_version=7;")?;
            Ok(())
        }).unwrap();
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        let migrated = store.recall_marker("one", "peer", "12").unwrap().unwrap();
        assert_eq!(migrated.observed_at, old.observed_at);
        assert_eq!(migrated.message_id, old.message_id);
        assert!(migrated.original_sender_id.is_none());
        assert!(migrated.original_direction.is_none());
        assert!(migrated.replacement_text.is_none());
        assert_eq!(
            store.runtime("one", "retained").unwrap().unwrap(),
            b"opaque"
        );
        assert_eq!(
            store
                .control("one", |db| Ok(db.pragma_query_value(
                    None,
                    "user_version",
                    |row| row.get::<_, i64>(0)
                )?))
                .unwrap(),
            8
        );
        assert_eq!(store.events("one", 0, 100).unwrap().len(), 1);
    }
    #[test]
    fn metadata_enriches_early_marker_without_reemitting_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let first = store
            .record_recall("one", "peer", "12", None, None, None, "live")
            .unwrap();
        let metadata = RecallMetadata {
            original_sender_id: Some("original-author".into()),
            original_direction: Some("outgoing".into()),
            replacement_text: Some("Server notice <text> & detail".into()),
        };
        let enriched = store
            .record_recall_details(
                "one",
                "peer",
                "12",
                Some("source"),
                Some(42),
                None,
                "catchup",
                &metadata,
            )
            .unwrap();
        assert_eq!(enriched.observed_at, first.observed_at);
        assert_eq!(enriched.origin, "live");
        assert_eq!(enriched.original_sender_id, metadata.original_sender_id);
        assert_eq!(enriched.original_direction, metadata.original_direction);
        assert_eq!(enriched.replacement_text, metadata.replacement_text);
        let changed_notice = RecallMetadata {
            replacement_text: Some("Later notice".into()),
            ..Default::default()
        };
        assert_eq!(
            store
                .record_recall_details(
                    "one",
                    "peer",
                    "12",
                    None,
                    None,
                    None,
                    "live",
                    &changed_notice
                )
                .unwrap()
                .replacement_text,
            metadata.replacement_text
        );
        assert!(store.recall_marker("two", "peer", "12").unwrap().is_none());
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].data.get("originalSenderId").is_none());
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        let retained = store
            .recall_by_source("one", "source", 42)
            .unwrap()
            .unwrap();
        assert_eq!(retained.original_sender_id, metadata.original_sender_id);
        assert_eq!(retained.original_direction, metadata.original_direction);
        assert_eq!(retained.replacement_text, metadata.replacement_text);
    }
    #[test]
    fn first_event_has_known_metadata_and_contradictory_author_is_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        let original = RecallMetadata {
            original_sender_id: Some("author".into()),
            original_direction: Some("incoming".into()),
            replacement_text: Some("Server notice".into()),
        };
        let marker = store
            .record_recall_details("one", "peer", "12", None, None, None, "live", &original)
            .unwrap();
        let events = store.events("one", 0, 100).unwrap();
        assert_eq!(events[0].data["originalSenderId"], "author");
        assert_eq!(events[0].data["originalDirection"], "incoming");
        assert_eq!(events[0].data["replacementText"], "Server notice");
        assert!(events[0].data.get("actorId").is_none());
        for conflicting in [
            RecallMetadata {
                original_sender_id: Some("different-author".into()),
                ..Default::default()
            },
            RecallMetadata {
                original_direction: Some("outgoing".into()),
                ..Default::default()
            },
        ] {
            assert!(store
                .record_recall_details(
                    "one",
                    "peer",
                    "12",
                    Some("must-not-bind"),
                    Some(42),
                    None,
                    "live",
                    &conflicting
                )
                .unwrap_err()
                .to_string()
                .contains("recall_author_conflict"));
            let unchanged = store.recall_marker("one", "peer", "12").unwrap().unwrap();
            assert_eq!(unchanged.observed_at, marker.observed_at);
            assert!(unchanged.message_id.is_none());
            assert_eq!(unchanged.original_sender_id, original.original_sender_id);
            assert_eq!(unchanged.original_direction, original.original_direction);
            assert_eq!(store.events("one", 0, 100).unwrap().len(), 1);
        }
    }
    #[test]
    fn malformed_metadata_never_creates_a_marker_or_event() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        for invalid in [
            RecallMetadata {
                original_sender_id: Some(String::new()),
                ..Default::default()
            },
            RecallMetadata {
                original_sender_id: Some("two users".into()),
                ..Default::default()
            },
            RecallMetadata {
                original_sender_id: Some("author\0hidden".into()),
                ..Default::default()
            },
            RecallMetadata {
                original_sender_id: Some("界".repeat(171)),
                ..Default::default()
            },
            RecallMetadata {
                original_direction: Some("self".into()),
                ..Default::default()
            },
            RecallMetadata {
                replacement_text: Some("🧪".repeat(4097)),
                ..Default::default()
            },
            RecallMetadata {
                replacement_text: Some("bad\u{0001}notice".into()),
                ..Default::default()
            },
        ] {
            assert!(store
                .record_recall_details("one", "peer", "12", None, None, None, "live", &invalid)
                .is_err());
            assert!(store.recall_marker("one", "peer", "12").unwrap().is_none());
            assert!(store.events("one", 0, 100).unwrap().is_empty());
        }
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
