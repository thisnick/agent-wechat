use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;
use wechat_protocol::{AccountState, DeviceProfile, LoginStatus, NativeClient, NativeOptions};
use wechat_store::{vault::Vault, Operation, SendIntent, Store};

pub struct Core {
    pub store: Arc<Store>,
    pub ownership_generation: String,
    leases: Mutex<Vec<wechat_store::Lease>>,
    vault: Arc<Vault>,
    gates: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
    cancels: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    workers: Mutex<std::collections::BTreeSet<String>>,
    pub stopping: AtomicBool,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginRequest {
    #[serde(default = "yes")]
    pub allow_qr: bool,
    #[serde(default)]
    pub force_qr: bool,
    #[serde(default = "deadline")]
    pub timeout_seconds: u64,
    #[serde(default = "computer")]
    pub computer_name: String,
}
fn yes() -> bool {
    true
}
fn deadline() -> u64 {
    300
}
fn computer() -> String {
    "agent-wechat".into()
}
impl Default for LoginRequest {
    fn default() -> Self {
        Self {
            allow_qr: true,
            force_qr: false,
            timeout_seconds: 300,
            computer_name: computer(),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct SendPayload {
    thread_id: String,
    text: String,
    created_at: u32,
    client_id: u32,
}
#[derive(Serialize, Deserialize)]
struct IdentityBinding {
    relative: String,
    format_key: Vec<u8>,
}
impl Drop for IdentityBinding {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.format_key.zeroize();
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn client() -> Result<NativeClient> {
    NativeClient::new(NativeOptions::default())
}
fn auth_rejected(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains(" -13") || message.ends_with(" -13")
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .context("account worker stopped")?
}

impl Core {
    pub fn new(store: Arc<Store>, storage_key: Option<&std::path::Path>) -> Result<Arc<Self>> {
        let default = store.root.join("storage-key");
        let path = storage_key.unwrap_or(&default);
        let vault = Vault::open(
            path,
            storage_key.is_none() && !store.protected_state_exists()?,
        )?;
        let leases = store.profile_leases()?;
        let core = Arc::new(Self {
            store,
            leases: Mutex::new(leases),
            vault: Arc::new(vault),
            ownership_generation: uuid::Uuid::new_v4().to_string(),
            gates: Mutex::new(BTreeMap::new()),
            cancels: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(std::collections::BTreeSet::new()),
            stopping: AtomicBool::new(false),
        });
        for account in core.store.accounts()? {
            if core.store.runtime(&account.id, "account-state")?.is_some() {
                core.state_unfenced(&account.id)?;
            }
            core.store.recover_uncertain_sends(&account.id)?;
        }
        Ok(core)
    }
    pub fn create_account(&self, id: &str) -> Result<wechat_store::Account> {
        let account = self.store.create_account(id)?;
        self.leases
            .lock()
            .map_err(|_| anyhow::anyhow!("profile ownership unavailable"))?
            .push(wechat_store::profile_lease(&account.profile_path)?);
        Ok(account)
    }
    fn gate(&self, account: &str) -> Result<Arc<AsyncMutex<()>>> {
        self.store.account(account)?;
        let mut gates = self
            .gates
            .lock()
            .map_err(|_| anyhow::anyhow!("account manager unavailable"))?;
        Ok(gates
            .entry(account.into())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone())
    }
    pub async fn lock_account(&self, account: &str) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        Ok(self.gate(account)?.lock_owned().await)
    }
    fn state_unfenced(&self, account: &str) -> Result<AccountState> {
        let encrypted = self
            .store
            .runtime(account, "account-state")?
            .context("auth_required")?;
        let plain = self
            .vault
            .open_record(account, "account-state", &encrypted)?;
        let state: AccountState = serde_json::from_slice(&plain)
            .map_err(|_| anyhow::anyhow!("invalid protected account state"))?;
        state.validate()?;
        Ok(state)
    }
    pub fn account_state(&self, account: &str) -> Result<AccountState> {
        if self.store.runtime(account, "logged-out")?.is_some() {
            bail!("auth_required");
        }
        self.state_unfenced(account)
    }
    fn save_state(&self, account: &str, state: &AccountState) -> Result<()> {
        state.validate()?;
        if let Ok(prior) = self.state_unfenced(account) {
            if prior.uin != state.uin || prior.own_username != state.own_username {
                bail!("account_identity_mismatch");
            }
        }
        self.store.put_runtime(
            account,
            "account-state",
            &self
                .vault
                .seal(account, "account-state", &serde_json::to_vec(state)?)?,
        )
    }
    pub fn restore_credentials(&self, account: &str, input: &[u8]) -> Result<()> {
        self.store.account(account)?;
        let state = match serde_json::from_slice::<AccountState>(input) {
            Ok(state) => state,
            Err(_) => AccountState::from_baseline_json(input)?,
        };
        self.save_state(account, &state)?;
        self.store.clear_runtime(account, "validated-generation")?;
        self.store
            .put_runtime(account, "desired", b"disconnected")?;
        self.store.append_event(
            account,
            "account.updated",
            &json!({"accountId":account,"authentication":"saved_unvalidated"}),
        )?;
        Ok(())
    }
    pub fn restore_cursor(&self, account: &str, cursor: &[u8]) -> Result<()> {
        if !cursor.is_empty() {
            wechat_protocol::wire::decode_cursor(cursor)?;
        }
        self.store.put_runtime(
            account,
            "received-cursor",
            &self.vault.seal(account, "received-cursor", cursor)?,
        )
    }
    fn cursor(&self, account: &str) -> Result<Vec<u8>> {
        match self.store.runtime(account, "received-cursor")? {
            Some(v) => Ok(self
                .vault
                .open_record(account, "received-cursor", &v)?
                .to_vec()),
            None => Ok(vec![]),
        }
    }
    pub fn account_status(&self, account: &str) -> Result<Value> {
        let record = self.store.account(account)?;
        let saved = self.account_state(account).is_ok();
        let auth = saved
            && self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                == Some(self.ownership_generation.as_bytes());
        let desired = self.store.runtime(account, "desired")?.unwrap_or_default();
        Ok(
            json!({"id":record.id,"storageMode":record.storage_mode,"schemaCompatibility":record.schema_compatibility,"authenticated":auth,"connection":if auth&&desired==b"connected"{"online"}else if saved&&desired==b"connected"{"reconnecting"}else{"disconnected"},"pendingProjectionBatches":self.store.pending_sync_count(account)?,"pendingReadProjections":self.store.pending_reads(account)?,"nativeRuntime":true}),
        )
    }
    pub fn disconnect(&self, account: &str) -> Result<()> {
        self.store.clear_runtime(account, "validated-generation")?;
        if let Ok(map) = self.cancels.lock() {
            if let Some(cancel) = map.get(&format!("login-active:{account}")) {
                cancel.store(true, Ordering::SeqCst);
            }
        }
        self.store
            .put_runtime(account, "desired", b"disconnected")?;
        self.store.append_event(
            account,
            "account.updated",
            &json!({"accountId":account,"connection":"disconnected"}),
        )?;
        Ok(())
    }
    pub fn logout_local(&self, account: &str) -> Result<()> {
        self.disconnect(account)?;
        self.store.put_runtime(account, "logged-out", b"1")?;
        self.store.append_event(account,"account.updated",&json!({"accountId":account,"authentication":"logged_out","serverRevocation":"not_requested"}))?;
        Ok(())
    }
    pub fn cancel_operation(&self, account: &str, id: &str) -> Result<Operation> {
        let mut op = self.store.operation(account, id)?;
        if let Some(cancel) = self
            .cancels
            .lock()
            .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?
            .get(id)
        {
            cancel.store(true, Ordering::SeqCst);
        }
        if !matches!(
            op.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "expired" | "rejected" | "needs_login"
        ) {
            op.state = "cancelled".into();
            op.updated_at = now();
            self.store.save_operation(&op)?;
        }
        Ok(op)
    }
    pub fn qr(&self, account: &str, id: &str) -> Result<Vec<u8>> {
        let op = self.store.operation(account, id)?;
        if !matches!(op.state.as_str(), "awaiting_scan" | "awaiting_approval") {
            bail!("login_challenge_unavailable");
        }
        let key = format!("qr:{id}");
        let encrypted = self
            .store
            .runtime(account, &key)?
            .context("login_challenge_unavailable")?;
        Ok(self.vault.open_record(account, &key, &encrypted)?.to_vec())
    }
    pub async fn login(
        self: &Arc<Self>,
        account: &str,
        request: LoginRequest,
    ) -> Result<Operation> {
        self.store.account(account)?;
        if !(10..=900).contains(&request.timeout_seconds)
            || request.computer_name.is_empty()
            || request.computer_name.len() > 100
        {
            bail!("invalid login options");
        }
        let gate = self.gate(account)?;
        if gate.try_lock().is_err() {
            bail!("operation_in_progress");
        }
        let active_key = format!("login-active:{account}");
        let mut cancels = self
            .cancels
            .lock()
            .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?;
        if cancels.contains_key(&active_key) {
            bail!("operation_in_progress");
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let timestamp = now();
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.into(),
            kind: "login".into(),
            state: "starting".into(),
            result: json!({}),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        cancels.insert(active_key.clone(), cancel.clone());
        cancels.insert(op.id.clone(), cancel.clone());
        drop(cancels);
        self.store.save_operation(&op)?;
        let core = self.clone();
        let worker_op = op.clone();
        tokio::spawn(async move {
            let _guard = gate.lock().await;
            let outcome = core.login_worker(worker_op.clone(), request, cancel).await;
            if let Err(error) = outcome {
                let code = if auth_rejected(&error) {
                    "auth_required"
                } else {
                    "login_failed"
                };
                let _ = core.progress(&worker_op, "failed", json!({"code":code,"retryable":false}));
            }
            if let Ok(mut map) = core.cancels.lock() {
                map.remove(&active_key);
                map.remove(&worker_op.id);
            }
        });
        Ok(op)
    }
    fn progress(&self, op: &Operation, state: &str, result: Value) -> Result<()> {
        let mut next = op.clone();
        next.state = state.into();
        next.result = result;
        next.updated_at = now();
        self.store.save_operation(&next)?;
        self.store.append_event(
            &op.account_id,
            "operation.updated",
            &json!({"id":op.id,"state":state,"kind":op.kind}),
        )?;
        Ok(())
    }
    async fn login_worker(
        self: &Arc<Self>,
        op: Operation,
        request: LoginRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<()> {
        let start = Instant::now();
        let account = op.account_id.clone();
        let previous = self.state_unfenced(&account).ok();
        let fenced = self.store.runtime(&account, "logged-out")?.is_some();
        if let Some(saved) = previous.as_ref().filter(|_| !request.force_qr && !fenced) {
            self.progress(&op, "resuming", json!({"method":"active"}))?;
            let mut state = saved.clone();
            let cursor = self.cursor(&account)?;
            let validated = blocking(move || {
                let client = client()?;
                match client.heartbeat(&mut state, &cursor) {
                    Ok(_) => Ok(state),
                    Err(error) if auth_rejected(&error) => {
                        client.renew_session(&state, "agent-wechat")
                    }
                    Err(e) => Err(e),
                }
            })
            .await;
            match validated {
                Ok(state) => {
                    if cancel.load(Ordering::SeqCst) {
                        self.progress(&op, "cancelled", json!({}))?;
                        return Ok(());
                    }
                    self.save_state(&account, &state)?;
                    self.store.put_runtime(
                        &account,
                        "validated-generation",
                        self.ownership_generation.as_bytes(),
                    )?;
                    self.store.clear_runtime(&account, "logged-out")?;
                    self.store.put_runtime(&account, "desired", b"connected")?;
                    self.progress(&op, "succeeded", json!({"method":"active_or_renewed"}))?;
                    self.start_workers()?;
                    return Ok(());
                }
                Err(error) if !auth_rejected(&error) => return Err(error),
                Err(_) => {}
            }
        }
        let mut attempt = if let Some(state) =
            previous.filter(|s| !request.force_qr && !s.auto_auth_key.is_empty())
        {
            self.progress(&op, "resuming", json!({"method":"saved_device"}))?;
            let name = request.computer_name.clone();
            blocking(move || client()?.start_saved_device_login(&state, &name)).await?
        } else {
            if !request.allow_qr {
                self.progress(
                    &op,
                    "needs_login",
                    json!({"reason":"no_usable_saved_trust"}),
                )?;
                return Ok(());
            }
            let device = if let Some(raw) = self.store.runtime(&account, "device")? {
                serde_json::from_slice::<DeviceProfile>(
                    &self.vault.open_record(&account, "device", &raw)?,
                )?
            } else {
                let device = DeviceProfile::generate("UnifiedPCLinux x86_64".into())?;
                self.store.put_runtime(
                    &account,
                    "device",
                    &self
                        .vault
                        .seal(&account, "device", &serde_json::to_vec(&device)?)?,
                )?;
                device
            };
            let name = request.computer_name.clone();
            blocking(move || client()?.start_qr_login(device, &name)).await?
        };
        let mut generation = 1;
        let mut scanned = false;
        loop {
            if cancel.load(Ordering::SeqCst) || self.stopping.load(Ordering::SeqCst) {
                self.progress(&op, "cancelled", json!({}))?;
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(request.timeout_seconds) {
                self.progress(&op, "expired", json!({"reason":"attempt_deadline"}))?;
                return Ok(());
            }
            if !attempt.qr_png.is_empty() {
                let png = white_png(&attempt.qr_png)?;
                let key = format!("qr:{}", op.id);
                self.store
                    .put_runtime(&account, &key, &self.vault.seal(&account, &key, &png)?)?;
            }
            self.progress(&op,if scanned||attempt.qr_png.is_empty(){"awaiting_approval"}else{"awaiting_scan"},json!({"qrGeneration":generation,"expiresAt":attempt.expires_at,"qrAvailable":!attempt.qr_png.is_empty()}))?;
            let result = blocking(move || {
                let status = client()?.poll_login(&mut attempt)?;
                Ok((attempt, status))
            })
            .await?;
            attempt = result.0;
            match result.1 {
                LoginStatus::Approved => {
                    self.progress(&op, "authenticating", json!({}))?;
                    let state = blocking(move || client()?.complete_login(&mut attempt)).await?;
                    if cancel.load(Ordering::SeqCst) {
                        self.progress(
                            &op,
                            "cancelled",
                            json!({"authenticationAcceptedButNotInstalled":true}),
                        )?;
                        return Ok(());
                    }
                    self.save_state(&account, &state)?;
                    self.store.put_runtime(
                        &account,
                        "validated-generation",
                        self.ownership_generation.as_bytes(),
                    )?;
                    self.store.clear_runtime(&account, "logged-out")?;
                    self.store.put_runtime(&account, "desired", b"connected")?;
                    self.progress(&op, "succeeded", json!({"method":"approved_login"}))?;
                    self.start_workers()?;
                    return Ok(());
                }
                LoginStatus::Scanned => scanned = true,
                LoginStatus::Expired
                    if !scanned
                        && !attempt.qr_png.is_empty()
                        && request.allow_qr
                        && generation < 4 =>
                {
                    generation += 1;
                    let device = attempt.device.clone();
                    let name = request.computer_name.clone();
                    attempt = blocking(move || client()?.start_qr_login(device, &name)).await?;
                }
                LoginStatus::Expired => {
                    self.progress(&op, "expired", json!({}))?;
                    return Ok(());
                }
                LoginStatus::Rejected | LoginStatus::Cancelled => {
                    self.progress(&op, "rejected", json!({}))?;
                    return Ok(());
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    pub async fn sync(self: &Arc<Self>, account: &str) -> Result<Value> {
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_owned();
        blocking(move||{
            let mut state=core.account_state(&account)?;let cursor=core.cursor(&account)?;let native=client()?;
            let batch=match native.sync(&mut state,&cursor) {Ok(batch)=>batch,Err(error) if auth_rejected(&error)=>{state=native.renew_session(&state,"agent-wechat")?;native.sync(&mut state,&cursor)?},Err(e)=>return Err(e)};
            core.save_state(&account,&state)?;core.store.put_runtime(&account,"validated-generation",core.ownership_generation.as_bytes())?;
            let origin=if core.store.runtime(&account,"history-caught-up")?.is_some(){"live"}else{"catchup"};
            let raw=serde_json::to_vec(&batch)?;let id=format!("{:x}",Sha256::digest(&raw));
            core.store.stage_sync(&account,&id,&core.vault.seal(&account,&format!("apply:{id}"),&raw)?,&core.vault.seal(&account,"received-cursor",&batch.cursor)?)?;
            core.store.put_runtime(&account,&format!("apply-origin:{id}"),origin.as_bytes())?;
            let projection=core.project_pending(&account)?;
            if !batch.continuation {core.store.put_runtime(&account,"history-caught-up",b"1")?;}
            core.store.append_event(&account,"sync.received",&json!({"batchId":id,"commandCount":batch.commands.len(),"profileProjection":"pending","continuation":batch.continuation}))?;
            Ok(json!({"batchId":id,"commandCount":batch.commands.len(),"continuation":batch.continuation,"projection":projection,"profileProjection":if core.store.pending_sync_count(&account)?==0{"complete"}else{"pending"}}))
        }).await
    }
    pub async fn lookup_contacts(
        self: &Arc<Self>,
        account: &str,
        ids: Vec<String>,
    ) -> Result<Value> {
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_string();
        blocking(move||{let mut state=core.account_state(&account)?;let contacts=client()?.lookup_contacts(&mut state,&ids)?;core.save_state(&account,&state)?;
            Ok(json!({"items":contacts.contacts.into_iter().map(|c|json!({"id":c.username,"displayName":c.remark.or(c.nickname).unwrap_or_else(||c.username.clone()),"alias":c.alias,"profileProjection":"pending"})).collect::<Vec<_>>(),"results":contacts.results}))
        }).await
    }
    pub async fn lookup_room(
        self: &Arc<Self>,
        account: &str,
        room: &str,
        version: u32,
    ) -> Result<Value> {
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_string();
        let room = room.to_string();
        blocking(move||{let mut state=core.account_state(&account)?;let roster=client()?.lookup_room(&mut state,&room,version)?;core.save_state(&account,&state)?;
            Ok(json!({"id":roster.room_id,"version":roster.version,"items":roster.members.into_iter().map(|m|json!({"id":m.username,"nickname":m.nickname,"roomDisplayName":m.room_display_name})).collect::<Vec<_>>(),"profileProjection":"pending"}))
        }).await
    }
    pub async fn send_text(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        destination: &str,
        text: &str,
    ) -> Result<SendIntent> {
        self.account_state(account)?;
        if self.account_status(account)?["authenticated"] != true {
            bail!("auth_required");
        }
        if text.is_empty()
            || text.len() > 64 * 1024
            || destination.is_empty()
            || destination.len() > 256
        {
            bail!("invalid send request");
        }
        let client_id =
            wechat_protocol::wire::client_message_id(format!("{account}:{key}").as_bytes()).max(1);
        let payload = SendPayload {
            thread_id: destination.into(),
            text: text.into(),
            created_at: wechat_protocol::wire::unix_time()?,
            client_id,
        };
        let hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(
                &json!({"threadId":destination,"text":text})
            )?)
        );
        let encrypted = self
            .vault
            .seal(account, "send-payload", &serde_json::to_vec(&payload)?)?;
        let (intent, created) = self.store.create_send(
            account,
            key,
            destination,
            &hash,
            &encrypted,
            &client_id.to_string(),
        )?;
        if created {
            let core = self.clone();
            let account = account.to_string();
            let id = intent.id.clone();
            tokio::spawn(async move {
                if core.perform_send(&account, &id).await.is_err() {
                    if let Ok(intent) = core.store.send(&account, &id) {
                        let state = if intent.state == "queued" {
                            "failed"
                        } else {
                            "needs_review"
                        };
                        let _ = core.store.update_send(
                            &account,
                            &id,
                            state,
                            None,
                            Some("worker_interrupted"),
                        );
                    }
                }
            });
        }
        Ok(intent)
    }
    async fn perform_send(self: &Arc<Self>, account: &str, id: &str) -> Result<()> {
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_string();
        let id = id.to_string();
        blocking(move || {
            let intent = core.store.send(&account, &id)?;
            if core.stopping.load(Ordering::SeqCst)
                || core.store.runtime(&account, "desired")?.as_deref() != Some(b"connected")
            {
                return Ok(());
            }
            if intent.state != "queued" {
                return Ok(());
            }
            let encrypted = core.store.send_payload(&account, &id)?;
            let plain = core
                .vault
                .open_record(&account, "send-payload", &encrypted)?;
            let payload: SendPayload = serde_json::from_slice(&plain)?;
            let mut state = core.account_state(&account)?;
            let native = client()?;
            core.store
                .update_send(&account, &id, "submitting", None, None)?;
            let result = native.send_text(
                &mut state,
                &payload.thread_id,
                &payload.text,
                payload.created_at,
                payload.client_id,
            );
            // Preserve refreshed cookies independently of a local projection failure.
            core.save_state(&account, &state)?;
            core.store.put_runtime(
                &account,
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )?;
            let updated = match result {
                Ok(result)
                    if result.accepted
                        && result.returned_destination_matches
                        && result.returned_client_id_matches =>
                {
                    core.store.update_send(
                        &account,
                        &id,
                        "accepted",
                        result.server_message_id.as_deref(),
                        None,
                    )?
                }
                Ok(result) if !result.accepted => core.store.update_send(
                    &account,
                    &id,
                    "failed",
                    None,
                    Some("service_rejected"),
                )?,
                _ => core.store.update_send(
                    &account,
                    &id,
                    "needs_review",
                    None,
                    Some("submission_outcome_unknown"),
                )?,
            };
            core.store
                .append_event(&account, "send.updated", &serde_json::to_value(updated)?)?;
            Ok(())
        })
        .await
    }
    pub fn recover_projection(&self, account: &str) -> Result<Value> {
        if self.store.pending_sync(account)?.is_empty() {
            self.store.retry_blocked(account)?;
        }
        self.project_pending(account)
    }
    fn project_pending(&self, account: &str) -> Result<Value> {
        let mut projected = 0;
        let mut unsupported = 0;
        let state = self.account_state(account)?;
        for (batch_id, encrypted) in self.store.pending_sync(account)? {
            let plain =
                self.vault
                    .open_record(account, &format!("apply:{batch_id}"), &encrypted)?;
            let batch: wechat_protocol::wire::SyncBatch = serde_json::from_slice(&plain)?;
            let origin = self
                .store
                .runtime(account, &format!("apply-origin:{batch_id}"))?
                .and_then(|v| String::from_utf8(v).ok())
                .unwrap_or_else(|| "catchup".into());
            let mut complete = true;
            for (index, command) in batch.commands.iter().enumerate() {
                let receipt = format!("projected:{batch_id}:{index}");
                if self.store.runtime(account, &receipt)?.is_some() {
                    continue;
                }
                let outcome = (|| -> Result<()> {
                    if command.id != 5 {
                        bail!("unsupported sync command");
                    }
                    let msg = wechat_protocol::wire::decode_add_message(
                        &command.payload,
                        &state.own_username,
                    )?;
                    if msg.outgoing || msg.peer.ends_with("@chatroom") || msg.message_type != 1 {
                        bail!("unsupported projection family");
                    }
                    let sequence = msg
                        .server_sequence
                        .context("unsupported message without verified sequence")?;
                    let input = wechat_profile::IncomingText {
                        peer: msg.peer.clone(),
                        own_username: state.own_username.clone(),
                        server_message_id: msg
                            .server_message_id
                            .context("unsupported message without server identity")?,
                        server_sequence: sequence as i64,
                        create_time: msg.created_at as i64,
                        content: msg.content_text.context("unsupported undecoded text")?,
                        source_xml: match msg.msgsource {
                            Some(v) => String::from_utf8(v)?,
                            None => String::new(),
                        },
                        unread: true,
                    };
                    let options = self.write_options(account, &receipt)?;
                    let (location, keys) = self.profile_location_keys(account)?;
                    let applied =
                        wechat_profile::apply_incoming_text(&location, &keys, &input, &options)?;
                    let id = crate::message_id(&applied.source_id)?;
                    self.store.append_event_once(account,&format!("message:{id}"),"message.committed",&json!({"messageId":id,"threadId":input.peer,"origin":origin,"direction":"incoming"}))?;
                    self.store.put_runtime(account, &receipt, b"1")?;
                    Ok(())
                })();
                match outcome {
                    Ok(()) => projected += 1,
                    Err(_) => {
                        complete = false;
                        unsupported += 1;
                    }
                }
            }
            if complete {
                self.store.finish_sync(account, &batch_id)?;
            } else {
                self.store.block_sync(account, &batch_id)?;
                self.store.append_event_once(account,&format!("blocked:{batch_id}"),"projection.blocked",&json!({"batchId":batch_id,"reason":"unsupported_or_unavailable_profile","retryableAfterRepair":true}))?;
            }
        }
        Ok(
            json!({"projectedCommands":projected,"pendingCommands":unsupported,"pendingBatches":self.store.pending_sync_count(account)?}),
        )
    }
    pub fn enable_profile_writes(
        &self,
        account: &str,
        own_username: &str,
        mmkv_store: &std::path::Path,
        format_key: &[u8],
    ) -> Result<()> {
        let state = self.account_state(account)?;
        if own_username != state.own_username {
            bail!("account_identity_mismatch");
        }
        let root = self.store.account(account)?.profile_path;
        let path = std::fs::canonicalize(mmkv_store)?;
        let relative = path
            .strip_prefix(&root)
            .context("identity source must be inside the selected official profile")?
            .to_str()
            .context("identity source path must be UTF-8")?;
        let binding = IdentityBinding {
            relative: relative.into(),
            format_key: format_key.to_vec(),
        };
        self.verify_profile_identity(account, &binding)?;
        self.store.put_runtime(
            account,
            "profile-write-binding",
            &self.vault.seal(
                account,
                "profile-write-binding",
                &serde_json::to_vec(&binding)?,
            )?,
        )?;
        self.store.retry_blocked(account)
    }
    fn verify_profile_identity(&self, account: &str, binding: &IdentityBinding) -> Result<()> {
        use std::{io::Read, os::unix::fs::OpenOptionsExt};
        let root = self.store.account(account)?.profile_path;
        let path = root.join(&binding.relative);
        if std::path::Path::new(&binding.relative).is_absolute()
            || std::fs::canonicalize(&path)? != path
        {
            bail!("invalid profile identity path");
        }
        let read = |path: &std::path::Path, max: u64| -> Result<Vec<u8>> {
            if std::fs::canonicalize(path)? != path || !path.starts_with(&root) {
                bail!("invalid profile identity path");
            }
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            let m = f.metadata()?;
            if !m.is_file() || m.len() > max {
                bail!("invalid profile identity file");
            }
            let mut bytes = vec![];
            f.take(max + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max {
                bail!("invalid profile identity file");
            }
            Ok(bytes)
        };
        let data = read(&path, 4 * 1024 * 1024 + 4)?;
        let meta = read(
            &path.with_file_name(format!(
                "{}.crc",
                path.file_name()
                    .context("invalid identity filename")?
                    .to_string_lossy()
            )),
            4096,
        )?;
        let identity = wechat_profile::read_mmkv_identity(&data, &meta, &binding.format_key)?;
        let state = self.account_state(account)?;
        if identity.username != state.own_username || identity.uin != state.uin {
            bail!("account_identity_mismatch");
        }
        let (_, keys) = self.profile_location_keys(account)?;
        let suffix = keys
            .account_binding
            .strip_prefix(&format!("{}_", identity.username));
        if keys.account_binding != identity.username
            && !suffix.is_some_and(|s| {
                !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
            })
        {
            bail!("account_identity_mismatch");
        }
        self.profile_reader(account)?;
        Ok(())
    }
    fn write_options(&self, account: &str, intent: &str) -> Result<wechat_profile::WriteOptions> {
        let encrypted = self
            .store
            .runtime(account, "profile-write-binding")?
            .context("profile_writes_not_enabled")?;
        let binding: IdentityBinding = serde_json::from_slice(&self.vault.open_record(
            account,
            "profile-write-binding",
            &encrypted,
        )?)?;
        self.verify_profile_identity(account, &binding)?;
        Ok(wechat_profile::WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: false,
            durable_intent_id: intent.into(),
        })
    }
    pub async fn mark_read(
        self: &Arc<Self>,
        account: &str,
        thread: &str,
        ids: Vec<String>,
        expected: &str,
    ) -> Result<Value> {
        if ids.is_empty() || ids.len() > 200 || expected.len() > 128 {
            bail!("invalid read set");
        }
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_string();
        let thread = thread.to_string();
        let expected = expected.to_string();
        blocking(move||{
            let reader=core.profile_reader(&account)?;let own=core.account_state(&account)?.own_username.clone();
            let counter=core.store.read_version(&account,&thread)?;
            let intent=json!({"messageIds":ids,"expectedVersion":expected});
            if core.read_state_version(&account,&thread,reader.generation())?!=expected&&core.store.runtime(&account,&format!("read-pending:{thread}"))?.as_deref()!=Some(serde_json::to_vec(&intent)?.as_slice()){bail!("read_state_conflict");}
            let mut sources=std::collections::BTreeSet::new();
            for id in &ids {let source=crate::source_message_id(id)?;let msg=reader.message(&source,Some(&own))?;
                if msg.peer.as_deref()!=Some(thread.as_str())||msg.outgoing!=Some(false) {bail!("invalid read message identity");}sources.insert(source);
            }drop(reader);
            let durable=format!("read:{:x}",Sha256::digest(serde_json::to_vec(&intent)?));
            let options=core.write_options(&account,&durable)?;
            let next=core.store.begin_read(&account,&thread,&counter,&intent)?;
            let (location,keys)=core.profile_location_keys(&account)?;
            let projection=wechat_profile::mark_read_messages(&location,&keys,&thread,&sources.into_iter().collect::<Vec<_>>(),&options)?;
            core.store.finish_read(&account,&thread,&next)?;
            let reader=core.profile_reader(&account)?;let next=core.read_state_version(&account,&thread,reader.generation())?;
            core.store.append_event_once(&account,&durable,"thread.read-state",&json!({"threadId":thread,"version":next,"messageIds":ids}))?;
            Ok(json!({"threadId":thread,"version":next,"projection":projection,"phoneReceiptSent":false}))
        }).await
    }
    /// Starts independent serialized account workers. A reconnect does not open
    /// a QR challenge; operator login remains an explicit operation.
    pub fn start_workers(self: &Arc<Self>) -> Result<()> {
        for account in self.store.accounts()? {
            if !self
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("worker manager unavailable"))?
                .insert(account.id.clone())
            {
                continue;
            }
            self.store.clear_runtime(&account.id, "history-caught-up")?;
            let core = self.clone();
            tokio::spawn(async move {
                let mut delay = 1;
                loop {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    if core.stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let desired = core.store.runtime(&account.id, "desired").ok().flatten();
                    if desired.as_deref() != Some(b"connected") {
                        delay = 2;
                        continue;
                    }
                    match core.tick(&account.id).await {
                        Ok(interval) => delay = interval,
                        Err(error) => {
                            let code = if auth_rejected(&error)
                                || error.to_string().contains("auth_required")
                            {
                                "auth_required"
                            } else {
                                "connection_unavailable"
                            };
                            let _ = core.store.append_event_once(
                                &account.id,
                                &format!("worker:{code}:{}", wechat_store::unix_seconds() / 60),
                                "account.connection",
                                &json!({"state":code}),
                            );
                            if code == "auth_required" {
                                let _ =
                                    core.store
                                        .put_runtime(&account.id, "desired", b"disconnected");
                            }
                            delay = (delay * 2).clamp(5, 60);
                        }
                    }
                }
            });
        }
        Ok(())
    }
    async fn tick(self: &Arc<Self>, account: &str) -> Result<u64> {
        let gate = self.gate(account)?;
        let heartbeat = {
            let _guard = gate.lock().await;
            let core = self.clone();
            let account = account.to_string();
            blocking(move || {
                let mut state = core.account_state(&account)?;
                let cursor = core.cursor(&account)?;
                let native = client()?;
                let beat = match native.heartbeat(&mut state, &cursor) {
                    Ok(beat) => beat,
                    Err(error) if auth_rejected(&error) => {
                        state = native.renew_session(&state, "agent-wechat")?;
                        native.heartbeat(&mut state, &cursor)?
                    }
                    Err(error) => return Err(error),
                };
                core.save_state(&account, &state)?;
                core.store.put_runtime(
                    &account,
                    "validated-generation",
                    core.ownership_generation.as_bytes(),
                )?;
                Ok(beat)
            })
            .await?
        };
        if self.stopping.load(Ordering::SeqCst)
            || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
        {
            return Ok(5);
        }
        if heartbeat.selector & 7 != 0
            || self.store.runtime(account, "history-caught-up")?.is_none()
        {
            for _ in 0..8 {
                if self.stopping.load(Ordering::SeqCst)
                    || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
                {
                    break;
                }
                let result = self.sync(account).await?;
                if result["continuation"] != true {
                    break;
                }
            }
        }
        for id in self.store.queued_sends(account)? {
            if self.stopping.load(Ordering::SeqCst)
                || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            {
                break;
            }
            self.perform_send(account, &id).await?;
        }
        // Heartbeat polling is the verified fallback. Unsolicited long-link
        // push association is not claimed as supported by this actor.
        Ok((heartbeat.next_interval as u64).clamp(5, 30))
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Ok(cancels) = self.cancels.lock() {
            for cancel in cancels.values() {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}
fn white_png(bytes: &[u8]) -> Result<Vec<u8>> {
    use image::{ImageFormat, ImageReader, Rgba, RgbaImage};
    let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.to_rgba8();
    let mut white = RgbaImage::from_pixel(
        image.width() + 16,
        image.height() + 16,
        Rgba([255, 255, 255, 255]),
    );
    image::imageops::overlay(&mut white, &image, 8, 8);
    let mut output = std::io::Cursor::new(Vec::new());
    white.write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: [b"A".as_slice(), &[1u8; 15]].concat(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 1,
            own_username: "synthetic_self".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    #[test]
    fn protected_credentials_survive_restart_without_plaintext_or_token_coupling() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("data");
        let store = Arc::new(Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.restore_credentials("one", &serde_json::to_vec(&state()).unwrap())
            .unwrap();
        let raw = store.runtime("one", "account-state").unwrap().unwrap();
        assert!(!raw.windows(14).any(|v| v == b"synthetic_self"));
        assert_eq!(core.account_state("one").unwrap().uin, 1);
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&root).unwrap());
        let core = Core::new(store.clone(), None).unwrap();
        assert_eq!(core.account_state("one").unwrap().uin, 1);
        let key = root.join("storage-key");
        let saved = std::fs::read(&key).unwrap();
        std::fs::remove_file(&key).unwrap();
        drop(core);
        assert!(Core::new(store, None).is_err());
        assert!(!key.exists());
        assert_eq!(saved.len(), 32);
    }
    #[test]
    fn logout_fences_active_credentials_and_identity_cannot_be_replaced() {
        let t = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        let mut state = state();
        core.restore_credentials("one", &serde_json::to_vec(&state).unwrap())
            .unwrap();
        core.logout_local("one").unwrap();
        assert!(core.account_state("one").is_err());
        assert!(core.state_unfenced("one").is_ok());
        state.uin = 2;
        assert!(core
            .restore_credentials("one", &serde_json::to_vec(&state).unwrap())
            .is_err());
    }
    #[tokio::test]
    async fn durable_batch_projects_before_event_and_exact_read_cas_preserves_other_state() {
        use md5::Md5;
        use wechat_profile::{ProfileLocation, ReadOptions};
        use wechat_protocol::{
            protobuf as p,
            wire::{SyncBatch, SyncCommand},
        };
        let t = tempfile::tempdir().unwrap();
        let profile = t.path().join("synthetic_self_1234");
        let storage = profile.join("db_storage");
        let create = |relative: &str, sql: &str| {
            let path = storage.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = rusqlite::Connection::open(path).unwrap();
            db.execute_batch(
                "PRAGMA key='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';PRAGMA cipher_compatibility=4;",
            )
            .unwrap();
            db.execute_batch(sql).unwrap();
        };
        create("contact/contact.db","CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,nick_name TEXT,remark TEXT,alias TEXT,local_type INTEGER);INSERT INTO contact VALUES(1,'peer','Peer','','',1),(2,'synthetic_self','Self','','',1);");
        create("session/session.db","CREATE TABLE SessionTable(username TEXT PRIMARY KEY,type INTEGER,unread_count INTEGER,unread_first_msg_srv_id INTEGER,unread_first_pat_msg_local_id INTEGER,unread_first_pat_msg_sort_seq INTEGER,is_hidden INTEGER,summary TEXT,draft TEXT,status INTEGER,last_timestamp INTEGER,sort_timestamp INTEGER,last_clear_unread_timestamp INTEGER,last_msg_locald_id INTEGER,last_msg_type INTEGER,last_msg_sub_type INTEGER,last_msg_sender TEXT,last_sender_display_name TEXT,last_msg_ext_type INTEGER);CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY);INSERT INTO Name2Id VALUES('peer');CREATE TABLE SessionUnreadListTable_1(username_id INTEGER,server_id INTEGER,local_id INTEGER,create_time INTEGER, CONSTRAINT _NameId_ServerId PRIMARY KEY(username_id,server_id));INSERT INTO SessionTable VALUES('peer',1,0,0,0,0,0,'Before','Keep draft',7,100,100,99,0,1,0,'','',0);");
        let table = format!("Msg_{:x}", Md5::digest(b"peer"));
        create("message/message_0.db",&format!("CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY,is_session INTEGER);INSERT INTO Name2Id VALUES('synthetic_self',1),('peer',1);CREATE TABLE {table}(local_id INTEGER PRIMARY KEY AUTOINCREMENT,server_id INTEGER,local_type INTEGER,sort_seq INTEGER,real_sender_id INTEGER,create_time INTEGER,status INTEGER,upload_status INTEGER,download_status INTEGER,server_seq INTEGER,origin_source INTEGER,source TEXT,message_content TEXT,compress_content TEXT,packed_info_data BLOB,WCDB_CT_message_content INTEGER DEFAULT NULL,WCDB_CT_source INTEGER DEFAULT NULL);CREATE INDEX {table}_SERVERID ON {table}(server_id);CREATE INDEX {table}_SORTSEQ ON {table}(sort_seq);CREATE INDEX {table}_TYPE_SEQ ON {table}(local_type,sort_seq);CREATE INDEX {table}_SENDERID ON {table}(real_sender_id);"));
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        store.attach("one", &profile, true).unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.restore_credentials("one", &serde_json::to_vec(&state()).unwrap())
            .unwrap();
        let keys = wechat_profile::keys_from_password(
            ProfileLocation::new(&profile).unwrap(),
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ReadOptions {
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        let keys_path = store.root.join("test-keys.json");
        keys.write_new(&keys_path).unwrap();
        store.set_keys_file("one", &keys_path).unwrap();
        let payload = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::bytes(1, b"peer")),
            p::bytes(3, &p::bytes(1, b"synthetic_self")),
            p::uint(4, 1),
            p::bytes(5, &p::bytes(1, b"Synthetic committed text")),
            p::uint(6, 0),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, 101),
            p::bytes(10, b"<msgsource/>"),
            p::uint(12, u64::MAX - 5),
            p::uint(13, 12),
        ])
        .unwrap();
        let batch = SyncBatch {
            code: 0,
            flags: 0,
            cursor: p::uint(1, 0),
            commands: vec![SyncCommand { id: 5, payload }],
            continuation: false,
        };
        let raw = serde_json::to_vec(&batch).unwrap();
        store
            .stage_sync(
                "one",
                "synthetic-batch",
                &core
                    .vault
                    .seal("one", "apply:synthetic-batch", &raw)
                    .unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &batch.cursor)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["pendingCommands"],
            1
        );
        assert!(!store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .any(|e| e.kind == "message.committed"));
        let mut plain = vec![0];
        let frame = |bytes: &[u8]| {
            assert!(bytes.len() < 128);
            [vec![bytes.len() as u8], bytes.to_vec()].concat()
        };
        for (key, value) in [
            ("mmkv_key_user_name", frame(b"synthetic_self")),
            ("mmkv_key_latest_login_uin", vec![1]),
            ("key_wechat_device_id", frame(b"synthetic-device-16")),
        ] {
            plain.extend(frame(key.as_bytes()));
            plain.extend(frame(&value));
        }
        let encrypted = openssl::symm::encrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            &[9; 16],
            Some(&[3; 16]),
            &plain,
        )
        .unwrap();
        let mut metadata = vec![0; 112];
        metadata[..4].copy_from_slice(&crc32fast::hash(&encrypted).to_le_bytes());
        metadata[4..8].copy_from_slice(&4u32.to_le_bytes());
        metadata[12..28].copy_from_slice(&[3; 16]);
        metadata[28..32].copy_from_slice(&(encrypted.len() as u32).to_le_bytes());
        let mmkv = profile.join("identity");
        std::fs::write(&mmkv, [vec![0; 4], encrypted].concat()).unwrap();
        std::fs::write(profile.join("identity.crc"), metadata).unwrap();
        assert!(core
            .enable_profile_writes("one", "wrong", &mmkv, &[9; 16])
            .is_err());
        assert!(core
            .enable_profile_writes("one", "synthetic_self", &mmkv, &[8; 16])
            .is_err());
        core.enable_profile_writes("one", "synthetic_self", &mmkv, &[9; 16])
            .unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            1
        );
        assert!(store.pending_sync("one").unwrap().is_empty());
        let messages = core.messages("one", "peer", None, None).unwrap();
        let id = messages["items"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(
            core.message("one", &id).unwrap()["serverId"],
            (u64::MAX - 5).to_string()
        );
        assert_eq!(
            store
                .events("one", 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "message.committed")
                .count(),
            1
        );
        let threads = core.threads("one", &Default::default()).unwrap();
        let version = threads["items"][0]["readStateVersion"].as_str().unwrap();
        assert!(core
            .mark_read("one", "peer", vec![id.clone()], "stale")
            .await
            .is_err());
        let read = core
            .mark_read("one", "peer", vec![id.clone()], version)
            .await
            .unwrap();
        assert_eq!(read["projection"]["removedUnreadRows"], 1);
        assert_eq!(
            core.threads("one", &Default::default()).unwrap()["items"][0]["unreadCount"],
            0
        );
        assert!(core
            .mark_read("one", "peer", vec![id], version)
            .await
            .is_err());
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
    }
    #[test]
    fn qr_is_opaque_white_with_a_quiet_border() {
        use image::{ImageFormat, Rgba, RgbaImage};
        let source = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 255]));
        let mut bytes = std::io::Cursor::new(vec![]);
        source.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let decoded = image::load_from_memory(&white_png(bytes.get_ref()).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (18, 18));
        assert_eq!(decoded.get_pixel(0, 0), &Rgba([255, 255, 255, 255]));
        assert_eq!(decoded.get_pixel(8, 8), &Rgba([0, 0, 0, 255]));
    }
}
