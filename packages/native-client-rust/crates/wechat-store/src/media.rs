//! Account-scoped durable media jobs. Payloads are opaque vault ciphertext;
//! status/metadata never include URLs, native file IDs or encryption keys.
use crate::*;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use ts_rs::TS;

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct OwnedMediaSource {
    pub id: String,
    pub name: String,
    pub media_type: String,
    #[ts(type = "number")]
    pub byte_length: u64,
    pub sha256: String,
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
fn read_asset(db: &Connection, id: &str) -> Result<MediaAssetRecord> {
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
fn media_event(tx: &Connection, kind: &str, data: &Value) -> Result<()> {
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
    tx.execute(
        "DELETE FROM events WHERE sequence < ?1",
        [seq.saturating_sub(10_000)],
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
fn job_event(tx: &Connection, account: &str, row: &MediaJobRecord) -> Result<()> {
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
    media_event(tx, "job.updated", &data)?;
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
            db.execute(
                "INSERT INTO media_sources VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id, name, media_type, byte_length, sha256, payload, now],
            )?;
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
    pub fn media_source(&self, account: &str, id: &str) -> Result<(OwnedMediaSource, Vec<u8>)> {
        self.control(account,|db|{Ok(db.query_row("SELECT name,media_type,byte_length,sha256,payload,created_at FROM media_sources WHERE id=?1",[id],|r|Ok((OwnedMediaSource{id:id.into(),name:r.get(0)?,media_type:r.get(1)?,byte_length:r.get(2)?,sha256:r.get(3)?,created_at:r.get(5)?},r.get(4)?)))?)})
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
            if key.is_empty() || key.len()>250 || id.is_empty() || id.len()>100 || hash.len()!=64 || !matches!(kind,"download"|"send") { bail!("invalid media job"); }
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

    pub fn media_job(&self, account: &str, id: &str) -> Result<MediaJobRecord> {
        self.control(account, |db| read_job(db, id))
    }
    pub fn media_job_by_key(&self, account: &str, key: &str) -> Result<MediaJobRecord> {
        self.control(account, |db| {
            let id: String = db.query_row(
                "SELECT id FROM media_jobs WHERE idempotency_key=?1",
                [key],
                |r| r.get(0),
            )?;
            read_job(db, &id)
        })
    }
    pub fn pending_media_projections(&self, account: &str) -> Result<Vec<String>> {
        self.control(account,|db|{
            let mut query=db.prepare("SELECT id FROM media_jobs WHERE kind='send' AND state IN ('accepted','failed','cancelled','needs_review') AND coalesce(json_extract(data_json,'$.result.profileProjection'),'') NOT IN ('complete','not_applicable','unknown') ORDER BY created_at,id LIMIT 100")?;
            let rows=query.query_map([],|r|r.get(0))?;Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    /// A committed history row does not prove its cached bytes and resource
    /// metadata reached the official profile. Profile-only copies must report
    /// those gaps, including partial voice jobs with already accepted notes.
    pub fn incomplete_media_writeback(&self, account: &str) -> Result<bool> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_assets WHERE json_extract(metadata_json,'$.state')='available' AND coalesce(json_extract(metadata_json,'$.officialWriteback'),'')!='complete') OR EXISTS(SELECT 1 FROM media_jobs j WHERE j.kind='send' AND (j.state='accepted' OR json_extract(j.data_json,'$.serverMessageId') IS NOT NULL OR EXISTS(SELECT 1 FROM json_each(j.data_json,'$.result.chunks') c WHERE json_extract(c.value,'$.state')='accepted')) AND coalesce(json_extract(j.data_json,'$.result.officialMediaWriteback'),'') NOT IN ('complete','not_applicable'))",
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
            let mut q=db.prepare("SELECT family,id FROM (SELECT 'asset' AS family,id FROM media_assets WHERE kind IN ('image','file','video','voice') AND json_extract(metadata_json,'$.state')='available' AND coalesce(json_extract(metadata_json,'$.officialWriteback'),'')!='complete' UNION ALL SELECT 'job' AS family,id FROM media_jobs j WHERE j.kind='send' AND (j.state='accepted' OR json_extract(j.data_json,'$.serverMessageId') IS NOT NULL OR EXISTS(SELECT 1 FROM json_each(j.data_json,'$.result.chunks') c WHERE json_extract(c.value,'$.state')='accepted')) AND coalesce(json_extract(j.data_json,'$.result.officialMediaWriteback'),'') NOT IN ('complete','not_applicable')) ORDER BY CASE WHEN family>?2 OR (family=?2 AND id>?3) THEN 0 ELSE 1 END,family,id LIMIT ?1")?;
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
                tx.execute("UPDATE media_jobs SET cancel_requested=1,state=CASE WHEN state='queued' THEN 'cancelled' ELSE state END,retryable=CASE WHEN kind='download' THEN 1 ELSE retryable END,updated_at=?2 WHERE id=?1",params![id,chrono::Utc::now().to_rfc3339()])?;
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
            tx.execute("UPDATE media_jobs SET state='needs_review',error_code=CASE WHEN phase='reserving' THEN 'restart_during_reservation' ELSE 'restart_after_submission' END,retryable=0,updated_at=?1 WHERE state='running' AND kind='send' AND phase IN ('reserving','uploading','submitting')",[&now])?;
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
        self.control(account,|db|{db.execute("INSERT INTO media_assets VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO NOTHING",params![asset.id,asset.message_id,asset.variant,asset.kind,serde_json::to_string(&asset.metadata)?,asset.plan,asset.payload,chrono::Utc::now().to_rfc3339()])?;read_asset(db,&asset.id)})
    }
    pub fn media_asset(&self, account: &str, id: &str) -> Result<MediaAssetRecord> {
        self.control(account, |db| read_asset(db, id))
    }
    /// Fetch cached bytes only for content reads or local writeback repair.
    pub fn media_asset_with_payload(&self, account: &str, id: &str) -> Result<MediaAssetRecord> {
        self.control(account, |db| {
            let mut row = read_asset(db, id)?;
            row.payload =
                db.query_row("SELECT payload FROM media_assets WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
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
        self.control(account,|db|{
            let tx=db.transaction()?;let old=read_asset(&tx,id)?;
            if metadata["state"]=="available" && payload.is_none() {
                let cached:bool=tx.query_row("SELECT payload IS NOT NULL FROM media_assets WHERE id=?1",[id],|r|r.get(0))?;
                if !cached {bail!("media_bytes_missing");}
            }
            tx.execute("UPDATE media_assets SET metadata_json=?2,payload=COALESCE(?3,payload),updated_at=?4 WHERE id=?1",params![id,serde_json::to_string(metadata)?,payload,chrono::Utc::now().to_rfc3339()])?;
            let row=read_asset(&tx,id)?;
            if old.metadata["state"]!="available" && metadata["state"]=="available" {media_event(&tx,"media.ready",&json!({"assetId":id,"jobId":metadata["jobId"],"accountId":account}))?;}
            tx.commit()?;Ok(row)
        })
    }
    pub fn media_sync_receipts(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT id,payload FROM apply_intents ORDER BY created_at DESC,id DESC LIMIT 10000",
            )?;
            let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
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
}
