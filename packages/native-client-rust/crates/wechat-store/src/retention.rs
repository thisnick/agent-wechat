//! Retention for the account sidecar, `agent/control.sqlite`.
//!
//! Settled messages, contacts and groups live in the official profile. This
//! sidecar keeps operational records: pending work, raw receipts, duplicate
//! detection and idempotency. Without a lifecycle those records only grow, so
//! each family below has one. Cleanup only drops bytes this account already
//! owns. It never sends a request, starts a transfer, writes the profile or
//! advances a cursor.
//!
//! | Family | Kept whole while | Then kept as | Removed |
//! | --- | --- | --- | --- |
//! | Sync batches (`apply_intents`) | pending or blocked; a message batch among the newest 256, which recall evidence and the voice fallback read | batch ID, for duplicate detection | 30 days after arrival |
//! | Text sends (`outbox`) | unfinished; accepted but not yet written to the profile; for a day | state, server ID and key | 90 days; key retired |
//! | Media sends | unfinished or awaiting profile writeback; for a day, and until their upload is released | public job record | 90 days; key retired |
//! | Downloads | always; they hold no payload | - | kept: the key comes from the attachment, so retiring it would block downloading it again |
//! | Sticker jobs | always: finished plans are reopened when a pack is added again | - | kept |
//! | Group and contact changes, recalls | unfinished, awaiting a profile write or contact sync; for a day | operation record and key | 365 days; key retired |
//! | Sync, metadata and media writeback operations | unfinished | - | 30 days after finishing; ID retired |
//! | Calls | active | session and action IDs | kept: calls are rare and IDs guard against reuse |
//! | Duplicate guards in `runtime` (see [`aged_runtime`]) | 30 days, and while they name an unfinished sync batch | - | after 30 days |
//! | Raw replies and call diagnostics in `runtime` | 7 days, for offline investigation | - | after 7 days |
//! | Metadata receipts in `runtime` | while their request is pending | - | once it finishes |
//! | Pagination views | until they expire | - | once expired |
//!
//! Uncertain (`needs_review`) records expire after [`REVIEW_DAYS`] or once
//! more than [`REVIEW_LIMIT`] newer ones exist in the same family. Expiry keeps
//! the record and its unknown outcome but drops its sealed request and reply,
//! so local repair can no longer resolve it and it stops blocking other work.
//!
//! Removal keeps a 16-byte SHA-256 prefix of each idempotency key and record
//! ID in `retired_keys`. Reusing a retired key is refused with
//! `idempotency_key_expired` and reading a retired ID returns
//! `operation_expired`; neither starts new work.
//!
//! Runtime rows have no timestamp. Once a day maintenance inserts a
//! `retention-day:` marker with the next row ID, so a receipt with a lower row
//! ID was written before that day. The newest marker always holds the highest
//! row ID, so later rows never reuse a lower one. A marker is added only for a
//! date after every existing marker, so a clock that moves backward cannot make
//! a recent receipt look old.
use super::*;
use rusqlite::Transaction;
use serde_json::json;
use std::collections::BTreeMap;

/// Message batches whose payloads are kept for recall evidence and the voice
/// fallback. Both readers look at no more than these.
pub const RECENT_MESSAGE_BATCHES: i64 = 256;
/// Days an uncertain record keeps its sealed request and reply.
pub const REVIEW_DAYS: i64 = 7;
/// Uncertain records kept whole per family, newest first.
pub const REVIEW_LIMIT: i64 = 100;
const DAY: i64 = 86_400;
/// Finished sends, jobs and mutations stay whole for inspection this long.
const WHOLE_DAYS: i64 = 1;
const RECEIPT_DAYS: i64 = 30;
/// Raw replies kept for investigating a decoder failure without a new request.
const RAW_DAYS: i64 = 7;
const SEND_RECEIPT_DAYS: i64 = 90;
const MUTATION_RECEIPT_DAYS: i64 = 365;
const MARK_DAYS: i64 = 400;
/// Row changes per step outside the runtime scan.
const STEP_ROWS: i64 = 50;
/// Runtime rows examined per step.
const RUNTIME_SCAN: i64 = 500;
const MARK_PREFIX: &str = "retention-day:";
/// Sorts after every marker key.
const MARK_END: &str = "retention-day;";

/// Retired-key families. Text sends and media sends share one key space.
pub(crate) const SEND_KEY: &str = "send-key";
pub(crate) const SEND_ID: &str = "send";
pub(crate) const MEDIA_KEY: &str = "media-job-key";
pub(crate) const MEDIA_ID: &str = "media-job";
pub(crate) const GROUP_KEY: &str = "group-key";
pub(crate) const CONTACT_KEY: &str = "contact-key";
pub(crate) const RECALL_KEY: &str = "recall-key";
pub(crate) const OPERATION_ID: &str = "operation";
/// Read notifications are submitted once per server boundary.
pub const READ_NOTIFICATION: &str = "read-notification";
/// Operation kinds with no idempotency key or dependent table. Logins are
/// excluded: checkpoint recovery reads the successful login record.
const KEYLESS_OPERATIONS: &str = "'sync','metadata','media-writeback'";
const FINAL_OPERATION_STATES: &str =
    "'cancelled','succeeded','failed','expired','rejected','needs_login','needs_review'";

/// One bounded unit of retention work.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionStep {
    /// Uncertain records whose sealed request and reply were dropped.
    pub expired_reviews: usize,
    /// Finished records reduced to their receipt.
    pub compacted: usize,
    /// Receipts removed, with their keys retired where they had one.
    pub retired: usize,
    /// Runtime receipts and expired pagination views removed.
    pub runtime_removed: usize,
    /// Pass back as `runtime_after` to continue the runtime scan.
    #[serde(skip)]
    pub runtime_after: i64,
    /// Whether another step has work to do.
    #[serde(skip)]
    pub more: bool,
}
impl RetentionStep {
    pub fn add(&mut self, other: &Self) {
        self.expired_reviews += other.expired_reviews;
        self.compacted += other.compacted;
        self.retired += other.retired;
        self.runtime_removed += other.runtime_removed;
    }
}

/// Row count and stored bytes of one table or runtime family.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorageCount {
    pub name: String,
    pub rows: i64,
    pub bytes: i64,
}
/// Size of an account sidecar and the unresolved work that keeps records whole.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    pub file_bytes: i64,
    pub free_bytes: i64,
    pub tables: Vec<StorageCount>,
    /// The largest runtime key families, by stored bytes.
    pub runtime_families: Vec<StorageCount>,
    /// Records retention keeps whole until they finish or expire.
    pub unresolved: BTreeMap<String, i64>,
    pub retired_keys: i64,
}

/// The first 16 bytes of SHA-256 over the family and value.
pub fn key_digest(family: &str, value: &str) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(family.as_bytes());
    hash.update([0]);
    hash.update(value.as_bytes());
    hash.finalize()[..16].to_vec()
}
pub(crate) fn is_retired(db: &Connection, family: &str, value: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM retired_keys WHERE family=?1 AND digest=?2)",
        params![family, key_digest(family, value)],
        |r| r.get(0),
    )?)
}
/// Refuse a request that reuses a retired idempotency key.
pub(crate) fn ensure_key_live(db: &Connection, family: &str, value: &str) -> Result<()> {
    if is_retired(db, family, value)? {
        bail!("idempotency_key_expired");
    }
    Ok(())
}
/// Map a missing record to `operation_expired` when its ID was retired.
pub(crate) fn expired_if_missing<T>(
    db: &Connection,
    family: &str,
    id: &str,
    result: Result<T>,
) -> Result<T> {
    match result {
        Err(error)
            if matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::QueryReturnedNoRows)
            ) && is_retired(db, family, id)? =>
        {
            bail!("operation_expired")
        }
        other => other,
    }
}
fn retire(tx: &Connection, family: &str, value: &str, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO retired_keys VALUES(?1,?2,?3)",
        params![family, key_digest(family, value), now],
    )?;
    Ok(())
}
fn at(now: i64, days: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(now.saturating_sub(days.saturating_mul(DAY)), 0)
        .unwrap_or_default()
}
/// RFC 3339 time `days` before `now`, comparable with stored timestamps.
fn cutoff(now: i64, days: i64) -> String {
    at(now, days).to_rfc3339()
}
fn day_key(now: i64, days: i64) -> String {
    format!("{MARK_PREFIX}{}", at(now, days).format("%Y-%m-%d"))
}
fn operation_event(tx: &Connection, id: &str, kind: &str, state: &str, now: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)",
        params![
            serde_json::to_string(&json!({"id":id,"kind":kind,"state":state}))?,
            now
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "DELETE FROM events WHERE sequence<=?1",
        [seq.saturating_sub(10_000)],
    )?;
    Ok(())
}
fn ids(tx: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut query = tx.prepare(sql)?;
    let rows = query.query_map(args, |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
fn take(budget: &mut i64, n: usize) -> usize {
    *budget -= n as i64;
    n
}

/// Record today's marker unless a marker for today or a later date exists.
/// Markers older than [`MARK_DAYS`] are removed; the newest always remains.
fn mark_day(tx: &Connection, now: i64) -> Result<()> {
    let today = day_key(now, 0);
    tx.execute(
        "INSERT INTO runtime(rowid,key,value) SELECT (SELECT COALESCE(MAX(rowid),0)+1 FROM runtime),?1,x'31' WHERE NOT EXISTS(SELECT 1 FROM runtime WHERE key>=?1 AND key<?2)",
        params![today, MARK_END],
    )?;
    tx.execute(
        "DELETE FROM runtime WHERE key>=?1 AND key<?2",
        params![MARK_PREFIX, day_key(now, MARK_DAYS)],
    )?;
    Ok(())
}

/// Drop the sealed request and reply of uncertain records past the review
/// window or beyond the newest [`REVIEW_LIMIT`] in their family.
fn expire_reviews(tx: &Transaction, account: &str, now: i64, budget: &mut i64) -> Result<usize> {
    let before = cutoff(now, REVIEW_DAYS);
    let stamp = at(now, 0).to_rfc3339();
    let mut expired = 0;
    for id in ids(
        tx,
        "SELECT id FROM (SELECT id,created_at,ROW_NUMBER() OVER (ORDER BY created_at DESC,id DESC) AS n FROM outbox WHERE state='needs_review' AND length(payload)>0) WHERE created_at<=?1 OR n>?2 ORDER BY created_at,id LIMIT ?3",
        params![before, REVIEW_LIMIT, (*budget).max(0)],
    )? {
        tx.execute(
            "UPDATE outbox SET payload=x'',error_code='review_expired' WHERE id=?1",
            [&id],
        )?;
        crate::outbox::event(tx, &crate::operations::read_send(tx, account, &id)?)?;
        expired += take(budget, 1);
    }
    for id in ids(
        tx,
        "SELECT id FROM (SELECT id,updated_at,ROW_NUMBER() OVER (ORDER BY updated_at DESC,id DESC) AS n FROM media_jobs WHERE state='needs_review' AND coalesce(error_code,'')!='review_expired') WHERE updated_at<=?1 OR n>?2 ORDER BY updated_at,id LIMIT ?3",
        params![before, REVIEW_LIMIT, (*budget).max(0)],
    )? {
        // A send keeps its plan until its uploads are released: the plan is
        // the only record of every upload it used.
        tx.execute(
            "UPDATE media_jobs SET payload=CASE WHEN kind='send' THEN payload ELSE x'' END,error_code='review_expired',retryable=0,updated_at=?2 WHERE id=?1",
            params![id, stamp],
        )?;
        crate::media::job_event(tx, account, &crate::media::read_job(tx, &id)?)?;
        expired += take(budget, 1);
    }
    for (table, blobs) in [
        (
            "group_mutations",
            "encrypted_request=x'',encrypted_receipts=x''",
        ),
        (
            "contact_mutations",
            "encrypted_request=x'',encrypted_receipts=x''",
        ),
        ("outgoing_recalls", "encrypted_request=x''"),
    ] {
        let rows = {
            let mut query = tx.prepare(&format!(
                "SELECT id,kind FROM (SELECT o.id AS id,o.kind AS kind,o.updated_at AS ts,ROW_NUMBER() OVER (ORDER BY o.updated_at DESC,o.id DESC) AS n FROM {table} t JOIN operations o ON o.id=t.operation_id WHERE t.phase='needs_review' AND length(t.encrypted_request)>0) WHERE ts<=?1 OR n>?2 ORDER BY ts,id LIMIT ?3"
            ))?;
            let rows = query.query_map(params![before, REVIEW_LIMIT, (*budget).max(0)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, kind) in rows {
            tx.execute(
                &format!("UPDATE {table} SET {blobs} WHERE operation_id=?1"),
                [&id],
            )?;
            tx.execute(
                "UPDATE operations SET state='expired',result_json=json_set(result_json,'$.code','review_expired','$.reviewExpired',json('true')),updated_at=?2 WHERE id=?1",
                params![id, stamp],
            )?;
            operation_event(tx, &id, &kind, "expired", &stamp)?;
            expired += take(budget, 1);
        }
    }
    // Startup recovery already ends every unfinished call; this covers a
    // process that stays up. One unresolved call at most can exist.
    expired += take(
        budget,
        tx.execute(
            "UPDATE call_sessions SET state='ended',local_end_reason='review_expired',updated_at=?1 WHERE state='needs_review' AND updated_at<=?2",
            params![now, now.saturating_sub(REVIEW_DAYS * DAY)],
        )?,
    );
    Ok(expired)
}

/// Reduce finished records to their receipts once nothing reads their sealed
/// bytes any more.
fn compact(tx: &Transaction, now: i64, budget: &mut i64) -> Result<usize> {
    let before = cutoff(now, WHOLE_DAYS);
    let mut compacted = 0;
    // Recall evidence and the voice fallback read only the newest message
    // batches. Projection has already written inline voice to the profile.
    compacted += take(
        budget,
        tx.execute(
            "UPDATE apply_intents SET payload=x'' WHERE id IN (SELECT id FROM apply_intents WHERE state='projected' AND length(payload)>0 AND (stream!='message' OR id NOT IN (SELECT id FROM apply_intents WHERE stream='message' ORDER BY created_at DESC,id DESC LIMIT ?1)) ORDER BY created_at,id LIMIT ?2)",
            params![RECENT_MESSAGE_BATCHES, (*budget).max(0)],
        )?,
    );
    // An accepted text send keeps its text until the profile has it.
    compacted += take(
        budget,
        tx.execute(
            "UPDATE outbox SET payload=x'' WHERE id IN (SELECT id FROM outbox WHERE length(payload)>0 AND created_at<=?1 AND (state IN ('failed','cancelled') OR (state='accepted' AND EXISTS(SELECT 1 FROM runtime WHERE key='send-projected:'||outbox.id))) LIMIT ?2)",
            params![before, (*budget).max(0)],
        )?,
    );
    // The same finished tests that release uploads, once a send's upload is
    // gone: the plan is the only record of every upload it used, and uploads
    // are released before this runs. Sticker jobs keep their plans: finished
    // acquisitions and packs are reopened when a pack is added again.
    compacted += take(
        budget,
        tx.execute(
            "UPDATE media_jobs SET payload=x'' WHERE id IN (SELECT id FROM media_jobs WHERE length(payload)>0 AND retryable=0 AND updated_at<=?1 AND (
                (kind='send' AND (state IN ('failed','cancelled') OR (state='needs_review' AND coalesce(error_code,'')='review_expired') OR (state='accepted' AND coalesce(json_extract(data_json,'$.result.profileProjection'),'') IN ('complete','not_applicable') AND coalesce(json_extract(data_json,'$.result.officialMediaWriteback'),'') IN ('complete','not_applicable'))) AND NOT EXISTS(SELECT 1 FROM media_sources s WHERE s.id=json_extract(media_jobs.data_json,'$.sourceId')))
            ) LIMIT ?2)",
            params![before, (*budget).max(0)],
        )?,
    );
    for (table, blobs, phases) in [
        (
            "group_mutations",
            "encrypted_request=x'',encrypted_receipts=x''",
            "'succeeded','failed','cancelled'",
        ),
        (
            "contact_mutations",
            "encrypted_request=x'',encrypted_receipts=x''",
            "'succeeded','failed','cancelled'",
        ),
        (
            "outgoing_recalls",
            "encrypted_request=x''",
            "'succeeded','failed'",
        ),
    ] {
        compacted += take(
            budget,
            tx.execute(
                &format!("UPDATE {table} SET {blobs} WHERE operation_id IN (SELECT t.operation_id FROM {table} t JOIN operations o ON o.id=t.operation_id WHERE length(t.encrypted_request)>0 AND t.phase IN ({phases}) AND o.updated_at<=?1 AND coalesce(json_extract(o.result_json,'$.profileProjection'),'') NOT IN ('pending','awaiting_contact_sync') LIMIT ?2)"),
                params![before, (*budget).max(0)],
            )?,
        );
    }
    // Ended calls keep their action IDs and outcomes, never their bytes.
    compacted += take(
        budget,
        tx.execute(
            "UPDATE call_actions SET encrypted_request=NULL,encrypted_reply=NULL WHERE rowid IN (SELECT a.rowid FROM call_actions a JOIN call_sessions s ON s.call_id=a.call_id WHERE s.state='ended' AND s.updated_at<=?1 AND (a.encrypted_request IS NOT NULL OR a.encrypted_reply IS NOT NULL) LIMIT ?2)",
            params![now.saturating_sub(WHOLE_DAYS * DAY), (*budget).max(0)],
        )?,
    );
    Ok(compacted)
}

/// Remove compacted receipts past their window, retiring their keys and IDs.
fn retire_receipts(tx: &Transaction, now: i64, budget: &mut i64) -> Result<usize> {
    let mut retired = 0;
    // A batch ID only detects an exact repeat of a batch, which the server
    // sends only after an older cursor. The received cursor is saved with the
    // batch and only moves forward.
    retired += take(
        budget,
        tx.execute(
            "DELETE FROM apply_intents WHERE id IN (SELECT id FROM apply_intents WHERE state='projected' AND length(payload)=0 AND created_at<=?1 LIMIT ?2)",
            params![cutoff(now, RECEIPT_DAYS), (*budget).max(0)],
        )?,
    );
    let sends = {
        let mut query = tx.prepare(
            "SELECT id,idempotency_key FROM outbox WHERE length(payload)=0 AND created_at<=?1 LIMIT ?2",
        )?;
        let rows = query.query_map(
            params![cutoff(now, SEND_RECEIPT_DAYS), (*budget).max(0)],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, key) in sends {
        retire(tx, SEND_KEY, &key, now)?;
        retire(tx, SEND_ID, &id, now)?;
        tx.execute(
            "DELETE FROM runtime WHERE key IN ('send-projected:'||?1,'send-projection:'||?1)",
            [&id],
        )?;
        tx.execute("DELETE FROM outbox WHERE id=?1", [&id])?;
        retired += take(budget, 1);
    }
    let jobs = {
        let mut query = tx.prepare(
            "SELECT id,idempotency_key FROM media_jobs WHERE kind='send' AND length(payload)=0 AND retryable=0 AND state IN ('accepted','failed','cancelled','needs_review') AND updated_at<=?1 LIMIT ?2",
        )?;
        let rows = query.query_map(
            params![cutoff(now, SEND_RECEIPT_DAYS), (*budget).max(0)],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, key) in jobs {
        retire(tx, MEDIA_KEY, &key, now)?;
        if let Some(send) = key.strip_prefix("send:") {
            retire(tx, SEND_KEY, send, now)?;
        }
        retire(tx, MEDIA_ID, &id, now)?;
        tx.execute(
            "DELETE FROM runtime WHERE key IN ('media-projection:'||?1,'sticker-send:'||?1) OR substr(key,1,length('media-projection:'||?1||':'))='media-projection:'||?1||':'",
            [&id],
        )?;
        tx.execute("DELETE FROM media_jobs WHERE id=?1", [&id])?;
        retired += take(budget, 1);
    }
    let old = cutoff(now, MUTATION_RECEIPT_DAYS);
    for (table, family) in [
        ("group_mutations", GROUP_KEY),
        ("contact_mutations", CONTACT_KEY),
    ] {
        let rows = {
            let mut query = tx.prepare(&format!(
                "SELECT t.operation_id,t.key_hash FROM {table} t JOIN operations o ON o.id=t.operation_id WHERE length(t.encrypted_request)=0 AND o.updated_at<=?1 LIMIT ?2"
            ))?;
            let rows = query.query_map(params![old, (*budget).max(0)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, key) in rows {
            retire(tx, family, &key, now)?;
            retire(tx, OPERATION_ID, &id, now)?;
            tx.execute(&format!("DELETE FROM {table} WHERE operation_id=?1"), [&id])?;
            tx.execute("DELETE FROM operations WHERE id=?1", [&id])?;
            retired += take(budget, 1);
        }
    }
    for id in ids(
        tx,
        "SELECT r.operation_id FROM outgoing_recalls r JOIN operations o ON o.id=r.operation_id WHERE length(r.encrypted_request)=0 AND o.updated_at<=?1 LIMIT ?2",
        params![old, (*budget).max(0)],
    )? {
        for key in ids(
            tx,
            "SELECT key_hash FROM recall_keys WHERE operation_id=?1",
            [&id],
        )? {
            retire(tx, RECALL_KEY, &key, now)?;
        }
        retire(tx, OPERATION_ID, &id, now)?;
        tx.execute("DELETE FROM recall_keys WHERE operation_id=?1", [&id])?;
        tx.execute("DELETE FROM outgoing_recalls WHERE operation_id=?1", [&id])?;
        tx.execute("DELETE FROM operations WHERE id=?1", [&id])?;
        retired += take(budget, 1);
    }
    for id in ids(
        tx,
        &format!("SELECT id FROM operations WHERE kind IN ({KEYLESS_OPERATIONS}) AND state IN ({FINAL_OPERATION_STATES}) AND updated_at<=?1 LIMIT ?2"),
        params![cutoff(now, RECEIPT_DAYS), (*budget).max(0)],
    )? {
        retire(tx, OPERATION_ID, &id, now)?;
        tx.execute("DELETE FROM operations WHERE id=?1", [&id])?;
        retired += take(budget, 1);
    }
    Ok(retired)
}

/// How a runtime key is treated by retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRetention {
    /// Kept: a singleton, per-thread or per-contact state, a pending marker,
    /// a held or blocked job, recall evidence or a family not listed here.
    Keep,
    /// Removed once older than this many days.
    Remove(i64),
    /// Removed once its metadata request has finished; nothing reads it then.
    RemoveFinishedMetadata,
    /// Removed once older than this many days, retiring its key so the same
    /// read notification is never submitted twice.
    Retire(&'static str, i64),
}

/// Retention for one runtime key. Duplicate guards cover one batch, command,
/// message, operation or minute and are consulted only while that work runs
/// or shortly after. A key naming an unfinished sync batch is always kept.
pub fn aged_runtime(key: &str) -> RuntimeRetention {
    // Per-command projection receipts and their phase markers.
    const COMMAND: [&str; 9] = [
        "projected:",
        "apply-origin:",
        "contact-fields:projected:",
        "contact-mute:projected:",
        "contact-pin:projected:",
        "partial-thread-entry-read:projected:",
        "partial-thread-exit-read:projected:",
        "friend-message-stage:projected:",
        "friend-attention-stage:projected:",
    ];
    // Once-only event receipts for one message, command, operation or minute.
    const EVENT: [&str; 29] = [
        "message:",
        "remote-thread-read:",
        "control-observed:",
        "control-no-local-effect:",
        "contact:",
        "contact-pin-event:",
        "contact-pin:",
        "contact-delete:",
        "group-fields:",
        "friend-message-event:",
        "friend-attention:",
        "partial-thread-entry-read:",
        "partial-thread-exit-read:",
        "group-refreshed:",
        "group-refresh-held:",
        "received:",
        "blocked-v2:",
        "blocked:",
        "group-members:",
        "group-detail:",
        "read-thread:",
        "contact-mutation-event:",
        "contact-mutation:",
        "thread-pin:",
        "group-mutation:",
        "auto-media-job:",
        "auto-media-unavailable:",
        "worker:",
        "push:",
    ];
    // Raw replies kept for investigation and call diagnostics. No code reads
    // them back.
    const RAW: [&str; 10] = [
        "metadata-wire:",
        "metadata-unavailable:",
        "incoming-group-receipt:",
        "call-host-diagnostic:",
        "call-media-error:",
        "call-media-diagnostic:",
        "call-conference-receive:",
        "call-conference-failure:",
        "call-conference-phase:",
        "call-conference-profile:",
    ];
    if let Some(event) = key.strip_prefix("event:") {
        return if EVENT.iter().any(|prefix| event.starts_with(prefix)) {
            RuntimeRetention::Remove(RECEIPT_DAYS)
        } else {
            RuntimeRetention::Keep
        };
    }
    if COMMAND.iter().any(|p| key.starts_with(p)) {
        RuntimeRetention::Remove(RECEIPT_DAYS)
    } else if RAW.iter().any(|p| key.starts_with(p)) {
        RuntimeRetention::Remove(RAW_DAYS)
    } else if key.starts_with("metadata-receipt:") {
        RuntimeRetention::RemoveFinishedMetadata
    } else if key.starts_with("thread-read-notification:") {
        RuntimeRetention::Retire(READ_NOTIFICATION, RECEIPT_DAYS)
    } else {
        RuntimeRetention::Keep
    }
}

/// Remove metadata receipts whose request finished, and expired pagination
/// views. Neither is read again.
fn remove_finished(tx: &Transaction, now: i64, budget: &mut i64) -> Result<usize> {
    let mut removed = take(
        budget,
        tx.execute(
            "DELETE FROM runtime WHERE rowid IN (SELECT r.rowid FROM runtime r WHERE r.key>='metadata-receipt:' AND r.key<'metadata-receipt;' AND NOT EXISTS(SELECT 1 FROM runtime p WHERE p.key='metadata-pending:'||substr(r.key,18)) LIMIT ?1)",
            [(*budget).max(0)],
        )?,
    );
    removed += take(
        budget,
        tx.execute(
            "DELETE FROM pagination_views WHERE id IN (SELECT id FROM pagination_views WHERE expires_at<=?1 LIMIT ?2)",
            params![now, (*budget).max(0)],
        )?,
    );
    Ok(removed)
}

/// Row ID of the newest day marker at least `days` + 1 old; rows below it
/// were written at least `days` days ago.
fn marker_before(tx: &Connection, now: i64, days: i64) -> Result<Option<i64>> {
    Ok(tx.query_row(
        "SELECT MAX(rowid) FROM runtime WHERE key>=?1 AND key<=?2",
        params![MARK_PREFIX, day_key(now, days + 1)],
        |r| r.get(0),
    )?)
}

/// Scan runtime rows old enough for the shortest window, in row ID order.
/// Returns rows removed, the last row ID examined and whether a full chunk was
/// read.
fn age_runtime(tx: &Transaction, now: i64, after: i64) -> Result<(usize, i64, bool)> {
    let windows: BTreeMap<i64, i64> = [RAW_DAYS, RECEIPT_DAYS]
        .into_iter()
        .filter_map(|days| {
            marker_before(tx, now, days)
                .transpose()
                .map(|m| m.map(|m| (days, m)))
        })
        .collect::<Result<_>>()?;
    let Some(limit) = windows.values().max().copied() else {
        return Ok((0, after, false));
    };
    let old_enough = |rowid: i64, days: i64| windows.get(&days).is_some_and(|m| rowid < *m);
    let unfinished = ids(
        tx,
        "SELECT id FROM apply_intents WHERE state!='projected'",
        [],
    )?;
    let rows = {
        let mut query = tx.prepare(
            "SELECT rowid,key FROM runtime WHERE rowid>?1 AND rowid<?2 ORDER BY rowid LIMIT ?3",
        )?;
        let rows = query.query_map(params![after, limit, RUNTIME_SCAN], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let full = rows.len() as i64 == RUNTIME_SCAN;
    let last = rows.last().map_or(after, |(rowid, _)| *rowid);
    let mut removed = 0;
    for (rowid, key) in rows {
        let remove = match aged_runtime(&key) {
            RuntimeRetention::Keep | RuntimeRetention::RemoveFinishedMetadata => false,
            _ if unfinished.iter().any(|id| key.contains(id.as_str())) => false,
            RuntimeRetention::Remove(days) => old_enough(rowid, days),
            RuntimeRetention::Retire(family, days) => {
                let old = old_enough(rowid, days);
                if old {
                    retire(tx, family, &key, now)?;
                }
                old
            }
        };
        if remove {
            removed += tx.execute("DELETE FROM runtime WHERE rowid=?1", [rowid])?;
        }
    }
    Ok((removed, last, full))
}

impl Store {
    /// Run one bounded unit of retention for an account in one transaction.
    /// Call again with the returned `runtime_after` while `more` is set.
    /// Every step is safe to interrupt: rows move only forward, from whole to
    /// compacted to retired. A read snapshot changes nothing.
    pub fn retention_step(
        &self,
        account: &str,
        now: i64,
        runtime_after: i64,
    ) -> Result<RetentionStep> {
        if self.is_read_snapshot() {
            return Ok(RetentionStep::default());
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            mark_day(&tx, now)?;
            let mut budget = STEP_ROWS;
            let mut step = RetentionStep {
                expired_reviews: expire_reviews(&tx, account, now, &mut budget)?,
                ..Default::default()
            };
            step.compacted = compact(&tx, now, &mut budget)?;
            step.retired = retire_receipts(&tx, now, &mut budget)?;
            let finished = remove_finished(&tx, now, &mut budget)?;
            let (removed, last, full) = age_runtime(&tx, now, runtime_after)?;
            step.runtime_removed = removed + finished;
            step.runtime_after = last;
            step.more = budget <= 0 || full;
            tx.commit()?;
            Ok(step)
        })
    }
    /// Whether a key in this family was retired.
    pub fn is_retired_key(&self, account: &str, family: &str, value: &str) -> Result<bool> {
        self.control(account, |db| is_retired(db, family, value))
    }
    /// Row counts and stored bytes per table and runtime family, and the
    /// unresolved work that keeps records whole. Reads only.
    pub fn storage_usage(&self, account: &str) -> Result<StorageUsage> {
        self.control(account, |db| {
            let pragma = |name: &str| -> Result<i64> {
                Ok(db.pragma_query_value(None, name, |r| r.get(0))?)
            };
            let page = pragma("page_size")?;
            let count = |name: &str, sql: &str| -> Result<StorageCount> {
                let (rows, bytes) = db.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                Ok(StorageCount {
                    name: name.into(),
                    rows,
                    bytes,
                })
            };
            let mut tables = vec![
                count("apply_intents", "SELECT count(*),coalesce(sum(length(payload)+length(id)),0) FROM apply_intents")?,
                count("runtime", "SELECT count(*),coalesce(sum(length(key)+length(value)),0) FROM runtime")?,
                count("events", "SELECT count(*),coalesce(sum(length(data_json)),0) FROM events")?,
                count("operations", "SELECT count(*),coalesce(sum(length(result_json)),0) FROM operations")?,
                count("outbox", "SELECT count(*),coalesce(sum(length(payload)+length(idempotency_key)),0) FROM outbox")?,
                count("media_jobs", "SELECT count(*),coalesce(sum(length(payload)+length(data_json)),0) FROM media_jobs")?,
                count("media_assets", "SELECT count(*),coalesce(sum(length(metadata_json)+length(plan)+coalesce(length(payload),0)),0) FROM media_assets")?,
                count("media_sources", "SELECT count(*),coalesce(sum(length(payload)),0) FROM media_sources")?,
                count("group_mutations", "SELECT count(*),coalesce(sum(length(encrypted_request)+length(encrypted_receipts)),0) FROM group_mutations")?,
                count("contact_mutations", "SELECT count(*),coalesce(sum(length(encrypted_request)+length(encrypted_receipts)),0) FROM contact_mutations")?,
                count("outgoing_recalls", "SELECT count(*),coalesce(sum(length(encrypted_request)),0) FROM outgoing_recalls")?,
                count("message_recalls", "SELECT count(*),0 FROM message_recalls")?,
                count("call_actions", "SELECT count(*),coalesce(sum(coalesce(length(encrypted_request),0)+coalesce(length(encrypted_reply),0)),0) FROM call_actions")?,
                count("pagination_views", "SELECT count(*),coalesce(sum(length(payload)),0) FROM pagination_views")?,
            ];
            tables.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name)));
            let mut runtime_families = {
                let mut query = db.prepare(
                    "SELECT CASE WHEN key LIKE 'event:%' THEN 'event:'||substr(substr(key,7),1,instr(substr(key,7)||':',':')-1) ELSE substr(key,1,instr(key||':',':')-1) END AS family,count(*),sum(length(key)+length(value)) FROM runtime GROUP BY family ORDER BY 3 DESC,1 LIMIT 30",
                )?;
                let rows = query.query_map([], |r| {
                    Ok(StorageCount {
                        name: r.get(0)?,
                        rows: r.get(1)?,
                        bytes: r.get(2)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            runtime_families.truncate(30);
            let mut unresolved = BTreeMap::new();
            for (name, sql) in [
                ("syncBatchesPending", "SELECT count(*) FROM apply_intents WHERE state='pending'"),
                ("syncBatchesBlocked", "SELECT count(*) FROM apply_intents WHERE state='blocked'"),
                ("sendsNeedReview", "SELECT count(*) FROM outbox WHERE state='needs_review' AND length(payload)>0"),
                ("sendsAwaitingProfile", "SELECT count(*) FROM outbox WHERE state='accepted' AND NOT EXISTS(SELECT 1 FROM runtime WHERE key='send-projected:'||outbox.id)"),
                ("mediaJobsNeedReview", "SELECT count(*) FROM media_jobs WHERE state='needs_review' AND coalesce(error_code,'')!='review_expired'"),
                ("mediaJobsUnfinished", "SELECT count(*) FROM media_jobs WHERE state IN ('queued','running') OR retryable=1"),
                ("groupMutationsNeedReview", "SELECT count(*) FROM group_mutations WHERE phase='needs_review' AND length(encrypted_request)>0"),
                ("contactMutationsNeedReview", "SELECT count(*) FROM contact_mutations WHERE phase='needs_review' AND length(encrypted_request)>0"),
                ("contactMutationsAwaitingSync", "SELECT count(*) FROM contact_mutations c JOIN operations o ON o.id=c.operation_id WHERE json_extract(o.result_json,'$.profileProjection')='awaiting_contact_sync'"),
                ("recallsNeedReview", "SELECT count(*) FROM outgoing_recalls WHERE phase='needs_review' AND length(encrypted_request)>0"),
                ("callsUnresolved", "SELECT count(*) FROM call_sessions WHERE state!='ended'"),
            ] {
                unresolved.insert(name.to_owned(), db.query_row(sql, [], |r| r.get(0))?);
            }
            Ok(StorageUsage {
                file_bytes: pragma("page_count")? * page,
                free_bytes: pragma("freelist_count")? * page,
                tables,
                runtime_families,
                unresolved,
                retired_keys: db.query_row("SELECT count(*) FROM retired_keys", [], |r| r.get(0))?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("data")).unwrap();
        store.create_account("a").unwrap();
        (temp, store)
    }
    /// Run steps until none has work left, as maintenance does.
    fn settle(store: &Store, now: i64) -> RetentionStep {
        let mut total = RetentionStep::default();
        let mut after = 0;
        for _ in 0..1000 {
            let step = store.retention_step("a", now, after).unwrap();
            total.add(&step);
            after = step.runtime_after;
            if !step.more {
                return total;
            }
        }
        panic!("retention did not settle");
    }
    fn sql<T>(store: &Store, f: impl FnOnce(&mut Connection) -> Result<T>) -> T {
        store.control("a", f).unwrap()
    }
    fn payload_length(store: &Store, table: &str, column: &str, key: &str, id: &str) -> i64 {
        sql(store, |db| {
            Ok(db.query_row(
                &format!("SELECT length({column}) FROM {table} WHERE {key}=?1"),
                [id],
                |r| r.get(0),
            )?)
        })
    }
    fn error<T>(result: Result<T>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error.to_string(),
        }
    }
    fn operation(id: &str, kind: &str) -> Operation {
        let now = chrono::Utc::now().to_rfc3339();
        Operation {
            id: id.into(),
            account_id: "a".into(),
            kind: kind.into(),
            state: "queued".into(),
            result: json!({}),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[test]
    fn sync_batches_keep_the_recent_window_and_their_ids() {
        let (_temp, store) = fixture();
        let total = RECENT_MESSAGE_BATCHES as usize + 20;
        let start = unix_seconds() - 10_000;
        for index in 0..total {
            let id = format!("batch-{index:04}");
            store
                .stage_sync("a", &id, b"sealed batch", b"cursor")
                .unwrap();
            store.finish_sync("a", &id).unwrap();
            // Distinct, ordered arrival times.
            sql(&store, |db| {
                db.execute(
                    "UPDATE apply_intents SET created_at=?2 WHERE id=?1",
                    params![id, at(start + index as i64, 0).to_rfc3339()],
                )?;
                Ok(())
            });
        }
        store
            .stage_sync("a", "unfinished", b"sealed batch", b"cursor")
            .unwrap();
        store
            .stage_contact_sync("a", "contact:done", b"sealed contact batch", b"cursor")
            .unwrap();
        store.finish_sync("a", "contact:done").unwrap();
        let now = unix_seconds();
        let step = settle(&store, now);
        // The unfinished batch is the newest and takes one place in the
        // window; the finished contact batch is compacted too.
        assert_eq!(step.compacted, total - RECENT_MESSAGE_BATCHES as usize + 2);
        // The newest batches stay whole; recall evidence and the voice
        // fallback read exactly those.
        let whole: i64 = sql(&store, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM apply_intents WHERE stream='message' AND length(payload)>0",
                [],
                |r| r.get(0),
            )?)
        });
        assert_eq!(whole, RECENT_MESSAGE_BATCHES);
        assert_eq!(
            store.media_sync_receipts("a").unwrap().len(),
            RECENT_MESSAGE_BATCHES as usize
        );
        assert_eq!(
            store.recall_sync_evidence("a").unwrap(),
            store.media_sync_receipts("a").unwrap()
        );
        assert_eq!(
            payload_length(&store, "apply_intents", "payload", "id", "batch-0000"),
            0
        );
        assert_eq!(
            payload_length(&store, "apply_intents", "payload", "id", "contact:done"),
            0
        );
        // Unfinished work is never touched.
        assert_eq!(store.pending_sync("a").unwrap()[0].1, b"sealed batch");
        // A compacted batch still detects an exact repeat.
        assert!(!store
            .stage_sync_if_new("a", "batch-0000", b"sealed batch", b"cursor")
            .unwrap());
        // After the receipt window only the compacted IDs go.
        settle(&store, now + 31 * DAY);
        let left: i64 = sql(&store, |db| {
            Ok(db.query_row("SELECT count(*) FROM apply_intents", [], |r| r.get(0))?)
        });
        assert_eq!(left, RECENT_MESSAGE_BATCHES);
        assert_eq!(store.pending_sync("a").unwrap().len(), 1);
    }

    #[test]
    fn sends_keep_their_text_until_the_profile_has_it_then_retire_the_key() {
        let (_temp, store) = fixture();
        let (send, _) = store
            .create_send("a", "text-key", "peer", "hash", b"sealed text", "client")
            .unwrap();
        store
            .update_send("a", &send.id, "accepted", Some("100"), None)
            .unwrap();
        let now = unix_seconds();
        // Accepted but not yet written to the profile: kept whole however old.
        settle(&store, now + 400 * DAY);
        assert_eq!(
            store.pending_text_projections("a").unwrap(),
            std::slice::from_ref(&send.id)
        );
        assert_eq!(store.send_payload("a", &send.id).unwrap(), b"sealed text");
        store
            .finish_text_projection("a", &send.id, "local-1")
            .unwrap();
        settle(&store, now);
        assert_eq!(store.send_payload("a", &send.id).unwrap(), b"sealed text");
        settle(&store, now + 2 * DAY);
        assert!(store.send_payload("a", &send.id).unwrap().is_empty());
        // The receipt still answers a replay with the original send.
        let replay = store.send_by_key("a", "text-key").unwrap();
        assert_eq!(replay.id, send.id);
        assert_eq!(replay.local_message_id.as_deref(), Some("local-1"));
        settle(&store, now + 91 * DAY);
        assert_eq!(
            error(store.send_by_key("a", "text-key")),
            "idempotency_key_expired"
        );
        assert_eq!(
            error(store.create_send("a", "text-key", "peer", "hash", b"new", "client")),
            "idempotency_key_expired"
        );
        assert_eq!(
            error(store.create_media_job(
                "a",
                "job",
                "send:text-key",
                &"0".repeat(64),
                "send",
                &json!({}),
                b"plan"
            )),
            "idempotency_key_expired"
        );
        assert_eq!(error(store.send("a", &send.id)), "operation_expired");
        assert!(store
            .runtime("a", &format!("send-projected:{}", send.id))
            .unwrap()
            .is_none());
        // An unknown key and ID are still ordinary misses.
        assert!(!error(store.send_by_key("a", "other")).contains("expired"));
        assert!(!error(store.send("a", "other")).contains("expired"));
    }

    #[test]
    fn uncertain_records_expire_after_seven_days_or_beyond_the_newest_hundred() {
        let (_temp, store) = fixture();
        let mut ids = Vec::new();
        for index in 0..=REVIEW_LIMIT {
            let (send, _) = store
                .create_send(
                    "a",
                    &format!("key-{index}"),
                    "peer",
                    "hash",
                    b"sealed",
                    "client",
                )
                .unwrap();
            store
                .update_send(
                    "a",
                    &send.id,
                    "needs_review",
                    None,
                    Some("restart_after_submission"),
                )
                .unwrap();
            sql(&store, |db| {
                db.execute(
                    "UPDATE outbox SET created_at=?2 WHERE id=?1",
                    params![
                        send.id,
                        format!("2099-01-01T00:{:02}:{:02}+00:00", index / 60, index % 60)
                    ],
                )?;
                Ok(())
            });
            ids.push(send.id);
        }
        let now = unix_seconds();
        // Only the oldest is beyond the newest hundred.
        assert_eq!(settle(&store, now).expired_reviews, 1);
        let oldest = store.send("a", &ids[0]).unwrap();
        assert_eq!(oldest.state, "needs_review");
        assert_eq!(oldest.error_code.as_deref(), Some("review_expired"));
        assert!(store.send_payload("a", &ids[0]).unwrap().is_empty());
        assert_eq!(store.send_payload("a", &ids[1]).unwrap(), b"sealed");
        // The rest expire once a week old; the key still replays the record.
        sql(&store, |db| {
            db.execute("UPDATE outbox SET created_at=?1", [cutoff(now, 10)])?;
            Ok(())
        });
        assert_eq!(settle(&store, now).expired_reviews, REVIEW_LIMIT as usize);
        assert_eq!(store.send_by_key("a", "key-5").unwrap().id, ids[5]);
    }

    fn contact_mutation(store: &Store, id: &str, key: &str, hash: &str) {
        store
            .create_contact_mutation(
                &operation(id, "contact_mutation"),
                key,
                hash,
                b"sealed request",
                b"sealed receipts",
            )
            .unwrap();
    }
    fn set_phase(store: &Store, id: &str, phase: &str, state: &str, projection: &str) {
        sql(store, |db| {
            db.execute(
                "UPDATE contact_mutations SET phase=?2 WHERE operation_id=?1",
                params![id, phase],
            )?;
            db.execute(
                "UPDATE operations SET state=?2,result_json=json_object('profileProjection',?3) WHERE id=?1",
                params![id, state, projection],
            )?;
            Ok(())
        });
    }

    #[test]
    fn replaying_an_old_deletion_key_after_newer_history_never_admits_work() {
        let (_temp, store) = fixture();
        let old_hash = "1".repeat(64);
        contact_mutation(&store, "delete-op", "delete-friend", &old_hash);
        set_phase(&store, "delete-op", "succeeded", "succeeded", "complete");
        // Newer history for the same contact, still unfinished.
        contact_mutation(&store, "add-op", "add-friend", &"2".repeat(64));
        // A finished change still awaiting the contact sync is also kept.
        contact_mutation(&store, "await-op", "await-sync", &"3".repeat(64));
        set_phase(
            &store,
            "await-op",
            "succeeded",
            "succeeded",
            "awaiting_contact_sync",
        );
        let now = unix_seconds();
        settle(&store, now + 2 * DAY);
        assert_eq!(
            payload_length(
                &store,
                "contact_mutations",
                "encrypted_request",
                "operation_id",
                "delete-op"
            ),
            0
        );
        // The compacted receipt replays the original operation.
        let replay = store
            .contact_mutation_by_key("a", "delete-friend", Some(&old_hash))
            .unwrap()
            .unwrap();
        assert_eq!(replay.operation.id, "delete-op");
        settle(&store, now + 366 * DAY);
        assert_eq!(
            error(store.contact_mutation_by_key("a", "delete-friend", Some(&old_hash))),
            "idempotency_key_expired"
        );
        assert_eq!(
            error(store.create_contact_mutation(
                &operation("replayed-op", "contact_mutation"),
                "delete-friend",
                &old_hash,
                b"sealed request",
                b"sealed receipts",
            )),
            "idempotency_key_expired"
        );
        assert_eq!(
            error(store.operation("a", "delete-op")),
            "operation_expired"
        );
        assert!(store.operation("a", "replayed-op").is_err());
        // Newer and awaiting records are untouched.
        let newer = store.contact_mutation("a", "add-op").unwrap();
        assert_eq!(newer.phase, "queued");
        assert_eq!(newer.encrypted_request, b"sealed request");
        assert_eq!(
            store
                .contact_mutation("a", "await-op")
                .unwrap()
                .encrypted_request,
            b"sealed request"
        );
    }

    #[test]
    fn expired_reviews_stop_blocking_and_leave_repair() {
        let (_temp, store) = fixture();
        contact_mutation(&store, "uncertain", "uncertain-key", &"4".repeat(64));
        set_phase(
            &store,
            "uncertain",
            "needs_review",
            "needs_review",
            "pending",
        );
        let now = unix_seconds();
        settle(&store, now + 6 * DAY);
        assert_eq!(store.active_contact_mutations("a", None).unwrap().len(), 1);
        assert_eq!(store.projecting_contact_mutations("a").unwrap().len(), 1);
        let step = settle(&store, now + 8 * DAY);
        assert_eq!(step.expired_reviews, 1);
        assert!(store
            .active_contact_mutations("a", None)
            .unwrap()
            .is_empty());
        assert!(store.projecting_contact_mutations("a").unwrap().is_empty());
        let expired = store.operation("a", "uncertain").unwrap();
        assert_eq!(expired.state, "expired");
        assert_eq!(expired.result["code"], "review_expired");
        let record = store.contact_mutation("a", "uncertain").unwrap();
        assert_eq!(record.phase, "needs_review");
        assert!(record.encrypted_request.is_empty() && record.encrypted_receipts.is_empty());
        assert!(store
            .events("a", 0, 200)
            .unwrap()
            .iter()
            .any(|e| e.kind == "operation.updated" && e.data["state"] == "expired"));
    }

    #[test]
    fn runtime_receipts_age_out_by_day_markers_and_keep_unfinished_work() {
        let (_temp, store) = fixture();
        let now = unix_seconds();
        let notification = format!("thread-read-notification:{}", "a".repeat(64));
        store
            .stage_sync("a", "open-batch", b"sealed", b"cursor")
            .unwrap();
        for key in [
            "projected:done-batch:0",
            "projected:done-batch:0:message-started",
            "event:message:m1",
            "metadata-wire:x",
            "metadata-receipt:running",
            "metadata-pending:running",
            "read-version:thread",
            "sync-control:projected:done-batch:1",
            "event:profile-recovery-pending",
            "projected:open-batch:0",
            &notification,
        ] {
            store.put_runtime("a", key, b"1").unwrap();
        }
        // A finished metadata receipt goes at once; nothing else is old
        // enough yet, and the first pass marks the day.
        store
            .put_runtime("a", "metadata-receipt:finished", b"1")
            .unwrap();
        assert_eq!(settle(&store, now).runtime_removed, 1);
        assert!(store
            .runtime("a", "metadata-receipt:finished")
            .unwrap()
            .is_none());
        store.put_runtime("a", "event:message:m2", b"2").unwrap();
        // Raw replies are kept a week, duplicate guards a month.
        assert_eq!(settle(&store, now + 9 * DAY).runtime_removed, 1);
        assert!(store.runtime("a", "metadata-wire:x").unwrap().is_none());
        assert!(store.runtime("a", "event:message:m1").unwrap().is_some());
        let step = settle(&store, now + 32 * DAY);
        assert_eq!(step.runtime_removed, 4);
        let kept = |key: &str| store.runtime("a", key).unwrap().is_some();
        for key in [
            "projected:done-batch:0",
            "projected:done-batch:0:message-started",
            "event:message:m1",
            notification.as_str(),
        ] {
            assert!(!kept(key), "{key} should age out");
        }
        for key in [
            "metadata-receipt:running",
            "read-version:thread",
            "sync-control:projected:done-batch:1",
            "event:profile-recovery-pending",
            "projected:open-batch:0",
            // Written after the first marker, so younger than the window.
            "event:message:m2",
        ] {
            assert!(kept(key), "{key} should be kept");
        }
        // The read notification for that boundary is never submitted again.
        assert!(store
            .is_retired_key("a", READ_NOTIFICATION, &notification)
            .unwrap());
        assert_eq!(
            store
                .begin_read_notification("a", &notification, b"sealed", None)
                .err()
                .unwrap()
                .to_string(),
            "idempotency_key_expired"
        );
    }

    #[test]
    fn expired_pagination_views_are_removed_without_waiting_for_a_new_one() {
        let (_temp, store) = fixture();
        let now = unix_seconds();
        store
            .save_pagination_view("a", "old", "hash", "0", now + 60, &[1; 1024])
            .unwrap();
        store
            .save_pagination_view("a", "live", "hash", "0", now + 3600, &[1; 1024])
            .unwrap();
        assert_eq!(settle(&store, now + 120).runtime_removed, 1);
        assert!(store.pagination_view("a", "old").is_err());
        assert!(store.pagination_view("a", "live").is_ok());
    }

    #[test]
    fn a_clock_that_moves_backward_never_makes_recent_receipts_look_old() {
        let (_temp, store) = fixture();
        let now = unix_seconds();
        settle(&store, now);
        // The clock jumps ahead, then back to a date with no marker yet.
        settle(&store, now + 40 * DAY);
        store
            .put_runtime("a", "event:message:recent", b"1")
            .unwrap();
        settle(&store, now + 2 * DAY);
        settle(&store, now + 45 * DAY);
        assert!(store
            .runtime("a", "event:message:recent")
            .unwrap()
            .is_some());
        assert!(store
            .runtime("a", &day_key(now + 2 * DAY, 0))
            .unwrap()
            .is_none());
    }

    #[test]
    fn day_markers_keep_later_row_ids_above_them() {
        let (_temp, store) = fixture();
        let now = unix_seconds();
        store.put_runtime("a", "event:message:old", b"1").unwrap();
        settle(&store, now);
        // Removing every row above the marker must not let a new row take a
        // lower row ID than the marker.
        store.clear_runtime("a", "event:message:old").unwrap();
        store.put_runtime("a", "event:message:new", b"1").unwrap();
        settle(&store, now + 32 * DAY);
        assert!(store.runtime("a", "event:message:new").unwrap().is_some());
    }

    #[test]
    fn a_failed_step_changes_nothing_and_the_next_one_finishes() {
        let (_temp, store) = fixture();
        let (send, _) = store
            .create_send("a", "key", "peer", "hash", b"sealed", "client")
            .unwrap();
        store
            .update_send("a", &send.id, "failed", None, Some("rejected"))
            .unwrap();
        for index in 0..RECENT_MESSAGE_BATCHES + 5 {
            let id = format!("batch-{index}");
            store
                .stage_sync("a", &id, b"sealed batch", b"cursor")
                .unwrap();
            store.finish_sync("a", &id).unwrap();
        }
        sql(&store, |db| {
            db.execute_batch("CREATE TRIGGER interrupt BEFORE DELETE ON outbox BEGIN SELECT RAISE(ABORT,'interrupted'); END;")?;
            Ok(())
        });
        let now = unix_seconds();
        assert!(store.retention_step("a", now + 100 * DAY, 0).is_err());
        // The whole step rolled back, including compaction done before the
        // failing statement.
        assert_eq!(store.send_payload("a", &send.id).unwrap(), b"sealed");
        let compacted: i64 = sql(&store, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM apply_intents WHERE length(payload)=0",
                [],
                |r| r.get(0),
            )?)
        });
        assert_eq!(compacted, 0);
        sql(&store, |db| {
            db.execute_batch("DROP TRIGGER interrupt;")?;
            Ok(())
        });
        settle(&store, now + 100 * DAY);
        assert_eq!(
            store.send("a", &send.id).err().unwrap().to_string(),
            "operation_expired"
        );
    }

    #[test]
    fn compaction_returns_freed_pages_and_switches_to_incremental_mode() {
        let (temp, store) = fixture();
        let database = temp.path().join("data/accounts/a/agent/control.sqlite");
        let large = vec![7_u8; 64 * 1024];
        for index in 0..RECENT_MESSAGE_BATCHES + 400 {
            let id = format!("batch-{index:04}");
            store.stage_sync("a", &id, &large, b"cursor").unwrap();
            store.finish_sync("a", &id).unwrap();
        }
        let before = fs::metadata(&database).unwrap().len();
        settle(&store, unix_seconds());
        assert!(store.compact_control("a").unwrap());
        let after = fs::metadata(&database).unwrap().len();
        assert!(after < before / 2, "{before} -> {after}");
        let mode: i64 = sql(&store, |db| {
            Ok(db.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?)
        });
        assert_eq!(mode, 2);
        // Later passes release pages without rewriting the file.
        for index in 0..200 {
            let id = format!("later-{index:04}");
            store.stage_sync("a", &id, &large, b"cursor").unwrap();
            store.finish_sync("a", &id).unwrap();
        }
        let grown = fs::metadata(&database).unwrap().len();
        settle(&store, unix_seconds());
        let free = |store: &Store| -> i64 {
            sql(store, |db| {
                Ok(db.pragma_query_value(None, "freelist_count", |r| r.get(0))?)
            })
        };
        assert!(free(&store) > 0);
        assert!(store.compact_control("a").unwrap());
        assert_eq!(free(&store), 0, "incremental vacuum left pages");
        assert!(fs::metadata(&database).unwrap().len() < grown);
        assert!(!store.compact_control("a").unwrap());
    }

    #[test]
    fn send_jobs_keep_their_plan_until_their_upload_is_released() {
        let (_temp, store) = fixture();
        let hash = "0".repeat(64);
        for (id, state) in [("done", "accepted"), ("uncertain", "needs_review")] {
            store
                .create_media_source(
                    "a",
                    &format!("upload-{id}"),
                    "a.bin",
                    "application/octet-stream",
                    4,
                    &hash,
                    b"data",
                )
                .unwrap();
            store
                .create_media_job(
                    "a",
                    id,
                    &format!("send:{id}"),
                    &hash,
                    "send",
                    &json!({"sourceId":format!("upload-{id}"),"result":{"profileProjection":"complete","officialMediaWriteback":"complete"}}),
                    b"sealed plan",
                )
                .unwrap();
            sql(&store, |db| {
                db.execute(
                    "UPDATE media_jobs SET state=?2,retryable=0,updated_at=?3 WHERE id=?1",
                    params![id, state, cutoff(unix_seconds(), 8)],
                )?;
                Ok(())
            });
        }
        let now = unix_seconds();
        settle(&store, now);
        // The uncertain job expired but, like the finished one, keeps the
        // plan that names its uploads.
        let uncertain = store.media_job("a", "uncertain").unwrap();
        assert_eq!(uncertain.error_code.as_deref(), Some("review_expired"));
        for id in ["done", "uncertain"] {
            assert_eq!(store.media_job("a", id).unwrap().payload, b"sealed plan");
            assert!(store
                .delete_media_source("a", &format!("upload-{id}"))
                .unwrap());
        }
        settle(&store, now + 2 * DAY);
        for id in ["done", "uncertain"] {
            assert!(store.media_job("a", id).unwrap().payload.is_empty());
        }
        settle(&store, now + 92 * DAY);
        assert_eq!(error(store.media_job("a", "done")), "operation_expired");
        assert_eq!(
            error(store.media_job_by_key("a", "send:done")),
            "idempotency_key_expired"
        );
        assert_eq!(
            error(store.create_send("a", "done", "peer", "hash", b"text", "client")),
            "idempotency_key_expired"
        );
    }

    #[test]
    fn download_jobs_are_kept_so_an_attachment_can_be_fetched_again() {
        let (_temp, store) = fixture();
        let hash = "0".repeat(64);
        // Downloads hold no payload and their key comes from the attachment.
        for (id, key, state) in [
            ("finished", "download:asset-1", "available"),
            ("waiting", "download:asset-2", "queued"),
        ] {
            store
                .create_media_job(
                    "a",
                    id,
                    key,
                    &hash,
                    "download",
                    &json!({"assetId":"asset"}),
                    b"",
                )
                .unwrap();
            sql(&store, |db| {
                db.execute(
                    "UPDATE media_jobs SET state=?2,updated_at='2026-01-01T00:00:00+00:00' WHERE id=?1",
                    params![id, state],
                )?;
                Ok(())
            });
        }
        let step = settle(&store, unix_seconds() + 91 * DAY);
        assert_eq!(step.retired, 0);
        assert_eq!(store.media_job("a", "waiting").unwrap().state, "queued");
        // The renewal chain still finds the finished job and starts a new one.
        let previous = store.media_job_by_key("a", "download:asset-1").unwrap();
        assert_eq!(previous.id, "finished");
        let (again, created) = store
            .create_media_job(
                "a",
                "again",
                "download:asset-1:after:finished",
                &hash,
                "download",
                &json!({"assetId":"asset"}),
                b"",
            )
            .unwrap();
        assert!(created);
        assert_eq!(again.state, "queued");
        assert!(!store
            .is_retired_key("a", MEDIA_KEY, "download:asset-1")
            .unwrap());
    }

    #[test]
    fn compacted_sticker_snapshots_stay_idempotent() {
        let (_temp, store) = fixture();
        store
            .stage_sticker_projection("a", "snapshot", b"sealed")
            .unwrap();
        sql(&store, |db| {
            db.execute(
                "UPDATE apply_intents SET state='projected' WHERE id='snapshot'",
                [],
            )?;
            Ok(())
        });
        settle(&store, unix_seconds());
        assert_eq!(
            payload_length(&store, "apply_intents", "payload", "id", "snapshot"),
            0
        );
        store
            .stage_sticker_projection("a", "snapshot", b"sealed")
            .unwrap();
    }

    #[test]
    fn version_14_sidecars_upgrade_without_losing_records() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        {
            let store = Store::open(&root).unwrap();
            store.create_account("a").unwrap();
            store.put_runtime("a", "kept", b"value").unwrap();
            store
                .control("a", |db| {
                    db.execute_batch("DROP TABLE retired_keys;PRAGMA user_version=14;")?;
                    Ok(())
                })
                .unwrap();
        }
        let store = Store::open_existing(&root).unwrap();
        assert_eq!(store.runtime("a", "kept").unwrap().unwrap(), b"value");
        assert!(!store.is_retired_key("a", SEND_KEY, "anything").unwrap());
        let usage = store.storage_usage("a").unwrap();
        assert_eq!(usage.retired_keys, 0);
        assert!(usage.file_bytes > 0);
    }

    #[test]
    fn read_snapshots_and_usage_reports_change_nothing() {
        let (_temp, store) = fixture();
        store.stage_sync("a", "open", b"sealed", b"cursor").unwrap();
        let usage = store.storage_usage("a").unwrap();
        assert_eq!(usage.unresolved["syncBatchesPending"], 1);
        assert!(usage
            .tables
            .iter()
            .any(|t| t.name == "apply_intents" && t.rows == 1));
        assert!(store
            .runtime("a", &day_key(unix_seconds(), 0))
            .unwrap()
            .is_none());
    }

    #[test]
    fn runtime_families_are_classified_conservatively() {
        use RuntimeRetention::*;
        for (key, expected) in [
            ("projected:b:1", Remove(RECEIPT_DAYS)),
            ("contact-fields:projected:b:1", Remove(RECEIPT_DAYS)),
            ("contact-fields:legacy-mute", Keep),
            ("contact-pinning:r:u", Keep),
            ("event:message:id", Remove(RECEIPT_DAYS)),
            ("event:blocked-v2:b", Remove(RECEIPT_DAYS)),
            ("event:profile-recovery-pending", Keep),
            ("event:profile-wal-conversion:x", Keep),
            ("sync-control:projected:b:1", Keep),
            ("recall-control:x", Keep),
            ("read-version:t", Keep),
            ("received-cursor", Keep),
            ("metadata-receipt:u", RemoveFinishedMetadata),
            ("metadata-wire:u", Remove(RAW_DAYS)),
            ("incoming-group-held:x", Keep),
            (
                "thread-read-notification:h",
                Retire(READ_NOTIFICATION, RECEIPT_DAYS),
            ),
            ("retention-day:2026-01-01", Keep),
        ] {
            assert_eq!(aged_runtime(key), expected, "{key}");
        }
    }
}
