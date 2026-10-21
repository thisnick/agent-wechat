use super::*;
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
#[path = "logout.rs"]
mod logout;

pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(include_str!(
        "../migrations/registry/V002__account_settings_tokens.sql"
    ))?;
    tx.commit()?;
    Ok(())
}
pub fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub cursor: String,
    pub kind: String,
    pub data: Value,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    pub account_id: String,
    pub kind: String,
    pub state: String,
    pub result: Value,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendIntent {
    pub id: String,
    pub account_id: String,
    pub destination: String,
    pub state: String,
    pub client_id: String,
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_message_id: Option<String>,
    pub error_code: Option<String>,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessToken {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub digest: String,
    pub accounts: Vec<String>,
    pub scopes: Vec<String>,
    pub expires_at: Option<i64>,
    pub revoked: bool,
}

impl Store {
    pub fn issue_token(
        &self,
        name: &str,
        digest: &str,
        accounts: &[String],
        scopes: &[String],
        expires_at: Option<i64>,
    ) -> Result<AccessToken> {
        if name.is_empty()
            || name.len() > 128
            || accounts.is_empty()
            || accounts.len() > 100
            || scopes.is_empty()
            || scopes.iter().any(|s| {
                !matches!(
                    s.as_str(),
                    "read" | "media" | "send" | "read-state" | "login" | "admin"
                )
            })
        {
            bail!("invalid token grants");
        }
        if expires_at.is_some_and(|t| t <= unix_seconds()) {
            bail!("token expiry must be in the future");
        }
        for account in accounts {
            if account != "*" {
                self.account(account)?;
            }
        }
        let token = AccessToken {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            digest: digest.into(),
            accounts: accounts.to_vec(),
            scopes: scopes.to_vec(),
            expires_at,
            revoked: false,
        };
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        db.execute(
            "INSERT INTO access_tokens VALUES(?1,?2,?3,?4,?5,?6,0)",
            params![
                token.id,
                token.name,
                token.digest,
                serde_json::to_string(accounts)?,
                serde_json::to_string(scopes)?,
                expires_at
            ],
        )?;
        Ok(token)
    }
    pub fn access_tokens(&self) -> Result<Vec<AccessToken>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        let mut q=db.prepare("SELECT id,name,digest,accounts_json,scopes_json,expires_at,revoked FROM access_tokens ORDER BY id")?;
        let rows = q.query_map([], |r| {
            let a: String = r.get(3)?;
            let s: String = r.get(4)?;
            Ok(AccessToken {
                id: r.get(0)?,
                name: r.get(1)?,
                digest: r.get(2)?,
                accounts: serde_json::from_str(&a).unwrap_or_default(),
                scopes: serde_json::from_str(&s).unwrap_or_default(),
                expires_at: r.get(5)?,
                revoked: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn revoke_token(&self, id: &str) -> Result<bool> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        Ok(db.execute("UPDATE access_tokens SET revoked=1 WHERE id=?1", [id])? > 0)
    }
    pub fn account(&self, id: &str) -> Result<Account> {
        self.accounts()?
            .into_iter()
            .find(|a| a.id == id)
            .context("account not found")
    }
    pub fn create_account(&self, id: &str) -> Result<Account> {
        self.create_account_with_alias(id, None)
    }
    pub fn create_account_with_alias(&self, id: &str, alias: Option<&str>) -> Result<Account> {
        let target = self.account_dir(id)?;
        let parent = target.parent().unwrap();
        private_dir(parent)?;
        let stage = wechat_platform::private_tempdir_in(parent, ".account-")?;
        private_dir(&stage.path().join("profile"))?;
        self.register(
            id,
            "managed",
            &target.join("profile"),
            Some((
                stage,
                copy::Manifest {
                    schema_version: 1,
                    entries: vec![],
                },
            )),
            alias,
        )
    }
    pub fn keys_file(&self, id: &str) -> Result<Option<PathBuf>> {
        self.account(id)?;
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        let mut stmt = db.prepare("SELECT keys_file FROM account_settings WHERE account_id=?1")?;
        let mut rows = stmt.query([id])?;
        Ok(rows
            .next()?
            .and_then(|r| r.get::<_, Option<String>>(0).ok().flatten())
            .map(PathBuf::from))
    }
    pub fn set_keys_file(&self, id: &str, path: &Path) -> Result<()> {
        self.account(id)?;
        let path = fs::canonicalize(path)?;
        let m = fs::symlink_metadata(&path)?;
        if !m.is_file() || !wechat_platform::private_path(&path)? {
            bail!("keys file must be an owner-only regular file");
        }
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        db.execute("INSERT INTO account_settings(account_id,keys_file) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET keys_file=excluded.keys_file",params![id,path.to_str().context("keys path must be UTF-8")?])?;
        Ok(())
    }
    pub(crate) fn control<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        self.account(id)?;
        // Serialize sidecar operations through the store lock as well as SQLite.
        let _guard = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        let path = self.account_dir(id)?.join("agent/control.sqlite");
        let m = fs::symlink_metadata(&path)?;
        if !m.is_file() || !wechat_platform::private_path(&path)? {
            bail!("invalid account sidecar permissions");
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if !(1..=4).contains(&version) {
            bail!("unsupported account sidecar schema");
        }
        if version == 1 {
            let tx = db.transaction()?;
            tx.execute_batch(include_str!("../migrations/account/V002__operations.sql"))?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = db.transaction()?;
            tx.execute_batch(include_str!("../migrations/account/V003__media_jobs.sql"))?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = db.transaction()?;
            tx.execute_batch(include_str!(
                "../migrations/account/V004__pagination_views.sql"
            ))?;
            tx.commit()?;
        }
        f(&mut db)
    }
    pub fn protected_state_exists(&self) -> Result<bool> {
        for account in self.accounts()? {
            if self.control(&account.id,|db|Ok(db.query_row("SELECT (SELECT count(*) FROM runtime)+(SELECT count(*) FROM outbox)+(SELECT count(*) FROM apply_intents)+(SELECT count(*) FROM media_sources)+(SELECT count(*) FROM media_jobs)+(SELECT count(*) FROM media_assets)+(SELECT count(*) FROM pagination_views)",[],|r|r.get::<_,i64>(0))?>0))? {return Ok(true);}
        }
        Ok(false)
    }
    pub fn runtime(&self, id: &str, key: &str) -> Result<Option<Vec<u8>>> {
        self.control(id, |db| {
            let mut q = db.prepare("SELECT value FROM runtime WHERE key=?1")?;
            let mut rows = q.query([key])?;
            Ok(match rows.next()? {
                Some(r) => Some(r.get(0)?),
                None => None,
            })
        })
    }
    pub fn put_runtime(&self, id: &str, key: &str, value: &[u8]) -> Result<()> {
        self.control(id,|db|{db.execute("INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;Ok(())})
    }
    pub fn clear_runtime(&self, id: &str, key: &str) -> Result<()> {
        self.control(id, |db| {
            db.execute("DELETE FROM runtime WHERE key=?1", [key])?;
            Ok(())
        })
    }
    pub fn append_event(&self, id: &str, kind: &str, data: &Value) -> Result<Event> {
        self.control(id, |db| {
            let tx = db.transaction()?;
            let seq: i64 =
                tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events", [], |r| {
                    r.get(0)
                })?;
            let now = chrono::Utc::now().to_rfc3339();
            tx.execute(
                "INSERT INTO events VALUES(?1,?2,?3,?4)",
                params![seq, kind, serde_json::to_string(data)?, now],
            )?;
            tx.execute(
                "DELETE FROM events WHERE sequence < ?1",
                [seq.saturating_sub(10_000)],
            )?;
            tx.commit()?;
            Ok(Event {
                cursor: seq.to_string(),
                kind: kind.into(),
                data: data.clone(),
                created_at: now,
            })
        })
    }
    pub fn events(&self, id: &str, after: i64, limit: usize) -> Result<Vec<Event>> {
        self.control(id,|db|{
        if after<0 || !(1..=200).contains(&limit) {bail!("invalid event cursor/limit");}
        let earliest:Option<i64>=db.query_row("SELECT MIN(sequence) FROM events",[],|r|r.get(0))?;
        if earliest.is_some_and(|first|after<first-1) {bail!("event_cursor_expired");}
        let mut q=db.prepare("SELECT sequence,kind,data_json,created_at FROM events WHERE sequence>?1 ORDER BY sequence LIMIT ?2")?;
        let rows=q.query_map(params![after,limit as i64],|r|{let raw:String=r.get(2)?;Ok(Event {cursor:r.get::<_,i64>(0)?.to_string(),kind:r.get(1)?,data:serde_json::from_str(&raw).unwrap_or(Value::Null),created_at:r.get(3)?})})?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })
    }
    pub fn acknowledge(&self, id: &str, consumer: &str, cursor: i64) -> Result<()> {
        self.control(id,|db|{
        if consumer.is_empty() || consumer.len()>128 || cursor<0 {bail!("invalid consumer acknowledgement");}
        let latest:i64=db.query_row("SELECT COALESCE(MAX(sequence),0) FROM events",[],|r|r.get(0))?;
        if cursor>latest {bail!("cannot acknowledge a future cursor");}
        db.execute("INSERT INTO consumer_offsets VALUES(?1,?2) ON CONFLICT(consumer_id) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",params![consumer,cursor])?;Ok(())
    })
    }
    pub fn save_operation(&self, op: &Operation) -> Result<()> {
        self.control(&op.account_id,|db|{
            let tx=db.transaction()?;
            let changed=tx.execute("INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET state=excluded.state,result_json=excluded.result_json,updated_at=excluded.updated_at WHERE operations.state NOT IN ('cancelled','succeeded','failed','expired','rejected','needs_login','needs_review') OR operations.state=excluded.state",params![op.id,op.kind,op.state,serde_json::to_string(&op.result)?,op.created_at,op.updated_at])?;
            if changed==1 {
                tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)",params![serde_json::to_string(&serde_json::json!({"id":op.id,"state":op.state,"kind":op.kind}))?,op.updated_at])?;
                let seq=tx.last_insert_rowid();tx.execute("DELETE FROM events WHERE sequence<=?1",[seq.saturating_sub(10_000)])?;
            }
            tx.commit()?;Ok(())
        })
    }
    pub fn operation(&self, account: &str, id: &str) -> Result<Operation> {
        self.control(account,|db|{
        let (kind,state,result,created,updated):(String,String,String,String,String)=db.query_row("SELECT kind,state,result_json,created_at,updated_at FROM operations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        Ok(Operation {id:id.into(),account_id:account.into(),kind,state,result:serde_json::from_str(&result)?,created_at:created,updated_at:updated})
    })
    }
    /// A restarted owner cannot continue the previous process's network tasks.
    /// Staged profile receipts are recovered separately; no RPC is replayed here.
    pub fn recover_interrupted_operations(&self, account: &str) -> Result<usize> {
        self.control(account, |db| {
            let tx = db.transaction()?;
            let rows = {
                let mut query = tx.prepare("SELECT id,kind FROM operations WHERE kind IN ('login','metadata','sync','media-writeback') AND state NOT IN ('cancelled','succeeded','failed','expired','rejected','needs_login','needs_review')")?;
                let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let now = chrono::Utc::now().to_rfc3339();
            for (id, kind) in &rows {
                tx.execute("UPDATE operations SET state='failed',result_json=?1,updated_at=?2 WHERE id=?3", params![serde_json::to_string(&serde_json::json!({"code":"operation_interrupted","stagedDataRetained":true}))?,now,id])?;
                tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('operation.updated',?1,?2)", params![serde_json::to_string(&serde_json::json!({"id":id,"kind":kind,"state":"failed"}))?,now])?;
            }
            tx.commit()?;
            Ok(rows.len())
        })
    }
    pub fn create_send(
        &self,
        account: &str,
        key: &str,
        destination: &str,
        hash: &str,
        encrypted: &[u8],
        client_id: &str,
    ) -> Result<(SendIntent, bool)> {
        self.control(account, |db| {
            if key.is_empty()
                || key.len() > 200
                || destination.is_empty()
                || destination.len() > 256
            {
                bail!("invalid send intent");
            }
            let tx = db.transaction()?;
            let media_conflict: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM media_jobs WHERE idempotency_key=?1)",
                [format!("send:{key}")],
                |r| r.get(0),
            )?;
            if media_conflict {
                bail!("idempotency_conflict");
            }
            let existing: Option<(String, String)> = {
                let mut q =
                    tx.prepare("SELECT id,payload_hash FROM outbox WHERE idempotency_key=?1")?;
                let mut r = q.query([key])?;
                match r.next()? {
                    Some(row) => Some((row.get(0)?, row.get(1)?)),
                    None => None,
                }
            };
            if let Some((id, old_hash)) = existing {
                if old_hash != hash {
                    bail!("idempotency_conflict");
                }
                let intent = read_send(&tx, account, &id)?;
                return Ok((intent, false));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            tx.execute(
                "INSERT INTO outbox VALUES(?1,?2,?3,?4,?5,'queued',?6,NULL,NULL,?7)",
                params![id, key, hash, destination, encrypted, client_id, now],
            )?;
            let intent = read_send(&tx, account, &id)?;
            tx.commit()?;
            Ok((intent, true))
        })
    }
    pub fn send(&self, account: &str, id: &str) -> Result<SendIntent> {
        self.control(account, |db| read_send(db, account, id))
    }
    pub fn queued_sends(&self, account: &str) -> Result<Vec<String>> {
        self.control(account, |db| {
            let mut q = db.prepare(
                "SELECT id FROM outbox WHERE state='queued' ORDER BY created_at,id LIMIT 100",
            )?;
            let rows = q.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
    pub fn send_payload(&self, account: &str, id: &str) -> Result<Vec<u8>> {
        self.control(account, |db| {
            Ok(db.query_row("SELECT payload FROM outbox WHERE id=?1", [id], |r| r.get(0))?)
        })
    }
    pub fn update_send(
        &self,
        account: &str,
        id: &str,
        state: &str,
        server_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<SendIntent> {
        self.control(account,|db|{
            let tx=db.transaction()?;
            tx.execute("UPDATE outbox SET state=?2,server_id=COALESCE(?3,server_id),error_code=?4 WHERE id=?1 AND state!='cancelled'",params![id,state,server_id,error])?;
            let intent=read_send(&tx,account,id)?;
            tx.execute("INSERT INTO events(kind,data_json,created_at)VALUES('send.updated',?1,?2)",params![serde_json::to_string(&intent)?,chrono::Utc::now().to_rfc3339()])?;
            let seq=tx.last_insert_rowid();tx.execute("DELETE FROM events WHERE sequence<=?1",[seq.saturating_sub(10_000)])?;
            tx.commit()?;Ok(intent)
        })
    }
    pub fn recover_uncertain_sends(&self, account: &str) -> Result<usize> {
        self.control(account,|db|Ok(db.execute("UPDATE outbox SET state='needs_review',error_code='restart_after_submission' WHERE state='submitting'",[])?))
    }
    /// The received checkpoint means the original commands are recoverably
    /// staged, not that every official-file projection has completed.
    pub fn stage_sync(
        &self,
        account: &str,
        id: &str,
        payload: &[u8],
        received_cursor: &[u8],
    ) -> Result<()> {
        self.control(account,|db|{
        let tx=db.transaction()?;tx.execute("INSERT OR IGNORE INTO apply_intents VALUES(?1,?2,'pending',?3)",params![id,payload,chrono::Utc::now().to_rfc3339()])?;
        tx.execute("INSERT INTO runtime VALUES('received-cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[received_cursor])?;
        tx.execute("INSERT INTO runtime VALUES('cursor-projection-needed',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value",[b"1".as_slice()])?;tx.commit()?;Ok(())
    })
    }
    pub fn pending_reads(&self, account: &str) -> Result<usize> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM runtime WHERE key LIKE 'read-pending:%'",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize)
        })
    }
    pub fn pending_sync_count(&self, account: &str) -> Result<usize> {
        self.control(account, |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM apply_intents WHERE state!='projected'",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize)
        })
    }
    pub fn block_sync(&self, account: &str, id: &str) -> Result<()> {
        self.control(account, |db| {
            db.execute("UPDATE apply_intents SET state='blocked' WHERE id=?1", [id])?;
            Ok(())
        })
    }
    pub fn retry_blocked(&self, account: &str) -> Result<()> {
        self.control(account, |db| {
            db.execute(
                "UPDATE apply_intents SET state='pending' WHERE state='blocked'",
                [],
            )?;
            Ok(())
        })
    }
    pub fn pending_sync(&self, account: &str) -> Result<Vec<(String, Vec<u8>)>> {
        self.control(account,|db|{
        let mut q=db.prepare("SELECT id,payload FROM apply_intents WHERE state='pending' ORDER BY created_at,id LIMIT 100")?;
        let rows=q.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?;Ok(rows.collect::<rusqlite::Result<_>>()?)
    })
    }
    pub fn append_event_once(
        &self,
        id: &str,
        key: &str,
        kind: &str,
        data: &Value,
    ) -> Result<Option<Event>> {
        self.control(id, |db| {
            let tx = db.transaction()?;
            let receipt = format!("event:{key}");
            let exists: i64 = tx.query_row(
                "SELECT count(*) FROM runtime WHERE key=?1",
                [&receipt],
                |r| r.get(0),
            )?;
            if exists != 0 {
                return Ok(None);
            }
            let seq: i64 =
                tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM events", [], |r| {
                    r.get(0)
                })?;
            let now = chrono::Utc::now().to_rfc3339();
            tx.execute(
                "INSERT INTO events VALUES(?1,?2,?3,?4)",
                params![seq, kind, serde_json::to_string(data)?, now],
            )?;
            tx.execute(
                "INSERT INTO runtime VALUES(?1,?2)",
                params![receipt, seq.to_string().as_bytes()],
            )?;
            tx.execute(
                "DELETE FROM events WHERE sequence < ?1",
                [seq.saturating_sub(10_000)],
            )?;
            tx.commit()?;
            Ok(Some(Event {
                cursor: seq.to_string(),
                kind: kind.into(),
                data: data.clone(),
                created_at: now,
            }))
        })
    }
    pub fn read_version(&self, id: &str, thread: &str) -> Result<String> {
        let key = format!("read-version:{thread}");
        Ok(self
            .runtime(id, &key)?
            .and_then(|v| String::from_utf8(v).ok())
            .unwrap_or_else(|| "0".into()))
    }
    pub fn begin_read(
        &self,
        id: &str,
        thread: &str,
        expected: &str,
        intent: &Value,
    ) -> Result<String> {
        self.control(id, |db| {
            let tx = db.transaction()?;
            let key = format!("read-version:{thread}");
            let old = tx
                .query_row("SELECT value FROM runtime WHERE key=?1", [&key], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .optional()?
                .and_then(|v| String::from_utf8(v).ok())
                .unwrap_or_else(|| "0".into());
            if old != expected {
                bail!("read_state_conflict");
            }
            let pending = format!("read-pending:{thread}");
            let existing = tx
                .query_row("SELECT value FROM runtime WHERE key=?1", [&pending], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .optional()?;
            let bytes = serde_json::to_vec(intent)?;
            if existing.as_ref().is_some_and(|v| v != &bytes) {
                bail!("read_state_conflict");
            }
            let next = old
                .parse::<u64>()?
                .checked_add(1)
                .context("read version exhausted")?
                .to_string();
            tx.execute(
                "INSERT OR IGNORE INTO runtime VALUES(?1,?2)",
                params![pending, bytes],
            )?;
            tx.commit()?;
            Ok(next)
        })
    }
    pub fn finish_read(&self, id: &str, thread: &str, next: &str) -> Result<()> {
        self.control(id,|db|{
        let tx=db.transaction()?;tx.execute("INSERT INTO runtime VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("read-version:{thread}"),next.as_bytes()])?;
        tx.execute("DELETE FROM runtime WHERE key=?1",[format!("read-pending:{thread}")])?;tx.commit()?;Ok(())
    })
    }
    pub fn finish_sync(&self, account: &str, id: &str) -> Result<()> {
        self.control(account, |db| {
            db.execute(
                "UPDATE apply_intents SET state='projected' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
    }
}
pub(crate) fn read_send(db: &Connection, account: &str, id: &str) -> Result<SendIntent> {
    let local: Option<Vec<u8>> = db
        .query_row(
            "SELECT value FROM runtime WHERE key=?1",
            [format!("send-projected:{id}")],
            |r| r.get(0),
        )
        .optional()?;
    let local_message_id = local
        .map(String::from_utf8)
        .transpose()
        .context("invalid text projection receipt")?;
    Ok(db.query_row("SELECT destination,state,client_id,server_id,error_code,created_at FROM outbox WHERE id=?1",[id],|r|Ok(SendIntent {id:id.into(),account_id:account.into(),destination:r.get(0)?,state:r.get(1)?,client_id:r.get(2)?,server_id:r.get(3)?,local_message_id,error_code:r.get(4)?,created_at:r.get(5)?}))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Store) {
        let t = tempfile::tempdir().unwrap();
        let store = Store::open(&t.path().join("data")).unwrap();
        store.create_account("test").unwrap();
        (t, store)
    }
    #[test]
    fn durable_outbox_deduplicates_and_fences_uncertain_submissions() {
        let (_t, store) = setup();
        let (first, created) = store
            .create_send(
                "test",
                "key",
                "filehelper",
                "hash",
                b"synthetic encrypted payload",
                "4294967295",
            )
            .unwrap();
        assert!(created);
        let (second, created) = store
            .create_send(
                "test",
                "key",
                "filehelper",
                "hash",
                b"different encryption nonce",
                "4294967295",
            )
            .unwrap();
        assert!(!created);
        assert_eq!(first.id, second.id);
        assert!(store
            .create_send("test", "key", "other", "different hash", b"x", "1")
            .is_err());
        store
            .update_send("test", &first.id, "submitting", None, None)
            .unwrap();
        assert_eq!(store.recover_uncertain_sends("test").unwrap(), 1);
        assert_eq!(store.send("test", &first.id).unwrap().state, "needs_review");
        assert_eq!(store.recover_uncertain_sends("test").unwrap(), 0);
    }
    #[test]
    fn restart_interrupts_only_unfinished_network_operations_atomically() {
        let (_temp, store) = setup();
        for (id, kind, state) in [
            ("active-login", "login", "awaiting_approval"),
            ("active-sync", "sync", "running"),
            ("active-metadata", "metadata", "queued"),
            ("done", "metadata", "succeeded"),
            ("logout", "logout", "running"),
        ] {
            store
                .save_operation(&Operation {
                    id: id.into(),
                    account_id: "test".into(),
                    kind: kind.into(),
                    state: state.into(),
                    result: Value::Null,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                })
                .unwrap();
        }
        store.control("test", |db| { db.execute_batch("CREATE TRIGGER reject_restart_event BEFORE INSERT ON events WHEN NEW.kind='operation.updated' BEGIN SELECT RAISE(ABORT,'event unavailable'); END;")?; Ok(()) }).unwrap();
        assert!(store.recover_interrupted_operations("test").is_err());
        assert_eq!(
            store.operation("test", "active-sync").unwrap().state,
            "running"
        );
        store
            .control("test", |db| {
                db.execute_batch("DROP TRIGGER reject_restart_event;")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(store.recover_interrupted_operations("test").unwrap(), 3);
        assert_eq!(
            store.operation("test", "active-login").unwrap().result["code"],
            "operation_interrupted"
        );
        assert_eq!(store.operation("test", "done").unwrap().state, "succeeded");
        assert_eq!(store.operation("test", "logout").unwrap().state, "running");
        assert_eq!(store.recover_interrupted_operations("test").unwrap(), 0);
    }
    #[test]
    fn staging_and_received_cursor_commit_without_claiming_projection() {
        let (_t, store) = setup();
        store
            .stage_sync("test", "batch", b"encrypted original", b"encrypted cursor")
            .unwrap();
        assert_eq!(
            store.runtime("test", "received-cursor").unwrap().unwrap(),
            b"encrypted cursor"
        );
        assert_eq!(store.pending_sync("test").unwrap().len(), 1);
        store
            .stage_sync("test", "batch", b"new nonce", b"encrypted cursor")
            .unwrap();
        assert_eq!(
            store.pending_sync("test").unwrap()[0].1,
            b"encrypted original"
        );
        store.finish_sync("test", "batch").unwrap();
        assert!(store.pending_sync("test").unwrap().is_empty());
    }
    #[test]
    fn events_are_once_only_and_acknowledgements_are_separate() {
        let (_t, store) = setup();
        assert!(store
            .append_event_once(
                "test",
                "source",
                "message.committed",
                &serde_json::json!({"messageId":"synthetic"})
            )
            .unwrap()
            .is_some());
        assert!(store
            .append_event_once("test", "source", "message.committed", &Value::Null)
            .unwrap()
            .is_none());
        assert_eq!(store.events("test", 0, 100).unwrap().len(), 1);
        store.acknowledge("test", "consumer", 1).unwrap();
        store.acknowledge("test", "consumer", 0).unwrap();
        assert!(store.acknowledge("test", "consumer", 2).is_err());
        assert_eq!(store.read_version("test", "thread").unwrap(), "0");
    }
    #[test]
    fn read_generation_and_recovery_intent_are_compare_and_swap() {
        let (_t, store) = setup();
        let intent = serde_json::json!({"messageIds":["exact source"]});
        assert_eq!(
            store.begin_read("test", "thread", "0", &intent).unwrap(),
            "1"
        );
        assert_eq!(
            store.begin_read("test", "thread", "0", &intent).unwrap(),
            "1"
        );
        assert!(store
            .begin_read("test", "thread", "0", &Value::Null)
            .is_err());
        store.finish_read("test", "thread", "1").unwrap();
        assert!(store.begin_read("test", "thread", "0", &intent).is_err());
        assert_eq!(store.read_version("test", "thread").unwrap(), "1");
    }
    #[test]
    fn blocked_batches_are_retained_without_starving_later_work() {
        let (_t, store) = setup();
        for i in 0..101 {
            store
                .stage_sync("test", &format!("batch-{i:03}"), b"encrypted", b"cursor")
                .unwrap();
        }
        let initial = store.pending_sync("test").unwrap();
        assert_eq!(initial.len(), 100);
        for (id, _) in initial {
            store.block_sync("test", &id).unwrap();
        }
        assert_eq!(store.pending_sync_count("test").unwrap(), 101);
        let remaining = store.pending_sync("test").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "batch-100");
        store.finish_sync("test", &remaining[0].0).unwrap();
        assert_eq!(store.pending_sync_count("test").unwrap(), 100);
        store.retry_blocked("test").unwrap();
        assert_eq!(store.pending_sync("test").unwrap().len(), 100);
    }
    #[test]
    fn settings_and_token_grants_do_not_disclose_hashes() {
        let (_t, store) = setup();
        let grant = store
            .issue_token(
                "synthetic",
                "sha256",
                &["test".into()],
                &["read".into()],
                None,
            )
            .unwrap();
        let json = serde_json::to_string(&grant).unwrap();
        assert!(!json.contains("sha256"));
        assert!(!json.contains("digest"));
        assert!(store
            .issue_token("bad", "hash", &["test".into()], &["unknown".into()], None)
            .is_err());
        assert!(store.revoke_token(&grant.id).unwrap());
        assert!(store.access_tokens().unwrap()[0].revoked);
    }
}
