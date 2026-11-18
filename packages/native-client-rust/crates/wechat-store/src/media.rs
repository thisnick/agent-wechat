//! Account-scoped durable media jobs. Payloads are opaque vault ciphertext;
//! status/metadata never include URLs, native file IDs or encryption keys.
//!
//! Attachment and upload bytes are kept as files (`media_files`), not in the
//! database. Rows written by earlier versions still carry a BLOB and are read
//! from it until `externalize_media_payload` moves them.
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use ts_rs::TS;

/// A file uploaded to the daemon, ready to be sent.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OwnedMediaSource {
    /// Source ID. Pass it as `sourceId` when sending.
    pub id: String,
    /// File name given at upload.
    pub name: String,
    /// MIME type given at upload.
    pub media_type: String,
    /// Size of the file in bytes.
    #[ts(type = "number")]
    pub byte_length: u64,
    /// SHA-256 of the file's bytes, as lowercase hex.
    pub sha256: String,
    /// When the upload was stored, as an RFC 3339 time.
    pub created_at: String,
}
#[derive(Clone)]
pub struct MediaJobRecord {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub phase: String,
    pub attempts: u32,
    pub cancel_requested: bool,
    pub retryable: bool,
    pub error_code: Option<String>,
    pub data: Value,
    pub payload: Vec<u8>,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone)]
pub struct MediaAssetRecord {
    pub id: String,
    pub message_id: Option<String>,
    pub variant: String,
    pub kind: String,
    pub metadata: Value,
    pub plan: Vec<u8>,
    /// Present on writes and `media_asset_with_payload`; metadata reads omit it.
    pub payload: Option<Vec<u8>>,
}
/// An asset's own sealed copy, opened for streaming reads.
pub enum MediaAssetPayload {
    /// A private payload file; `name` is the file name its row records.
    File { file: std::fs::File, name: String },
    /// A sealed payload an earlier version stored inside the database.
    Embedded(Vec<u8>),
}
pub struct CompletedUploadRetry<'a> {
    pub expected: &'a MediaJobRecord,
    pub source: &'a OwnedMediaSource,
    pub request_hash: &'a str,
    pub expected_authorization: &'a [u8],
    pub expected_logout_epoch: Option<&'a [u8]>,
    pub ownership_generation: &'a str,
    pub payload: &'a [u8],
}
pub(crate) fn read_job(db: &Connection, id: &str) -> Result<MediaJobRecord> {
    let row=db.query_row("SELECT id,kind,state,phase,attempts,cancel_requested,retryable,error_code,data_json,payload,created_at,updated_at FROM media_jobs WHERE id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,u32>(4)?,r.get::<_,bool>(5)?,r.get::<_,bool>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,String>(8)?,r.get::<_,Vec<u8>>(9)?,r.get::<_,String>(10)?,r.get::<_,String>(11)?)))?;
    Ok(MediaJobRecord {
        id: row.0,
        kind: row.1,
        state: row.2,
        phase: row.3,
        attempts: row.4,
        cancel_requested: row.5,
        retryable: row.6,
        error_code: row.7,
        data: serde_json::from_str(&row.8)?,
        payload: row.9,
        created_at: row.10,
        updated_at: row.11,
    })
}
pub(crate) fn read_asset(db: &Connection, id: &str) -> Result<MediaAssetRecord> {
    // Metadata and authorization checks must not materialize attachment BLOBs.
    let row = db.query_row(
        "SELECT id,message_id,variant,kind,metadata_json,plan FROM media_assets WHERE id=?1",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, String>(4)?,
                r.get(5)?,
            ))
        },
    )?;
    Ok(MediaAssetRecord {
        id: row.0,
        message_id: row.1,
        variant: row.2,
        kind: row.3,
        metadata: serde_json::from_str(&row.4)?,
        plan: row.5,
        payload: None,
    })
}
fn media_event(tx: &Connection, kind: &str, data: &Value) -> Result<i64> {
    let seq = insert_event(tx, kind, data)?;
    crate::trim_events(tx)?;
    Ok(seq)
}
/// Append one event without trimming the log; returns its sequence number.
fn insert_event(tx: &Connection, kind: &str, data: &Value) -> Result<i64> {
    let seq: i64 = tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events", [], |r| {
        r.get(0)
    })?;
    tx.execute(
        "INSERT INTO events VALUES(?1,?2,?3,?4)",
        params![
            seq,
            kind,
            serde_json::to_string(data)?,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(seq)
}
/// Fields of a `job.updated` event that only report transfer progress.
const PROGRESS_FIELDS: [&str; 3] = ["completedBytes", "totalBytes", "updatedAt"];
/// Record a `job.updated` event. When the job's previous one differs only in
/// [`PROGRESS_FIELDS`], it is deleted after the new one is inserted, so the
/// event log keeps one progress event per job state instead of one per
/// chunk. Sequence numbers are never reused, state changes are never
/// merged, and the oldest event is never deleted (that would make a cursor
/// just before it look expired).
fn job_updated_event(tx: &Connection, job: &str, data: &Value) -> Result<()> {
    let key = format!("job-event:{job}");
    let previous: Option<i64> = tx
        .query_row(
            "SELECT CAST(CAST(value AS TEXT) AS INTEGER) FROM runtime WHERE key=?1",
            [&key],
            |r| r.get(0),
        )
        .optional()?;
    let seq = insert_event(tx, "job.updated", data)?;
    if let Some(previous) = previous {
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT kind,data_json FROM events WHERE sequence=?1 AND sequence>(SELECT MIN(sequence) FROM events)",
                [previous],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let progress_only = |old: &Value| {
            let strip = |value: &Value| {
                let mut value = value.clone();
                if let Some(object) = value.as_object_mut() {
                    for field in PROGRESS_FIELDS {
                        object.remove(field);
                    }
                }
                value
            };
            strip(old) == strip(data)
        };
        if let Some((kind, raw)) = row {
            if kind == "job.updated"
                && serde_json::from_str::<Value>(&raw).is_ok_and(|old| progress_only(&old))
            {
                tx.execute("DELETE FROM events WHERE sequence=?1", [previous])?;
            }
        }
    }
    // Trim after the merge, so the log still holds its full row count.
    crate::trim_events(tx)?;
    tx.execute(
        "INSERT INTO runtime(key,value)VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, seq.to_string().into_bytes()],
    )?;
    Ok(())
}
fn job_changed(old: &MediaJobRecord, new: &MediaJobRecord) -> bool {
    old.state != new.state
        || old.phase != new.phase
        || old.attempts != new.attempts
        || old.cancel_requested != new.cancel_requested
        || old.retryable != new.retryable
        || old.error_code != new.error_code
        || old.data != new.data
}
/// Copy a WAL sidecar's log back into the database and truncate it, so a
/// rewrite does not leave a log as large as the database. An open read
/// snapshot can keep the log in use; the next pass tries again.
fn checkpoint(db: &Connection) -> Result<()> {
    let mode: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if mode.eq_ignore_ascii_case("wal") {
        db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    }
    Ok(())
}
pub(crate) fn job_event(tx: &Connection, account: &str, row: &MediaJobRecord) -> Result<()> {
    if let Some(id) = row.data["assetId"].as_str() {
        let metadata: Option<String> = tx
            .query_row(
                "SELECT metadata_json FROM media_assets WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(metadata) = metadata {
            let mut metadata: Value = serde_json::from_str(&metadata)?;
            if metadata["state"] != "available" && row.state != "available" {
                metadata["state"] = if matches!(row.state.as_str(), "queued" | "running") {
                    "pending"
                } else {
                    &row.state
                }
                .into();
                metadata["reason"] = row.error_code.clone().into();
                metadata["retryable"] = row.retryable.into();
                metadata["jobId"] = row.id.clone().into();
                tx.execute(
                    "UPDATE media_assets SET metadata_json=?2,updated_at=?3 WHERE id=?1",
                    params![
                        id,
                        serde_json::to_string(&metadata)?,
                        chrono::Utc::now().to_rfc3339()
                    ],
                )?;
            }
        }
    }
    let data = json!({"id":row.id,"jobId":row.id,"accountId":account,"kind":row.kind,"state":row.state,"phase":row.phase,"attempts":row.attempts,"cancelRequested":row.cancel_requested,"retryable":row.retryable,"reason":row.error_code,"assetId":row.data["assetId"],"sourceId":row.data["sourceId"],"threadId":row.data["threadId"],"serverMessageId":row.data["serverMessageId"],"completedBytes":row.data["completedBytes"],"totalBytes":row.data["totalBytes"],"result":row.data["result"],"createdAt":row.created_at,"updatedAt":row.updated_at});
    job_updated_event(tx, &row.id, &data)?;
    if row.kind == "send"
        && matches!(
            row.state.as_str(),
            "accepted" | "failed" | "cancelled" | "needs_review" | "unsupported"
        )
    {
        media_event(tx, "send.updated", &data)?;
    }
    Ok(())
}
pub(crate) fn cancel_queued_media_for_logout(tx: &Connection, account: &str) -> Result<usize> {
    let ids = {
        let mut q = tx.prepare("SELECT id FROM media_jobs WHERE state='queued'")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    tx.execute("UPDATE media_jobs SET state='cancelled',error_code='logout_before_submission',retryable=0,cancel_requested=1,updated_at=?1 WHERE state='queued'",[chrono::Utc::now().to_rfc3339()])?;
    for id in &ids {
        job_event(tx, account, &read_job(tx, id)?)?;
    }
    Ok(ids.len())
}
impl Store {
    fn media_directory(&self, account: &str) -> Result<PathBuf> {
        Ok(media_files::directory(&self.account_dir(account)?))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_media_source(
        &self,
        account: &str,
        id: &str,
        name: &str,
        media_type: &str,
        byte_length: u64,
        sha256: &str,
        payload: &[u8],
    ) -> Result<OwnedMediaSource> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            if id.is_empty()
                || id.len() > 100
                || name.is_empty()
                || name.len() > 255
                || media_type.len() > 128
                || byte_length == 0
                || byte_length > wechat_media::MAX_FILE_BYTES as u64
                || sha256.len() != 64
            {
                bail!("invalid media source");
            }
            let now = chrono::Utc::now().to_rfc3339();
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_sources WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if exists {
                bail!("media source already exists");
            }
            let file = media_files::write(&files, payload)?;
            if let Err(error) = db.execute(
                "INSERT INTO media_sources(id,name,media_type,byte_length,sha256,payload,created_at,payload_file) VALUES(?1,?2,?3,?4,?5,x'',?6,?7)",
                params![id, name, media_type, byte_length, sha256, now, file],
            ) {
                media_files::remove(&files, &file);
                return Err(error.into());
            }
            Ok(OwnedMediaSource {
                id: id.into(),
                name: name.into(),
                media_type: media_type.into(),
                byte_length,
                sha256: sha256.into(),
                created_at: now,
            })
        })
    }
    /// Create an upload whose encrypted payload `write` streams into a new
    /// private file, returning the plaintext length and SHA-256. The file is
    /// written before the account database is locked, so a slow upload does not
    /// hold up other work; the row is added only once the payload is complete.
    pub fn create_media_source_streamed(
        &self,
        account: &str,
        id: &str,
        name: &str,
        media_type: &str,
        write: impl FnOnce(&mut std::fs::File) -> Result<(u64, String)>,
    ) -> Result<OwnedMediaSource> {
        let files = self.media_directory(account)?;
        let mut written = None;
        let file = media_files::write_with(&files, |payload| {
            written = Some(write(payload)?);
            Ok(())
        })?;
        let (byte_length, sha256) = written.context("media payload missing")?;
        let created = self.control(account, |db| {
            if id.is_empty()
                || id.len() > 100
                || name.is_empty()
                || name.len() > 255
                || media_type.len() > 128
                || byte_length == 0
                || byte_length > wechat_media::MAX_FILE_BYTES as u64
                || sha256.len() != 64
            {
                bail!("invalid media source");
            }
            let now = chrono::Utc::now().to_rfc3339();
            db.execute(
                "INSERT INTO media_sources(id,name,media_type,byte_length,sha256,payload,created_at,payload_file) VALUES(?1,?2,?3,?4,?5,x'',?6,?7)",
                params![id, name, media_type, byte_length, sha256, now, file],
            )?;
            Ok(OwnedMediaSource {
                id: id.into(),
                name: name.into(),
                media_type: media_type.into(),
                byte_length,
                sha256: sha256.clone(),
                created_at: now,
            })
        });
        if created.is_err() {
            media_files::remove(&files, &file);
        }
        created
    }
    /// The upload's metadata and a reader of its encrypted bytes, for
    /// streaming. A deleted file is an error.
    pub fn media_source_reader(
        &self,
        account: &str,
        id: &str,
    ) -> Result<(OwnedMediaSource, Box<dyn std::io::Read + Send>)> {
        let files = self.media_directory(account)?;
        let (source, file) = self.control(account, |db| {
            Ok(db.query_row(
                "SELECT name,media_type,byte_length,sha256,created_at,payload_file FROM media_sources WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        OwnedMediaSource {
                            id: id.into(),
                            name: r.get(0)?,
                            media_type: r.get(1)?,
                            byte_length: r.get(2)?,
                            sha256: r.get(3)?,
                            created_at: r.get(4)?,
                        },
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )?)
        })?;
        let reader: Box<dyn std::io::Read + Send> = match file {
            Some(file) => Box::new(std::io::BufReader::new(media_files::open(&files, &file)?)),
            // Earlier versions kept small payloads inside the database.
            None => Box::new(std::io::Cursor::new(self.media_source(account, id)?.1)),
        };
        Ok((source, reader))
    }
    /// The upload's metadata and encrypted bytes. A deleted file is an error.
    pub fn media_source(&self, account: &str, id: &str) -> Result<(OwnedMediaSource, Vec<u8>)> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let (source, file): (OwnedMediaSource, Option<String>) = db.query_row(
                "SELECT name,media_type,byte_length,sha256,created_at,payload_file FROM media_sources WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        OwnedMediaSource {
                            id: id.into(),
                            name: r.get(0)?,
                            media_type: r.get(1)?,
                            byte_length: r.get(2)?,
                            sha256: r.get(3)?,
                            created_at: r.get(4)?,
                        },
                        r.get(5)?,
                    ))
                },
            )?;
            let sealed = match file {
                Some(file) => media_files::read(&files, &file)?,
                None => db.query_row(
                    "SELECT payload FROM media_sources WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )?,
            };
            Ok((source, sealed))
        })
    }
    /// Delete an upload and its file. Returns false when it does not exist.
    pub fn delete_media_source(&self, account: &str, id: &str) -> Result<bool> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let file: Option<Option<String>> = db
                .query_row(
                    "SELECT payload_file FROM media_sources WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(file) = file else {
                return Ok(false);
            };
            db.execute("DELETE FROM media_sources WHERE id=?1", [id])?;
            if let Some(file) = file {
                media_files::remove(&files, &file);
            }
            Ok(true)
        })
    }
    /// IDs of uploads whose ID starts with `prefix`, at most `limit`.
    pub fn media_source_ids_with_prefix(
        &self,
        account: &str,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        anyhow::ensure!(
            !prefix.is_empty() && (1..=1000).contains(&limit),
            "invalid_limit"
        );
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT id FROM media_sources WHERE substr(id,1,length(?1))=?1 ORDER BY id LIMIT ?2",
            )?;
            let rows = q.query_map(params![prefix, limit], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    pub fn media_source_metadata(&self, account: &str, id: &str) -> Result<OwnedMediaSource> {
        self.control(account,|db|{Ok(db.query_row("SELECT name,media_type,byte_length,sha256,created_at FROM media_sources WHERE id=?1",[id],|r|Ok(OwnedMediaSource{id:id.into(),name:r.get(0)?,media_type:r.get(1)?,byte_length:r.get(2)?,sha256:r.get(3)?,created_at:r.get(4)?}))?)})
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_media_job(
        &self,
        account: &str,
        id: &str,
        key: &str,
        hash: &str,
        kind: &str,
        data: &Value,
        payload: &[u8],
    ) -> Result<(MediaJobRecord, bool)> {
        self.control(account, |db| {
            if key.is_empty() || key.len()>250 || id.is_empty() || id.len()>100 || hash.len()!=64 || !matches!(kind,"download"|"send"|"sticker_prepare"|"sticker_saved"|"sticker_pack"|"sticker_acquire"|"sticker_send") { bail!("invalid media job"); }
            let tx = db.transaction()?;
            if kind == "send" {
                let inhibited:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')",[],|r|r.get(0))?;
                if inhibited {bail!("auth_required");}
                let key = key.strip_prefix("send:").ok_or_else(||anyhow::anyhow!("invalid media idempotency namespace"))?;
                let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE idempotency_key=?1)",[key],|r|r.get(0))?;
                if exists {bail!("idempotency_conflict");}
            }
            let existing:Option<(String,String)> = tx.query_row("SELECT id,request_hash FROM media_jobs WHERE idempotency_key=?1",[key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((id,old))=existing {
                if old!=hash { bail!("idempotency_conflict"); }
                return Ok((read_job(&tx,&id)?,false));
            }
            crate::retention::ensure_key_live(&tx, crate::retention::MEDIA_KEY, key)?;
            if let Some(send) = key.strip_prefix("send:") {
                crate::retention::ensure_key_live(&tx, crate::retention::SEND_KEY, send)?;
            }
            let queued:u32=tx.query_row("SELECT count(*) FROM media_jobs WHERE state IN ('queued','running')",[],|r|r.get(0))?;
            if queued>=100 { bail!("media_queue_full"); }
            let now=chrono::Utc::now().to_rfc3339();
            tx.execute("INSERT INTO media_jobs VALUES(?1,?2,?3,?4,'queued','queued',0,0,0,NULL,?5,?6,?7,?7)",params![id,key,hash,kind,serde_json::to_string(data)?,payload,now])?;
            let row=read_job(&tx,id)?;
            job_event(&tx,account,&row)?;
            tx.commit()?;
            Ok((row,true))
        })
    }

    /// Latest completed acquisitions for one product, used only to reuse owned
    /// temporary archives after membership changes. This is not a pack catalog.
    pub fn completed_sticker_acquisitions(&self, account: &str, pack: &str) -> Result<Vec<String>> {
        self.control(account,|db| {
            // Retention keeps these plans and empties older ones: see
            // `retention::reused_acquisition_ids`.
            let mut query=db.prepare("SELECT id FROM media_jobs WHERE kind='sticker_acquire' AND state='available' AND json_extract(data_json,'$.packId')=?1 ORDER BY updated_at DESC,id DESC LIMIT ?2")?;
            let rows=query.query_map(params![pack,crate::retention::REUSED_ACQUISITIONS],|row|row.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    pub fn media_job(&self, account: &str, id: &str) -> Result<MediaJobRecord> {
        self.control(account, |db| {
            crate::retention::expired_if_missing(
                db,
                crate::retention::MEDIA_ID,
                id,
                read_job(db, id),
            )
        })
    }
    pub fn media_job_by_key(&self, account: &str, key: &str) -> Result<MediaJobRecord> {
        self.control(account, |db| {
            let id: Option<String> = db
                .query_row(
                    "SELECT id FROM media_jobs WHERE idempotency_key=?1",
                    [key],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                crate::retention::ensure_key_live(db, crate::retention::MEDIA_KEY, key)?;
                return Err(rusqlite::Error::QueryReturnedNoRows.into());
            };
            read_job(db, &id)
        })
    }
    /// Find an existing submission and validate its content hash before a
    /// caller rechecks expiring inputs or changing account preconditions.
    pub fn find_media_job(
        &self,
        account: &str,
        key: &str,
        hash: &str,
    ) -> Result<Option<MediaJobRecord>> {
        self.control(account, |db| {
            let existing: Option<(String, String)> = db
                .query_row(
                    "SELECT id,request_hash FROM media_jobs WHERE idempotency_key=?1",
                    [key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if existing.is_none() {
                crate::retention::ensure_key_live(db, crate::retention::MEDIA_KEY, key)?;
            }
            existing
                .map(|(id, old)| {
                    if old != hash {
                        bail!("idempotency_conflict");
                    }
                    read_job(db, &id)
                })
                .transpose()
        })
    }
    pub fn pending_media_projections(&self, account: &str) -> Result<Vec<String>> {
        self.control(account,|db|{
            let mut query=db.prepare("SELECT id FROM media_jobs WHERE kind='send' AND state IN ('accepted','failed','cancelled','needs_review') AND length(payload)>0 AND coalesce(json_extract(data_json,'$.result.profileProjection'),'') NOT IN ('complete','not_applicable','unknown') ORDER BY created_at,id LIMIT 100")?;
            let rows=query.query_map([],|r|r.get(0))?;Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    /// A committed history row does not prove its cached bytes and resource
    /// metadata reached the official profile. Profile-only copies must report
    /// those gaps, including partial voice jobs with already accepted notes.
    pub fn incomplete_media_writeback(&self, account: &str) -> Result<bool> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE json_extract(metadata_json,'$.state')='available' AND coalesce(json_extract(metadata_json,'$.officialWriteback'),'') NOT IN ('complete','not_applicable')) OR EXISTS(SELECT 1 FROM media_jobs j WHERE j.kind='send' AND (j.state='accepted' OR json_extract(j.data_json,'$.serverMessageId') IS NOT NULL OR EXISTS(SELECT 1 FROM json_each(j.data_json,'$.result.chunks') c WHERE json_extract(c.value,'$.state')='accepted')) AND coalesce(json_extract(j.data_json,'$.result.officialMediaWriteback'),'') NOT IN ('complete','not_applicable')) OR EXISTS(SELECT 1 FROM media_jobs WHERE kind IN ('sticker_saved','sticker_pack','sticker_send','sticker_acquire') AND (state IN ('queued','running') OR (state='needs_review' AND coalesce(error_code,'')!='review_expired') OR (length(payload)>0 AND json_extract(data_json,'$.serverAcceptance')='accepted' AND coalesce(json_extract(data_json,'$.profileWriteback'),'')!='complete')))",
                [],
                |row| row.get(0),
            )?)
        })
    }
    /// Bounded local repair candidates. This never enqueues or re-submits a send.
    pub fn media_writeback_candidates(
        &self,
        account: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        anyhow::ensure!((1..=101).contains(&limit), "invalid_limit");
        self.control(account,|db|{
            let cursor:Option<Vec<u8>>=db.query_row("SELECT value FROM runtime WHERE key='media-writeback-cursor'",[],|r|r.get(0)).optional()?;
            let (family,id): (String,String)=match cursor {
                Some(bytes)=>serde_json::from_slice(&bytes)?,
                None=>(String::new(),String::new()),
            };
            anyhow::ensure!(family.is_empty() || matches!(family.as_str(),"asset"|"job"),"invalid_media_repair_cursor");
            anyhow::ensure!(id.len()<=255 && !id.chars().any(char::is_control),"invalid_media_repair_cursor");
            let mut q=db.prepare("SELECT family,id FROM (SELECT 'asset' AS family,id FROM media_assets WHERE kind IN ('image','file','video','voice') AND json_extract(metadata_json,'$.state')='available' AND coalesce(json_extract(metadata_json,'$.officialWriteback'),'') NOT IN ('complete','not_applicable') UNION ALL SELECT 'job' AS family,id FROM media_jobs WHERE kind IN ('sticker_saved','sticker_pack','sticker_send','sticker_acquire') AND length(payload)>0 AND json_extract(data_json,'$.serverAcceptance')='accepted' AND coalesce(json_extract(data_json,'$.profileWriteback'),'')!='complete' UNION ALL SELECT 'job' AS family,id FROM media_jobs j WHERE j.kind='send' AND (j.state='accepted' OR json_extract(j.data_json,'$.serverMessageId') IS NOT NULL OR EXISTS(SELECT 1 FROM json_each(j.data_json,'$.result.chunks') c WHERE json_extract(c.value,'$.state')='accepted')) AND coalesce(json_extract(j.data_json,'$.result.officialMediaWriteback'),'') NOT IN ('complete','not_applicable')) ORDER BY CASE WHEN family>?2 OR (family=?2 AND id>?3) THEN 0 ELSE 1 END,family,id LIMIT ?1")?;
            let rows=q.query_map(params![limit,family,id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    /// Move the bounded scan beyond an attempted item, including persistent
    /// conflicts. A crash before this checkpoint repeats only a local replay;
    /// the checkpoint and its audit event either both commit or both roll back.
    pub fn record_media_writeback_scan(
        &self,
        account: &str,
        family: &str,
        id: &str,
        repaired: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            matches!(family, "asset" | "job")
                && !id.is_empty()
                && id.len() <= 255
                && !id.chars().any(char::is_control),
            "invalid_media_repair_cursor"
        );
        let bytes = serde_json::to_vec(&(family, id))?;
        self.control(account,|db|{
            let tx=db.transaction()?;
            tx.execute("INSERT INTO runtime(key,value)VALUES('media-writeback-cursor',?1)ON CONFLICT(key)DO UPDATE SET value=excluded.value",[bytes])?;
            media_event(&tx,"media.writeback_examined",&json!({"accountId":account,"family":family,"id":id,"repaired":repaired}))?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn queued_media_jobs(&self, account: &str) -> Result<Vec<String>> {
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT id FROM media_jobs WHERE state='queued' ORDER BY created_at,id LIMIT 100",
            )?;
            let rows = q.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    pub fn claim_media_job(&self, account: &str, id: &str) -> Result<bool> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            let changed=tx.execute("UPDATE media_jobs SET state='running',phase=CASE WHEN phase='queued' THEN 'preparing' ELSE phase END,attempts=attempts+1,updated_at=?2 WHERE id=?1 AND state='queued' AND cancel_requested=0",params![id,chrono::Utc::now().to_rfc3339()])?==1;
            if changed { job_event(&tx,account,&read_job(&tx,id)?)?; }
            tx.commit()?;Ok(changed)
        })
    }
    pub fn save_media_job(&self, account: &str, row: &MediaJobRecord) -> Result<MediaJobRecord> {
        self.control(account,|db|{
            let tx=db.transaction()?;let old=read_job(&tx,&row.id)?;
            tx.execute("UPDATE media_jobs SET state=?2,phase=?3,retryable=?4,error_code=?5,data_json=?6,payload=?7,cancel_requested=MAX(cancel_requested,?8),updated_at=?9 WHERE id=?1",params![row.id,row.state,row.phase,row.retryable,row.error_code,serde_json::to_string(&row.data)?,row.payload,row.cancel_requested,chrono::Utc::now().to_rfc3339()])?;
            let saved=read_job(&tx,&row.id)?;
            if job_changed(&old,&saved) {job_event(&tx,account,&saved)?;}
            tx.commit()?;Ok(saved)
        })
    }
    pub fn cancel_media_job(&self, account: &str, id: &str) -> Result<MediaJobRecord> {
        self.control(account,|db|{
            let tx=db.transaction()?;let old=read_job(&tx,id)?;
            if !matches!(old.state.as_str(),"available"|"accepted"|"expired"|"unsupported"|"needs_review") {
                tx.execute("UPDATE media_jobs SET cancel_requested=1,state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,retryable=CASE WHEN kind IN ('download','sticker_prepare') THEN 1 ELSE retryable END,updated_at=?2 WHERE id=?1",params![id,chrono::Utc::now().to_rfc3339()])?;
            }
            let row=read_job(&tx,id)?;if job_changed(&old,&row){job_event(&tx,account,&row)?;}
            tx.commit()?;Ok(row)
        })
    }
    pub fn retry_media_job(&self, account: &str, id: &str) -> Result<MediaJobRecord> {
        self.control(account,|db|{
            let tx=db.transaction()?;let row=read_job(&tx,id)?;
            if !row.retryable || !matches!(row.state.as_str(),"failed"|"cancelled") || row.attempts>=3 || matches!(row.phase.as_str(),"reserving"|"uploading"|"submitting") {bail!("media_job_not_retryable");}
            let queued:u32=tx.query_row("SELECT count(*) FROM media_jobs WHERE state IN ('queued','running')",[],|r|r.get(0))?;
            if queued>=100 {bail!("media_queue_full");}
            tx.execute("UPDATE media_jobs SET state='queued',cancel_requested=0,error_code=NULL,retryable=0,updated_at=?2 WHERE id=?1",params![id,chrono::Utc::now().to_rfc3339()])?;
            let row=read_job(&tx,id)?;job_event(&tx,account,&row)?;
            tx.commit()?;Ok(row)
        })
    }
    /// Queue only a core-validated, completed upload before its first business
    /// submission. The exact old row and authorization must still be current.
    pub fn retry_media_job_completed_upload(
        &self,
        account: &str,
        retry: CompletedUploadRetry<'_>,
    ) -> Result<MediaJobRecord> {
        self.control(account, |db| {
            let tx=db.transaction()?;
            let old=read_job(&tx,&retry.expected.id)?;
            let expected=retry.expected;
            if old.kind!="send" || old.state!="unsupported" || old.phase!="readback" || old.attempts>=3 || old.cancel_requested || old.error_code.as_deref()!=Some("unsupported_media")
                || !old.data.is_object() || !(old.data["result"].is_null() || old.data["result"].is_object())
                || !old.data["serverMessageId"].is_null() || !old.data["result"]["nativeReceipt"].is_null()
                || !(old.data["result"]["messageIds"].is_null() || old.data["result"]["messageIds"].as_array().is_some_and(|ids|ids.is_empty()))
                || old.state!=expected.state || old.phase!=expected.phase || old.attempts!=expected.attempts
                || old.error_code!=expected.error_code || old.payload!=expected.payload || old.data!=expected.data
                || old.updated_at!=expected.updated_at {
                bail!("completed_upload_retry_not_admitted");
            }
            let hash:String=tx.query_row("SELECT request_hash FROM media_jobs WHERE id=?1",[&old.id],|r|r.get(0))?;
            if hash!=retry.request_hash {bail!("completed_upload_retry_not_admitted");}
            let source:(String,u64,String)=tx.query_row("SELECT name,byte_length,sha256 FROM media_sources WHERE id=?1",[&retry.source.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if source.0!=retry.source.name || source.1!=retry.source.byte_length || source.2!=retry.source.sha256 {bail!("completed_upload_retry_not_admitted");}
            let value=|key:&str|->Result<Option<Vec<u8>>>{Ok(tx.query_row("SELECT value FROM runtime WHERE key=?1",[key],|r|r.get(0)).optional()?)};
            let accepted=format!("{:x}",Sha256::digest(retry.expected_authorization));
            if value("logged-out")?.is_some() || value("logout-epoch")?.as_deref()!=retry.expected_logout_epoch
                || value("account-state")?.as_deref()!=Some(retry.expected_authorization)
                || value("saved-login-accepted-state-generation")?.as_deref()!=Some(accepted.as_bytes())
                || value("desired")?.as_deref()!=Some(b"connected")
                || value("validated-generation")?.as_deref()!=Some(retry.ownership_generation.as_bytes()) {bail!("authorization_generation_changed");}
            let queued:u32=tx.query_row("SELECT count(*) FROM media_jobs WHERE state IN ('queued','running')",[],|r|r.get(0))?;
            if queued>=100 {bail!("media_queue_full");}
            let mut data=old.data.clone();
            data["result"]["completedUploadReuse"]=json!({"requestedAt":chrono::Utc::now().to_rfc3339(),"previousReason":old.error_code});
            tx.execute("UPDATE media_jobs SET state='queued',cancel_requested=0,error_code=NULL,retryable=0,data_json=?2,payload=?3,updated_at=?4 WHERE id=?1",params![old.id,serde_json::to_string(&data)?,retry.payload,chrono::Utc::now().to_rfc3339()])?;
            let row=read_job(&tx,&old.id)?;
            job_event(&tx,account,&row)?;
            tx.commit()?;
            Ok(row)
        })
    }
    pub fn recover_media_jobs(&self, account: &str) -> Result<usize> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            let ids={let mut q=tx.prepare("SELECT id FROM media_jobs WHERE state='running'")?;let rows=q.query_map([],|r|r.get::<_,String>(0))?;rows.collect::<rusqlite::Result<Vec<_>>>()?};
            let now=chrono::Utc::now().to_rfc3339();
            tx.execute("UPDATE media_jobs SET state='needs_review',error_code=CASE WHEN phase='reserving' THEN 'restart_during_reservation' ELSE 'restart_after_submission' END,retryable=0,updated_at=?1 WHERE state='running' AND kind IN ('send','sticker_saved','sticker_pack','sticker_acquire','sticker_send') AND phase IN ('reserving','uploading','submitting')",[&now])?;
            tx.execute("UPDATE media_jobs SET state=CASE WHEN cancel_requested=1 THEN 'cancelled' WHEN attempts<3 THEN 'queued' ELSE 'failed' END,retryable=CASE WHEN attempts<3 THEN 1 ELSE 0 END,error_code='worker_interrupted',updated_at=?1 WHERE state='running'",[&now])?;
            for id in &ids {job_event(&tx,account,&read_job(&tx,id)?)?;}
            tx.commit()?;Ok(ids.len())
        })
    }
    pub fn put_media_asset(
        &self,
        account: &str,
        asset: &MediaAssetRecord,
    ) -> Result<MediaAssetRecord> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id=?1)",
                [&asset.id],
                |r| r.get(0),
            )?;
            if !exists {
                let file = asset
                    .payload
                    .as_deref()
                    .map(|sealed| media_files::write(&files, sealed))
                    .transpose()?;
                if let Err(error) = db.execute(
                    "INSERT INTO media_assets(id,message_id,variant,kind,metadata_json,plan,updated_at,payload_file) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        asset.id,
                        asset.message_id,
                        asset.variant,
                        asset.kind,
                        serde_json::to_string(&asset.metadata)?,
                        asset.plan,
                        chrono::Utc::now().to_rfc3339(),
                        file
                    ],
                ) {
                    if let Some(file) = &file {
                        media_files::remove(&files, file);
                    }
                    return Err(error.into());
                }
            }
            read_asset(db, &asset.id)
        })
    }
    /// Note that a listing just returned this asset, so retention's idle
    /// clock restarts (at most one write a day per row). Returns whether the
    /// row exists. A read snapshot changes nothing and reports `true`.
    pub fn touch_media_asset(&self, account: &str, id: &str) -> Result<bool> {
        if self.is_read_snapshot() {
            return Ok(true);
        }
        let now = chrono::Utc::now();
        self.control(account, |db| {
            let touched = db.execute(
                "UPDATE media_assets SET updated_at=?2 WHERE id=?1 AND updated_at<?3",
                params![
                    id,
                    now.to_rfc3339(),
                    (now - chrono::Duration::days(1)).to_rfc3339()
                ],
            )?;
            Ok(touched == 1
                || db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM media_assets WHERE id=?1)",
                    [id],
                    |r| r.get(0),
                )?)
        })
    }
    pub fn media_asset(&self, account: &str, id: &str) -> Result<MediaAssetRecord> {
        self.control(account, |db| {
            crate::retention::expired_if_missing(
                db,
                crate::retention::MEDIA_ASSET_ID,
                id,
                read_asset(db, id),
            )
        })
    }
    /// Fetch cached bytes only for content reads or local writeback repair.
    /// `payload` is `None` when this store keeps no copy, which includes an
    /// attachment read from the official profile. A deleted file is an error.
    pub fn media_asset_with_payload(&self, account: &str, id: &str) -> Result<MediaAssetRecord> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let mut row = read_asset(db, id)?;
            let file: Option<String> = db.query_row(
                "SELECT payload_file FROM media_assets WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            row.payload = match file {
                Some(file) => Some(media_files::read(&files, &file)?),
                None => {
                    db.query_row("SELECT payload FROM media_assets WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?
                }
            };
            Ok(row)
        })
    }
    /// The asset's own sealed copy opened for streaming, without reading it:
    /// the payload file, or a payload an earlier version kept inside the
    /// database. `None` when this store keeps no copy, which includes an
    /// attachment read from the official profile. A deleted file is an error.
    pub fn media_asset_payload(
        &self,
        account: &str,
        id: &str,
    ) -> Result<Option<MediaAssetPayload>> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let (file, embedded): (Option<String>, Option<Vec<u8>>) = db.query_row(
                "SELECT payload_file, CASE WHEN payload_file IS NULL THEN payload END FROM media_assets WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(match (file, embedded) {
                (Some(name), _) => Some(MediaAssetPayload::File {
                    file: media_files::open(&files, &name)?,
                    name,
                }),
                (None, Some(sealed)) => Some(MediaAssetPayload::Embedded(sealed)),
                (None, None) => None,
            })
        })
    }
    /// Whether this store still holds the asset's own copy: an embedded payload or a payload
    /// file that has not been deleted. Reads no payload bytes.
    pub fn media_asset_cached(&self, account: &str, id: &str) -> Result<bool> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let (file, embedded): (Option<String>, bool) = db.query_row(
                "SELECT payload_file, payload IS NOT NULL FROM media_assets WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            match file {
                Some(file) => media_files::present(&files, &file),
                None => Ok(embedded),
            }
        })
    }
    /// The official-profile lookup key recorded when this attachment's own copy
    /// was released, or `None` while its bytes are kept by this store.
    pub fn media_asset_profile_key(&self, account: &str, id: &str) -> Result<Option<String>> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT profile_key FROM media_assets WHERE id=?1",
                [id],
                |r| r.get(0),
            )?)
        })
    }
    /// Save metadata for an attachment whose bytes are read from the official
    /// profile, and remove any copy kept here. The caller has verified that the
    /// profile returns the same bytes.
    pub fn save_media_asset_in_profile(
        &self,
        account: &str,
        id: &str,
        metadata: &Value,
        profile_key: &str,
    ) -> Result<MediaAssetRecord> {
        anyhow::ensure!(
            !profile_key.is_empty() && profile_key.len() <= 255,
            "invalid media profile key"
        );
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let tx = db.transaction()?;
            let old = read_asset(&tx, id)?;
            let file: Option<String> = tx.query_row(
                "SELECT payload_file FROM media_assets WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE media_assets SET metadata_json=?2,payload=NULL,payload_file=NULL,profile_key=?3,updated_at=?4 WHERE id=?1",
                params![
                    id,
                    serde_json::to_string(metadata)?,
                    profile_key,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
            let row = read_asset(&tx, id)?;
            if old.metadata["state"] != "available" && metadata["state"] == "available" {
                media_event(
                    &tx,
                    "media.ready",
                    &json!({"assetId":id,"jobId":metadata["jobId"],"accountId":account}),
                )?;
            }
            tx.commit()?;
            if let Some(file) = file {
                media_files::remove(&files, &file);
            }
            Ok(row)
        })
    }
    pub fn media_assets(&self, account: &str, message_id: &str) -> Result<Vec<MediaAssetRecord>> {
        self.control(account, |db| {
            let mut q =
                db.prepare("SELECT id FROM media_assets WHERE message_id=?1 ORDER BY variant,id")?;
            let ids = q
                .query_map([message_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.iter().map(|id| read_asset(db, id)).collect()
        })
    }
    pub fn save_media_asset(
        &self,
        account: &str,
        id: &str,
        metadata: &Value,
        payload: Option<&[u8]>,
    ) -> Result<MediaAssetRecord> {
        match payload {
            Some(sealed) => self.save_media_asset_streamed(account, id, metadata, |file| {
                Ok(std::io::Write::write_all(file, sealed)?)
            }),
            None => self.save_media_asset_row(account, id, metadata, None),
        }
    }
    /// [`Self::save_media_asset`] with a sealed payload that `fill` streams into
    /// a new private file. The file is complete before the account database
    /// is locked, so a large attachment does not hold up other work.
    pub fn save_media_asset_streamed(
        &self,
        account: &str,
        id: &str,
        metadata: &Value,
        fill: impl FnOnce(&mut std::fs::File) -> Result<()>,
    ) -> Result<MediaAssetRecord> {
        let files = self.media_directory(account)?;
        let file = media_files::write_with(&files, fill)?;
        let saved = self.save_media_asset_row(account, id, metadata, Some(&file));
        if saved.is_err() {
            media_files::remove(&files, &file);
        }
        saved
    }
    /// Save an asset row. `written` names a payload file already published for
    /// it; the caller removes that file if this fails.
    fn save_media_asset_row(
        &self,
        account: &str,
        id: &str,
        metadata: &Value,
        written: Option<&str>,
    ) -> Result<MediaAssetRecord> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let tx = db.transaction()?;
            let old = read_asset(&tx, id)?;
            if metadata["state"] == "available" && written.is_none() {
                let cached: bool = tx.query_row(
                    "SELECT payload IS NOT NULL OR payload_file IS NOT NULL OR profile_key IS NOT NULL FROM media_assets WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                if !cached {
                    bail!("media_bytes_missing");
                }
            }
            let now = chrono::Utc::now().to_rfc3339();
            let mut replaced = None;
            let saved = (|| -> Result<MediaAssetRecord> {
                if let Some(file) = written {
                    replaced = tx.query_row(
                        "SELECT payload_file FROM media_assets WHERE id=?1",
                        [id],
                        |r| r.get::<_, Option<String>>(0),
                    )?;
                    tx.execute(
                        "UPDATE media_assets SET metadata_json=?2,payload=NULL,payload_file=?3,profile_key=NULL,updated_at=?4 WHERE id=?1",
                        params![id, serde_json::to_string(metadata)?, file, now],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE media_assets SET metadata_json=?2,updated_at=?3 WHERE id=?1",
                        params![id, serde_json::to_string(metadata)?, now],
                    )?;
                }
                let row = read_asset(&tx, id)?;
                if old.metadata["state"] != "available" && metadata["state"] == "available" {
                    media_event(
                        &tx,
                        "media.ready",
                        &json!({"assetId":id,"jobId":metadata["jobId"],"accountId":account}),
                    )?;
                }
                Ok(row)
            })();
            let committed = saved.and_then(|row| {
                tx.commit()?;
                Ok(row)
            });
            if committed.is_ok() {
                if let Some(file) = replaced.filter(|old| Some(old.as_str()) != written) {
                    media_files::remove(&files, &file);
                }
            }
            committed
        })
    }
    /// Rows that still carry a payload BLOB written by an earlier version, as
    /// `(family, id)` with family `asset` or `source`, at most `limit`.
    pub fn embedded_media_payloads(
        &self,
        account: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        anyhow::ensure!((1..=1000).contains(&limit), "invalid_limit");
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT family,id FROM (SELECT 'asset' AS family,id FROM media_assets WHERE payload IS NOT NULL UNION ALL SELECT 'source' AS family,id FROM media_sources WHERE payload_file IS NULL) ORDER BY family,id LIMIT ?1",
            )?;
            let rows = q.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    /// Move one earlier payload BLOB into a file without changing its bytes.
    /// Returns false when the row has no BLOB left to move.
    pub fn externalize_media_payload(&self, account: &str, family: &str, id: &str) -> Result<bool> {
        let (table, embedded, cleared) = match family {
            "asset" => ("media_assets", "payload IS NOT NULL", "NULL"),
            "source" => ("media_sources", "payload_file IS NULL", "x''"),
            _ => bail!("invalid media payload family"),
        };
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let sealed: Option<Vec<u8>> = db
                .query_row(
                    &format!("SELECT payload FROM {table} WHERE id=?1 AND {embedded}"),
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(sealed) = sealed else {
                return Ok(false);
            };
            let file = media_files::write(&files, &sealed)?;
            if let Err(error) = db.execute(
                &format!("UPDATE {table} SET payload={cleared},payload_file=?2 WHERE id=?1"),
                params![id, file],
            ) {
                media_files::remove(&files, &file);
                return Err(error.into());
            }
            Ok(true)
        })
    }
    /// Delete payload files that no row names, such as one written just before
    /// a crash. Returns how many were removed.
    pub fn sweep_media_files(&self, account: &str) -> Result<usize> {
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT payload_file FROM media_assets WHERE payload_file IS NOT NULL UNION ALL SELECT payload_file FROM media_sources WHERE payload_file IS NOT NULL",
            )?;
            let referenced = q
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            media_files::sweep(&files, &referenced)
        })
    }
    /// Return space freed by moved or deleted payloads to the file system.
    ///
    /// A database not yet in incremental mode is rewritten once at least
    /// 16 MiB and a quarter of it is unused; that rewrite also switches it to
    /// incremental mode. Afterwards each call releases up to 64 MiB of unused
    /// pages once 4 MiB are free, without rewriting the file. A rewrite needs
    /// free disk space for a full copy and its journal; without it the
    /// database is left as it is and `control_compaction_needs_space` is
    /// returned. Returns whether space was given back.
    pub fn compact_control(&self, account: &str) -> Result<bool> {
        const MIB: i64 = 1024 * 1024;
        if self.is_read_snapshot() {
            return Ok(false);
        }
        let directory = self.account_dir(account)?.join("agent");
        self.control(account, |db| {
            let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0))?;
            let free: i64 = db.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
            let size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0))?;
            let incremental: i64 = db.pragma_query_value(None, "auto_vacuum", |r| r.get(0))?;
            if incremental == 2 {
                if free.saturating_mul(size) < 4 * MIB {
                    return Ok(false);
                }
                // Each step of this statement frees one page.
                let mut release = db.prepare(&format!(
                    "PRAGMA incremental_vacuum({})",
                    64 * MIB / size.max(512)
                ))?;
                let mut rows = release.query([])?;
                while rows.next()?.is_some() {}
                drop(rows);
                drop(release);
                checkpoint(db)?;
                return Ok(true);
            }
            if free.saturating_mul(size) < 16 * MIB || free < pages / 4 {
                return Ok(false);
            }
            let needed =
                u64::try_from(pages.saturating_mul(size).saturating_mul(2)).unwrap_or(u64::MAX);
            if fs2::available_space(&directory)? < needed {
                bail!("control_compaction_needs_space");
            }
            db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;VACUUM;")?;
            checkpoint(db)?;
            Ok(true)
        })
    }
    /// Send jobs to consider when deleting uploads that are no longer needed, as
    /// `(job ID, finished)`. A finished job ended at least `settled_seconds` ago
    /// with nothing left to retry, review or write to the profile, and its
    /// upload still exists. Every other send job is listed as unfinished.
    pub fn send_jobs_for_source_release(
        &self,
        account: &str,
        settled_seconds: i64,
    ) -> Result<Vec<(String, bool)>> {
        anyhow::ensure!(settled_seconds >= 0, "invalid_limit");
        let before = (chrono::Utc::now() - chrono::Duration::seconds(settled_seconds)).to_rfc3339();
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT id,finished FROM (SELECT j.id AS id,(j.retryable=0 AND j.updated_at<=?1 AND (j.state IN ('failed','cancelled') OR (j.state='needs_review' AND coalesce(j.error_code,'')='review_expired') OR (j.state='accepted' AND coalesce(json_extract(j.data_json,'$.result.profileProjection'),'') IN ('complete','not_applicable') AND coalesce(json_extract(j.data_json,'$.result.officialMediaWriteback'),'') IN ('complete','not_applicable')))) AS finished,json_extract(j.data_json,'$.sourceId') AS source FROM media_jobs j WHERE j.kind='send') WHERE finished=0 OR EXISTS(SELECT 1 FROM media_sources s WHERE s.id=source) ORDER BY id",
            )?;
            let rows = q.query_map([before], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    /// Sources referenced by sticker preparations or sends, paired with whether
    /// the operation finished before the upload grace period. Uncertain sends
    /// and accepted sends awaiting profile writeback retain their inputs.
    pub fn sticker_preparation_sources_for_release(
        &self,
        account: &str,
        settled_seconds: i64,
    ) -> Result<Vec<(String, bool)>> {
        anyhow::ensure!(settled_seconds >= 0, "invalid_limit");
        let before = (chrono::Utc::now() - chrono::Duration::seconds(settled_seconds)).to_rfc3339();
        self.control(account, |db| {
            let mut query = db.prepare("SELECT json_extract(data_json,'$.sourceId'), (retryable=0 AND updated_at<=?1 AND (((state IN ('failed','cancelled','unsupported','expired') OR (state='available' AND (kind='sticker_prepare' OR json_extract(data_json,'$.profileWriteback')='complete'))) AND (coalesce(json_extract(data_json,'$.serverAcceptance'),'')!='accepted' OR coalesce(json_extract(data_json,'$.profileWriteback'),'')='complete')) OR (state='needs_review' AND coalesce(error_code,'')='review_expired'))) FROM media_jobs WHERE kind IN ('sticker_prepare','sticker_send') AND json_type(data_json,'$.sourceId')='text'")?;
            let rows = query.query_map([before], |r| Ok((r.get(0)?,r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
    /// Release expired local preparation outputs, retaining job/idempotency
    /// receipts. A pending operation naming the preparation keeps its bytes.
    /// This never touches official-profile files or performs network work.
    pub fn expire_sticker_preparation_assets(
        &self,
        account: &str,
        now: i64,
        limit: u32,
    ) -> Result<usize> {
        anyhow::ensure!((1..=1000).contains(&limit), "invalid_limit");
        let files = self.media_directory(account)?;
        self.control(account, |db| {
            let tx = db.transaction()?;
            let expired = {
                let mut query = tx.prepare("SELECT a.id,a.payload_file,a.metadata_json FROM media_assets a WHERE json_extract(a.metadata_json,'$.preparationExpiresAt')<=?1 AND json_extract(a.metadata_json,'$.state')='available' AND NOT EXISTS(SELECT 1 FROM media_jobs j WHERE json_extract(j.data_json,'$.preparationId')=json_extract(a.metadata_json,'$.jobId') AND (j.state IN ('queued','running') OR (j.state='needs_review' AND coalesce(j.error_code,'')!='review_expired') OR j.retryable=1 OR (j.kind IN ('sticker_saved','sticker_acquire') AND length(j.payload)>0 AND json_extract(j.data_json,'$.serverAcceptance')='accepted' AND coalesce(json_extract(j.data_json,'$.profileWriteback'),'')!='complete'))) LIMIT ?2")?;
                let rows = query.query_map(params![now,limit], |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (id,_,metadata) in &expired {
                let mut metadata: Value = serde_json::from_str(metadata)?;
                metadata["state"] = "expired".into(); metadata["reason"] = "sticker_preparation_expired".into(); metadata["contentUrl"] = Value::Null;
                tx.execute("UPDATE media_assets SET metadata_json=?2,payload=NULL,payload_file=NULL,updated_at=?3 WHERE id=?1",params![id,serde_json::to_string(&metadata)?,chrono::Utc::now().to_rfc3339()])?;
            }
            tx.commit()?;
            for (_,file,_) in &expired { if let Some(file) = file { media_files::remove(&files,file); } }
            Ok(expired.len())
        })
    }
    /// Retained message batches for the voice fallback: the same bounded window
    /// as recall evidence. Older batches have already written their inline
    /// voice to the profile, and retention drops their payloads.
    pub fn media_sync_receipts(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.recent_message_batches(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Store) {
        let t = tempfile::tempdir().unwrap();
        let s = Store::open(&t.path().join("data")).unwrap();
        s.create_account("test").unwrap();
        (t, s)
    }
    #[test]
    fn progress_updates_replace_the_previous_progress_event_without_reusing_sequences() {
        let (_dir, s) = setup();
        // An unrelated first event, so the job's events are never the oldest.
        s.append_event_once("test", "first", "account.push", &json!({}))
            .unwrap();
        let (mut row, _) = s
            .create_media_job(
                "test",
                "job",
                "download:asset",
                &"0".repeat(64),
                "download",
                &json!({"assetId":"asset","completedBytes":0}),
                b"",
            )
            .unwrap();
        let mut seen = 0;
        let mut save = |row: &mut MediaJobRecord, change: &dyn Fn(&mut MediaJobRecord)| {
            change(row);
            *row = s.save_media_job("test", row).unwrap();
            let events = s.events("test", 0, 200).unwrap();
            let newest: i64 = events.last().unwrap().cursor.parse().unwrap();
            assert!(newest > seen, "sequence reused");
            seen = newest;
            events
                .into_iter()
                .filter(|event| event.kind == "job.updated")
                .map(|event| {
                    (
                        event.data["state"].clone(),
                        event.data["completedBytes"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        save(&mut row, &|row| row.state = "running".into());
        let running_cursor = s
            .events("test", 0, 200)
            .unwrap()
            .last()
            .unwrap()
            .cursor
            .parse::<i64>()
            .unwrap();
        for bytes in [10, 20, 30] {
            save(&mut row, &|row| row.data["completedBytes"] = bytes.into());
        }
        let events = save(&mut row, &|row| row.state = "available".into());
        // One event per state: progress replaced its predecessor, including
        // the running transition, whose state the newest progress repeats.
        assert_eq!(
            events,
            vec![
                (json!("queued"), json!(0)),
                (json!("running"), json!(30)),
                (json!("available"), json!(30)),
            ]
        );
        // A reader whose cursor is the deleted running event still gets the
        // newer progress event and the completion.
        let later = s.events("test", running_cursor, 200).unwrap();
        assert_eq!(
            later
                .iter()
                .map(|event| event.data["state"].clone())
                .collect::<Vec<_>>(),
            vec![json!("running"), json!("available")]
        );
    }

    #[test]
    fn the_log_keeps_its_row_limit_when_merged_progress_leaves_gaps() {
        let (_dir, s) = setup();
        let (mut row, _) = s
            .create_media_job(
                "test",
                "job",
                "download:asset",
                &"0".repeat(64),
                "download",
                &json!({"assetId":"asset","completedBytes":0}),
                b"",
            )
            .unwrap();
        let bulk = |count: i64| {
            s.control("test", |db| {
                let tx = db.transaction()?;
                for n in 0..count {
                    tx.execute(
                        "INSERT INTO events VALUES((SELECT MAX(sequence)+1 FROM events),'account.push',?1,'2026-01-01T00:00:00+00:00')",
                        [format!("{{\"n\":{n}}}")],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .unwrap();
        };
        bulk(9_000);
        // Progress saves use up sequence numbers but leave one row, so the
        // window would hold fewer rows if it were cut by sequence distance.
        for bytes in 1..=300 {
            row.data["completedBytes"] = bytes.into();
            row = s.save_media_job("test", &row).unwrap();
        }
        bulk(1_500);
        row.data["completedBytes"] = 301.into();
        s.save_media_job("test", &row).unwrap();
        let count: i64 = s
            .control("test", |db| {
                Ok(db.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, crate::EVENT_LIMIT);
    }

    #[test]
    fn the_oldest_event_is_never_merged_away() {
        let (_dir, s) = setup();
        let (mut row, _) = s
            .create_media_job(
                "test",
                "job",
                "download:asset",
                &"0".repeat(64),
                "download",
                &json!({"assetId":"asset","completedBytes":0}),
                b"",
            )
            .unwrap();
        let first: i64 = s.events("test", 0, 10).unwrap()[0].cursor.parse().unwrap();
        row.data["completedBytes"] = 5.into();
        s.save_media_job("test", &row).unwrap();
        // Deleting the first event would make a cursor just before it expire.
        assert!(s.events("test", first - 1, 10).is_ok());
        assert_eq!(s.events("test", 0, 10).unwrap().len(), 2);
    }

    #[test]
    fn compaction_returns_space_once_embedded_payloads_have_moved_to_files() {
        let (dir, s) = setup();
        let database = dir.path().join("data/accounts/test/agent/control.sqlite");
        let payload = vec![7_u8; 24 * 1024 * 1024];
        // A row as written before payload files existed.
        s.control("test", |db| {
            db.execute(
                "INSERT INTO media_sources(id,name,media_type,byte_length,sha256,payload,created_at) VALUES('old','old.bin','application/octet-stream',1,?1,?2,'2020-01-01T00:00:00+00:00')",
                params!["a".repeat(64), payload],
            )?;
            Ok(())
        })
        .unwrap();
        let before = fs::metadata(&database).unwrap().len();
        assert!(before > payload.len() as u64);
        assert_eq!(s.media_source("test", "old").unwrap().1, payload);
        assert!(!s.compact_control("test").unwrap());
        assert_eq!(
            s.embedded_media_payloads("test", 10).unwrap(),
            [("source".to_string(), "old".to_string())]
        );
        assert!(s
            .externalize_media_payload("test", "source", "old")
            .unwrap());
        assert!(!s
            .externalize_media_payload("test", "source", "old")
            .unwrap());
        assert!(s.externalize_media_payload("test", "other", "old").is_err());
        assert!(s.embedded_media_payloads("test", 10).unwrap().is_empty());
        // The pages are free but the file has not shrunk until it is compacted.
        assert_eq!(fs::metadata(&database).unwrap().len(), before);
        assert!(s.compact_control("test").unwrap());
        assert!(fs::metadata(&database).unwrap().len() < 1024 * 1024);
        assert!(!s.compact_control("test").unwrap());
        assert_eq!(s.media_source("test", "old").unwrap().1, payload);
        drop(s);
        let s = Store::open_existing(&dir.path().join("data")).unwrap();
        assert_eq!(s.media_source("test", "old").unwrap().1, payload);
        assert!(s.delete_media_source("test", "old").unwrap());
        assert!(!s.delete_media_source("test", "old").unwrap());
        assert_eq!(
            fs::read_dir(dir.path().join("data/accounts/test/agent/media"))
                .unwrap()
                .count(),
            0
        );
    }
    #[test]
    fn completed_upload_retry_cas_rejects_job_source_and_authorization_races() {
        for change in [
            "none",
            "payload",
            "data",
            "phase",
            "attempts",
            "source",
            "request",
            "authorization",
            "epoch",
            "accepted",
            "ownership",
            "receipt",
            "malformed_root",
            "malformed_result",
        ] {
            let (_dir, s) = setup();
            let authorization = b"accepted encrypted credentials";
            let accepted = format!("{:x}", Sha256::digest(authorization));
            for (key, value) in [
                ("account-state", authorization.as_slice()),
                ("saved-login-accepted-state-generation", accepted.as_bytes()),
                ("desired", b"connected".as_slice()),
                ("validated-generation", b"owner".as_slice()),
            ] {
                s.put_runtime("test", key, value).unwrap();
            }
            s.create_media_source(
                "test",
                "source",
                "file.bin",
                "application/octet-stream",
                16,
                &"a".repeat(64),
                b"sealed source",
            )
            .unwrap();
            let source = s.media_source_metadata("test", "source").unwrap();
            s.create_media_job(
                "test",
                "job",
                "send:key",
                &"b".repeat(64),
                "send",
                &json!({"result":{"cdnDownloadFailure":{"kind":"unknown_field"}}}),
                b"original sealed plan",
            )
            .unwrap();
            assert!(s.claim_media_job("test", "job").unwrap());
            let mut row = s.media_job("test", "job").unwrap();
            row.state = "unsupported".into();
            row.phase = "readback".into();
            row.error_code = Some("unsupported_media".into());
            let expected = s.save_media_job("test", &row).unwrap();
            match change {
                "none"=>{},
                "authorization"=>s.put_runtime("test","account-state",b"changed accepted credentials").unwrap(),
                "epoch"=>s.put_runtime("test","logout-epoch",b"new login").unwrap(),
                "accepted"=>s.put_runtime("test","saved-login-accepted-state-generation",b"different").unwrap(),
                "ownership"=>s.put_runtime("test","validated-generation",b"different owner").unwrap(),
                _=>s.control("test",|db|{
                    let sql=match change {
                        "payload"=>"UPDATE media_jobs SET payload=x'01' WHERE id='job'",
                        "data"=>"UPDATE media_jobs SET data_json='{}' WHERE id='job'",
                        "phase"=>"UPDATE media_jobs SET phase='submitting' WHERE id='job'",
                        "attempts"=>"UPDATE media_jobs SET attempts=attempts+1 WHERE id='job'",
                        "source"=>"UPDATE media_sources SET sha256='changed' WHERE id='source'",
                        "request"=>"UPDATE media_jobs SET request_hash='changed' WHERE id='job'",
                        "receipt"=>r#"UPDATE media_jobs SET data_json='{"serverMessageId":"accepted"}' WHERE id='job'"#,
                        "malformed_root"=>"UPDATE media_jobs SET data_json='[]' WHERE id='job'",
                        "malformed_result"=>r#"UPDATE media_jobs SET data_json='{"result":"unexpected"}' WHERE id='job'"#,
                        _=>unreachable!(),
                    };
                    db.execute(sql,[])?;Ok(())
                }).unwrap(),
            }
            // Give malformed rows a current snapshot, so their rejection tests
            // shape admission rather than only the stale-row comparison.
            let expected = if change.starts_with("malformed_") {
                s.media_job("test", "job").unwrap()
            } else {
                expected
            };
            let request_hash = "b".repeat(64);
            let retry = || CompletedUploadRetry {
                expected: &expected,
                source: &source,
                request_hash: &request_hash,
                expected_authorization: authorization,
                expected_logout_epoch: None,
                ownership_generation: "owner",
                payload: b"new protected reuse plan",
            };
            let before = s.events("test", 0, 100).unwrap().len();
            let outcome = s.retry_media_job_completed_upload("test", retry());
            if change == "none" {
                let queued = outcome.unwrap();
                assert_eq!(queued.state, "queued");
                assert_eq!(queued.phase, "readback");
                assert_eq!(queued.attempts, 1);
                assert_eq!(queued.payload, b"new protected reuse plan");
                assert_eq!(
                    queued.data["result"]["cdnDownloadFailure"]["kind"],
                    "unknown_field"
                );
                assert_eq!(s.events("test", 0, 100).unwrap().len(), before + 1);
                assert!(s.retry_media_job_completed_upload("test", retry()).is_err());
                assert_eq!(s.events("test", 0, 100).unwrap().len(), before + 1);
                assert!(s.claim_media_job("test", "job").unwrap());
                assert!(!s.claim_media_job("test", "job").unwrap());
                assert_eq!(s.media_job("test", "job").unwrap().attempts, 2);
            } else {
                assert!(outcome.is_err(), "{change}");
                assert_ne!(
                    s.media_job("test", "job").unwrap().state,
                    "queued",
                    "{change}"
                );
                assert_eq!(s.events("test", 0, 100).unwrap().len(), before, "{change}");
            }
        }
    }
    #[test]
    fn local_writeback_candidates_are_bounded_and_require_cached_or_accepted_bytes() {
        let (_dir, s) = setup();
        for (id, state, official, payload) in [
            ("a", "available", "partial", Some(b"sealed".to_vec())),
            ("b", "pending", "pending", None),
            ("c", "available", "complete", Some(b"sealed".to_vec())),
        ] {
            s.put_media_asset(
                "test",
                &MediaAssetRecord {
                    id: id.into(),
                    message_id: None,
                    variant: "original".into(),
                    kind: "file".into(),
                    metadata: json!({"state":state,"officialWriteback":official}),
                    plan: b"sealed".to_vec(),
                    payload,
                },
            )
            .unwrap();
        }
        for (id, state, data) in [
            (
                "j1",
                "accepted",
                json!({"result":{"officialMediaWriteback":"partial"}}),
            ),
            (
                "j2",
                "failed",
                json!({"result":{"chunks":[{"state":"accepted"}],"officialMediaWriteback":"blocked"}}),
            ),
            (
                "j3",
                "failed",
                json!({"result":{"chunks":[],"officialMediaWriteback":"blocked"}}),
            ),
        ] {
            let (mut row, _) = s
                .create_media_job(
                    "test",
                    id,
                    &format!("send:{id}"),
                    &"0".repeat(64),
                    "send",
                    &data,
                    b"sealed",
                )
                .unwrap();
            row.state = state.into();
            s.save_media_job("test", &row).unwrap();
        }
        let found = s.media_writeback_candidates("test", 100).unwrap();
        assert_eq!(found.len(), 3);
        assert!(found.contains(&("asset".into(), "a".into())));
        assert!(found.contains(&("job".into(), "j1".into())));
        assert!(found.contains(&("job".into(), "j2".into())));
        assert_eq!(s.media_writeback_candidates("test", 1).unwrap().len(), 1);
        assert!(s.media_writeback_candidates("test", 0).is_err());
        assert!(s.media_writeback_candidates("test", 102).is_err());
        assert!(s.queued_media_jobs("test").unwrap().is_empty());
    }
    #[test]
    fn local_writeback_rotation_survives_restart_and_event_failure_does_not_skip() {
        let (dir, s) = setup();
        for (id, kind) in [
            ("000-unsupported", "sticker"),
            ("a", "file"),
            ("b", "video"),
            ("c", "voice"),
        ] {
            s.put_media_asset(
                "test",
                &MediaAssetRecord {
                    id: id.into(),
                    message_id: None,
                    variant: "original".into(),
                    kind: kind.into(),
                    metadata: json!({"state":"available","officialWriteback":"blocked"}),
                    plan: b"sealed".to_vec(),
                    payload: Some(b"sealed".to_vec()),
                },
            )
            .unwrap();
        }
        let next = |s: &Store| s.media_writeback_candidates("test", 1).unwrap()[0].clone();
        assert_eq!(next(&s), ("asset".into(), "a".into()));
        s.record_media_writeback_scan("test", "asset", "a", false)
            .unwrap();
        assert_eq!(next(&s), ("asset".into(), "b".into()));
        drop(s);
        let s = Store::open_existing(&dir.path().join("data")).unwrap();
        assert_eq!(next(&s), ("asset".into(), "b".into()));
        s.control("test",|db|{db.execute_batch("CREATE TRIGGER refuse_repair_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")?;Ok(())}).unwrap();
        assert!(s
            .record_media_writeback_scan("test", "asset", "b", false)
            .is_err());
        assert_eq!(next(&s), ("asset".into(), "b".into()));
        s.control("test", |db| {
            db.execute_batch("DROP TRIGGER refuse_repair_event")?;
            Ok(())
        })
        .unwrap();
        for id in ["b", "c"] {
            s.record_media_writeback_scan("test", "asset", id, false)
                .unwrap();
        }
        assert_eq!(next(&s), ("asset".into(), "a".into()));
        assert!(s.incomplete_media_writeback("test").unwrap());
        assert_eq!(
            s.events("test", 0, 100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "media.writeback_examined")
                .count(),
            3
        );
    }
    #[test]
    fn job_and_media_events_rollback_together_and_retries_do_not_duplicate() {
        let (_dir, s) = setup();
        let (row, _) = s
            .create_media_job(
                "test",
                "job_atomic",
                "key",
                &"0".repeat(64),
                "download",
                &json!({"assetId":"asset_atomic"}),
                b"sealed",
            )
            .unwrap();
        assert_eq!(s.events("test", 0, 100).unwrap().len(), 1);
        assert!(s.claim_media_job("test", &row.id).unwrap());
        let mut current = s.media_job("test", &row.id).unwrap();
        current.phase = "completed".into();
        current.state = "available".into();
        s.control("test",|db|{db.execute_batch("CREATE TRIGGER refuse_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END;")?;Ok(())}).unwrap();
        assert!(s.save_media_job("test", &current).is_err());
        assert_eq!(s.media_job("test", &row.id).unwrap().state, "running");
        assert_eq!(s.events("test", 0, 100).unwrap().len(), 2);
        s.control("test", |db| {
            db.execute_batch("DROP TRIGGER refuse_event")?;
            Ok(())
        })
        .unwrap();
        s.save_media_job("test", &current).unwrap();
        s.save_media_job("test", &current).unwrap();
        assert_eq!(s.events("test", 0, 100).unwrap().len(), 3);
        s.put_media_asset(
            "test",
            &MediaAssetRecord {
                id: "asset_atomic".into(),
                message_id: None,
                variant: "original".into(),
                kind: "file".into(),
                metadata: json!({"state":"pending"}),
                plan: b"sealed".to_vec(),
                payload: None,
            },
        )
        .unwrap();
        s.control("test",|db|{db.execute_batch("CREATE TRIGGER refuse_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'synthetic event failure'); END;")?;Ok(())}).unwrap();
        assert!(s
            .save_media_asset(
                "test",
                "asset_atomic",
                &json!({"state":"available"}),
                Some(b"sealed-cache")
            )
            .is_err());
        let unchanged = s.media_asset("test", "asset_atomic").unwrap();
        assert_eq!(unchanged.metadata["state"], "pending");
        assert!(unchanged.payload.is_none());
        s.control("test", |db| {
            db.execute_batch("DROP TRIGGER refuse_event")?;
            Ok(())
        })
        .unwrap();
        for _ in 0..2 {
            s.save_media_asset(
                "test",
                "asset_atomic",
                &json!({"state":"available"}),
                Some(b"sealed-cache"),
            )
            .unwrap();
        }
        let events = s.events("test", 0, 100).unwrap();
        assert_eq!(events.iter().filter(|e| e.kind == "media.ready").count(), 1);
    }
    #[test]
    fn media_idempotency_cancellation_and_uncertain_recovery() {
        let (_t, s) = setup();
        let hash = "0".repeat(64);
        let (j, new) = s
            .create_media_job(
                "test",
                "job1",
                "send:key",
                &hash,
                "send",
                &Value::Null,
                b"protected",
            )
            .unwrap();
        assert!(new);
        assert_eq!(
            s.create_media_job(
                "test",
                "other",
                "send:key",
                &hash,
                "send",
                &Value::Null,
                b"other"
            )
            .unwrap()
            .0
            .id,
            j.id
        );
        assert!(s
            .create_media_job(
                "test",
                "other",
                "send:key",
                &"1".repeat(64),
                "send",
                &Value::Null,
                b"other"
            )
            .is_err());
        assert!(s.claim_media_job("test", "job1").unwrap());
        let mut row = s.media_job("test", "job1").unwrap();
        row.phase = "submitting".into();
        s.save_media_job("test", &row).unwrap();
        s.recover_media_jobs("test").unwrap();
        assert_eq!(s.media_job("test", "job1").unwrap().state, "needs_review");
        assert!(s.retry_media_job("test", "job1").is_err());
    }
    #[test]
    fn reservation_restart_is_uncertain_while_readback_reuses_its_saved_plan() {
        let (directory, s) = setup();
        for phase in ["reserving", "uploading", "submitting", "readback"] {
            s.create_media_job(
                "test",
                phase,
                &format!("send:{phase}"),
                &"0".repeat(64),
                "send",
                &json!({}),
                b"protected-existing-receipts",
            )
            .unwrap();
            assert!(s.claim_media_job("test", phase).unwrap());
            let mut row = s.media_job("test", phase).unwrap();
            row.phase = phase.into();
            s.save_media_job("test", &row).unwrap();
        }
        s.cancel_media_job("test", "reserving").unwrap();
        assert_eq!(s.recover_media_jobs("test").unwrap(), 4);
        for phase in ["reserving", "uploading", "submitting"] {
            let row = s.media_job("test", phase).unwrap();
            assert_eq!(row.state, "needs_review");
            assert!(!row.retryable);
            assert!(s.retry_media_job("test", phase).is_err());
        }
        assert_eq!(
            s.media_job("test", "reserving")
                .unwrap()
                .error_code
                .as_deref(),
            Some("restart_during_reservation")
        );
        let row = s.media_job("test", "readback").unwrap();
        assert_eq!(row.state, "queued");
        assert_eq!(row.phase, "readback");
        assert_eq!(row.payload, b"protected-existing-receipts");
        drop(s);
        let s = Store::open_existing(&directory.path().join("data")).unwrap();
        assert_eq!(s.recover_media_jobs("test").unwrap(), 0);
        assert_eq!(s.queued_media_jobs("test").unwrap(), vec!["readback"]);
        assert!(s.claim_media_job("test", "readback").unwrap());
        assert!(!s.claim_media_job("test", "readback").unwrap());
    }
    #[test]
    fn retry_guard_rejects_uncertain_phase_even_with_stale_retryable_flag() {
        let (_directory, s) = setup();
        for phase in ["reserving", "uploading", "submitting"] {
            s.create_media_job(
                "test",
                phase,
                &format!("send:{phase}"),
                &"0".repeat(64),
                "send",
                &json!({}),
                b"protected",
            )
            .unwrap();
            let mut row = s.media_job("test", phase).unwrap();
            row.phase = phase.into();
            row.state = "cancelled".into();
            row.retryable = true;
            s.save_media_job("test", &row).unwrap();
            assert!(s.retry_media_job("test", phase).is_err());
        }
    }
    #[test]
    fn queued_cancel_is_durable_and_claim_is_atomic() {
        let (_t, s) = setup();
        s.create_media_job(
            "test",
            "download1",
            "asset:key",
            &"0".repeat(64),
            "download",
            &Value::Null,
            b"protected",
        )
        .unwrap();
        assert_eq!(
            s.cancel_media_job("test", "download1").unwrap().state,
            "cancelled"
        );
        assert!(!s.claim_media_job("test", "download1").unwrap());
        let row = s.retry_media_job("test", "download1").unwrap();
        assert_eq!(row.state, "queued");
        assert!(s.claim_media_job("test", "download1").unwrap());
        assert!(!s.claim_media_job("test", "download1").unwrap());
    }
    #[test]
    fn sticker_send_inputs_survive_uncertain_and_incomplete_writeback() {
        let (_directory, store) = setup();
        for (id, state, writeback, released) in [
            ("uncertain", "needs_review", "pending", false),
            ("writeback", "failed", "blocked", false),
            ("missing", "failed", "", false),
            ("finished", "available", "complete", true),
            ("failed", "failed", "pending", true),
        ] {
            store
                .create_media_job(
                    "test",
                    id,
                    &format!("send:{id}"),
                    &"0".repeat(64),
                    "sticker_send",
                    &json!({"sourceId":format!("source-{id}"),"profileWriteback":if id=="missing" {Value::Null} else {json!(writeback)},"serverAcceptance":if id=="writeback" || id=="missing" {"accepted"} else {"pending"}}),
                    b"protected",
                )
                .unwrap();
            let mut row = store.media_job("test", id).unwrap();
            row.state = state.into();
            row.retryable = false;
            store.save_media_job("test", &row).unwrap();
            let rows = store
                .sticker_preparation_sources_for_release("test", 0)
                .unwrap();
            assert!(rows.contains(&(format!("source-{id}"), released)));
        }
    }
    #[test]
    fn source_limits_and_asset_metadata_do_not_require_payload_copies() {
        let (_t, store) = setup();
        let max = wechat_media::MAX_FILE_BYTES as u64;
        let hash = "0".repeat(64);
        store
            .create_media_source(
                "test",
                "large",
                "large.bin",
                "application/octet-stream",
                max,
                &hash,
                b"opaque source record",
            )
            .unwrap();
        assert_eq!(
            store
                .media_source_metadata("test", "large")
                .unwrap()
                .byte_length,
            max
        );
        assert!(store
            .create_media_source(
                "test",
                "oversized",
                "large.bin",
                "application/octet-stream",
                max + 1,
                &hash,
                b"opaque source record"
            )
            .is_err());
        let payload = vec![9; 128 * 1024];
        let record = MediaAssetRecord {
            id: "cached".into(),
            message_id: Some("message".into()),
            variant: "normal".into(),
            kind: "file".into(),
            metadata: json!({"state":"available"}),
            plan: b"opaque plan".to_vec(),
            payload: Some(payload.clone()),
        };
        assert!(store
            .put_media_asset("test", &record)
            .unwrap()
            .payload
            .is_none());
        assert!(store
            .media_asset("test", "cached")
            .unwrap()
            .payload
            .is_none());
        assert!(store.media_assets("test", "message").unwrap()[0]
            .payload
            .is_none());
        assert!(store
            .save_media_asset(
                "test",
                "cached",
                &json!({"state":"available","officialWriteback":"complete"}),
                None
            )
            .unwrap()
            .payload
            .is_none());
        assert_eq!(
            store
                .media_asset_with_payload("test", "cached")
                .unwrap()
                .payload
                .unwrap(),
            payload
        );
        store
            .control("test", |db| {
                // zeroblob is lazy here: validate SQLite's scalar/record capacity
                // without writing or materializing a maximum-size attachment.
                let length: u64 =
                    db.query_row("SELECT length(zeroblob(?1))", [max + 29], |r| r.get(0))?;
                assert_eq!(length, max + 29);
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn interrupted_pack_membership_is_reviewable_and_not_queued_for_resubmission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("data")).unwrap();
        store.create_account("one").unwrap();
        let hash = "0".repeat(64);
        store
            .create_media_job(
                "one",
                "pack-intent",
                "pack:key",
                &hash,
                "sticker_pack",
                &serde_json::json!({}),
                b"sealed-owned-intent",
            )
            .unwrap();
        assert!(store.claim_media_job("one", "pack-intent").unwrap());
        let mut row = store.media_job("one", "pack-intent").unwrap();
        row.phase = "submitting".into();
        store.save_media_job("one", &row).unwrap();
        store.recover_media_jobs("one").unwrap();
        let row = store.media_job("one", "pack-intent").unwrap();
        assert_eq!(row.state, "needs_review");
        assert!(!row.retryable);
        assert!(store.queued_media_jobs("one").unwrap().is_empty());
        assert!(store.retry_media_job("one", "pack-intent").is_err());
        assert_eq!(
            store
                .find_media_job("one", "pack:key", &hash)
                .unwrap()
                .unwrap()
                .id,
            "pack-intent"
        );
        assert!(store
            .find_media_job("one", "pack:key", &"1".repeat(64))
            .is_err());
    }
}
