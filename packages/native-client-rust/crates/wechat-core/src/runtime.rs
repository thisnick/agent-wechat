#[path = "checkpoint.rs"]
mod checkpoint;
#[path = "logout.rs"]
mod logout;
#[path = "metadata.rs"]
mod metadata;
#[path = "sync_projection.rs"]
mod sync_projection;
pub use metadata::MetadataAction;
#[path = "actions.rs"]
mod actions;
#[path = "bootstrap.rs"]
mod bootstrap;
#[path = "identity.rs"]
mod identity;
#[path = "official_profile.rs"]
mod official_profile;
pub use official_profile::{OfficialProfileRequest, OfficialProfileSelection};
#[path = "profile_network.rs"]
mod profile_network;
#[path = "push.rs"]
mod push;
#[path = "read_control.rs"]
mod read_control;
#[path = "read_sync.rs"]
mod read_sync;
#[path = "saved_login.rs"]
mod saved_login;
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
    pub(crate) vault: Arc<Vault>,
    pub(crate) cursor_vault: Arc<Vault>,
    password_proofs: Mutex<BTreeMap<String, String>>,
    profile_format_key: Mutex<Option<zeroize::Zeroizing<Vec<u8>>>>,
    bootstrap_leases: Mutex<std::collections::BTreeSet<String>>,
    startup_profiles_recovered: AtomicBool,
    gates: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
    cancels: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    workers: Mutex<std::collections::BTreeSet<String>>,
    push_health: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    official_profiles: AsyncMutex<()>,
    pub stopping: AtomicBool,
}
#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginRequest {
    #[serde(default)]
    pub strategy: LoginStrategy,
    #[serde(default)]
    pub qr_refresh: QrRefresh,
    #[serde(default = "yes")]
    pub allow_qr: bool,
    #[serde(default)]
    pub force_qr: bool,
    #[serde(default = "deadline")]
    #[ts(type = "number")]
    pub timeout_seconds: u64,
    #[serde(default = "computer")]
    pub computer_name: String,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum LoginStrategy {
    #[default]
    Auto,
    SavedDevice,
    Qr,
}
#[derive(Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QrRefresh {
    #[serde(default)]
    pub mode: QrRefreshMode,
    #[serde(default = "refreshes")]
    pub max_refreshes: u8,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum QrRefreshMode {
    #[default]
    Auto,
    Manual,
}
fn refreshes() -> u8 {
    3
}
impl Default for QrRefresh {
    fn default() -> Self {
        Self {
            mode: QrRefreshMode::Auto,
            max_refreshes: refreshes(),
        }
    }
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
            strategy: LoginStrategy::Auto,
            qr_refresh: QrRefresh::default(),
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
    error.chain().any(|cause| {
        let message = cause.to_string();
        [
            "Native envelope returned ",
            "Heartbeat returned ",
            "Sync service returned ",
            "Contact service returned ",
            "Saved-device login returned ",
            "Account authentication returned ",
            "Automatic authentication returned ",
        ]
        .iter()
        .any(|prefix| {
            message
                .strip_prefix(prefix)
                .and_then(|v| v.parse::<i32>().ok())
                == Some(-13)
        })
    })
}
fn saved_trust_rejected(error: &anyhow::Error) -> bool {
    auth_rejected(error)
        || error.chain().any(|cause| {
            cause
                .to_string()
                .strip_prefix("Saved-device login returned ")
                .and_then(|v| v.parse::<i32>().ok())
                == Some(-2017)
        })
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
        let cursor_vault = Vault::open(
            &store.root.join("cursor-key"),
            !store.pagination_views_exist()?,
        )?;
        let bootstrap_leases = store
            .accounts()?
            .into_iter()
            .filter(|a| {
                a.profile_path.join("xwechat_files").is_dir()
                    || a.profile_path.join("db_storage").is_dir()
            })
            .map(|a| a.id)
            .collect();
        let core = Arc::new(Self {
            store,
            leases: Mutex::new(leases),
            vault: Arc::new(vault),
            cursor_vault: Arc::new(cursor_vault),
            password_proofs: Mutex::new(BTreeMap::new()),
            profile_format_key: Mutex::new(Some(crate::format::global_mmkv_format_key())),
            bootstrap_leases: Mutex::new(bootstrap_leases),
            startup_profiles_recovered: AtomicBool::new(false),
            ownership_generation: uuid::Uuid::new_v4().to_string(),
            gates: Mutex::new(BTreeMap::new()),
            cancels: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(std::collections::BTreeSet::new()),
            push_health: Mutex::new(BTreeMap::new()),
            official_profiles: AsyncMutex::new(()),
            stopping: AtomicBool::new(false),
        });
        for account in core.store.accounts()? {
            if core.store.runtime(&account.id, "account-state")?.is_some() {
                core.state_unfenced(&account.id)?;
            }
            core.store.recover_uncertain_sends(&account.id)?;
            core.store.recover_media_jobs(&account.id)?;
            core.recover_logout(&account.id)?;
            core.store.recover_interrupted_operations(&account.id)?;
            let _ = core.recover_metadata(&account.id)?;
            let _ = core.recover_saved_login(&account.id);
        }
        Ok(core)
    }
    pub fn create_account(&self, id: &str) -> Result<wechat_store::Account> {
        self.create_account_with_alias(id, None)
    }
    pub fn create_account_with_alias(
        &self,
        id: &str,
        alias: Option<&str>,
    ) -> Result<wechat_store::Account> {
        let account = self.store.create_account_with_alias(id, alias)?;
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
        let state = self.state_unfenced(account)?;
        self.check_state_profile(account, &state)?;
        Ok(state)
    }
    pub(crate) fn save_state(&self, account: &str, state: &AccountState) -> Result<()> {
        self.persist_state(account, state, true)
    }
    fn persist_state(&self, account: &str, state: &AccountState, accepted: bool) -> Result<()> {
        state.validate()?;
        self.check_state_profile(account, state)?;
        if let Ok(prior) = self.state_unfenced(account) {
            if prior.uin != state.uin || prior.own_username != state.own_username {
                bail!("account_identity_mismatch");
            }
        }
        let sealed = self
            .vault
            .seal(account, "account-state", &serde_json::to_vec(state)?)?;
        if accepted {
            self.store.save_accepted_authorization(account, &sealed)?;
            let _ = self.project_saved_login(account, state);
            Ok(())
        } else {
            self.store.save_active_authorization(account, &sealed)
        }
    }
    #[cfg(test)]
    fn install_login_state(
        &self,
        account: &str,
        state: &AccountState,
        epoch: Option<&[u8]>,
    ) -> Result<()> {
        self.install_login_operation(account, state, epoch, None)
    }
    fn install_login_operation(
        &self,
        account: &str,
        state: &AccountState,
        epoch: Option<&[u8]>,
        operation: Option<&str>,
    ) -> Result<()> {
        state.validate()?;
        self.prepare_official_profile_keys(account, state)?;
        self.check_state_profile(account, state)?;
        if let Ok(prior) = self.state_unfenced(account) {
            if prior.uin != state.uin || prior.own_username != state.own_username {
                bail!("account_identity_mismatch");
            }
        }
        self.store.commit_login_authorization_for_operation(
            account,
            &self
                .vault
                .seal(account, "account-state", &serde_json::to_vec(state)?)?,
            epoch,
            &self.ownership_generation,
            operation,
        )?;
        let _ = self.project_saved_login(account, state);
        Ok(())
    }
    pub fn restore_credentials(&self, account: &str, input: &[u8]) -> Result<()> {
        self.store.account(account)?;
        let state = match serde_json::from_slice::<AccountState>(input) {
            Ok(state) => state,
            Err(_) => AccountState::from_baseline_json(input)?,
        };
        self.persist_state(account, &state, false)?;
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
        wechat_protocol::wire::decode_cursor(cursor)?;
        self.store.restore_received_cursor(
            account,
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
        let auto_connect = self.store.runtime(account, "auto-connect")?.as_deref() != Some(b"0");
        let incoming = if auth && desired == b"connected" {
            if self.push_connected(account) {
                "native_push"
            } else {
                "heartbeat_polling"
            }
        } else {
            "disconnected"
        };
        Ok(
            json!({"id":record.id,"storageMode":record.storage_mode,"schemaCompatibility":record.schema_compatibility,"authenticated":auth,"connection":if auth&&desired==b"connected"{"online"}else if saved&&auto_connect&&desired==b"connected"{"reconnecting"}else{"disconnected"},"incomingTransport":incoming,"pendingProjectionBatches":self.store.pending_sync_count(account)?,"pendingCursorProjection":self.store.pending_cursor_projection(account)?,"pendingReadProjections":self.store.pending_reads(account)?,"pendingSavedLoginProjection":self.store.runtime(account,"saved-login-projection-needed")?.is_some()||self.store.runtime(account,"saved-login-publication")?.is_some(),"profileRecoveryPending":self.store.runtime(account,"profile-recovery-pending")?.is_some(),"nativeRuntime":true,"alias":self.store.account_alias(account)?,"autoConnect":auto_connect}),
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
        let stamp = now();
        let mut op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.into(),
            kind: "logout".into(),
            state: "queued".into(),
            result: json!({"remoteOutcome":"not_attempted","localCommitted":true}),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        self.store
            .begin_logout(&op, uuid::Uuid::new_v4().as_bytes())?;
        op.state = "succeeded".into();
        self.store.finish_logout(&op)
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
            if op.kind == "login" && op.state == "authenticating" {
                op.result["authenticationMayBeActive"] = json!(true);
            }
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
        self.login_keyed(account, request, None).await
    }
    pub async fn login_keyed(
        self: &Arc<Self>,
        account: &str,
        mut request: LoginRequest,
        idempotency_key: Option<&str>,
    ) -> Result<Operation> {
        self.store.account(account)?;
        self.require_profile_identity(account)?;
        if request.force_qr {
            request.strategy = LoginStrategy::Qr;
        }
        if !(10..=900).contains(&request.timeout_seconds)
            || request.computer_name.is_empty()
            || request.computer_name.len() > 100
            || request.qr_refresh.max_refreshes > 3
            || (request.strategy == LoginStrategy::Qr && !request.allow_qr)
        {
            bail!("invalid login options");
        }
        let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        if let Some(key) = idempotency_key {
            if key.is_empty() || key.len() > 200 {
                bail!("invalid login idempotency key");
            }
            if let Some(existing) =
                self.store
                    .login_operation_by_key(account, key, &request_hash)?
            {
                return Ok(existing);
            }
        }
        let gate = self.gate(account)?;
        if gate.try_lock().is_err() {
            bail!("operation_in_progress");
        }
        let active_key = format!("login-active:{account}");
        let logout_epoch = self.store.runtime(account, "logout-epoch")?;
        let mut cancels = self
            .cancels
            .lock()
            .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?;
        if cancels.contains_key(&format!("logout-active:{account}")) {
            bail!("operation_in_progress");
        }
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
        let saved = if let Some(key) = idempotency_key {
            self.store.save_login_operation(&op, key, &request_hash)
        } else {
            self.store.save_operation(&op)
        };
        if let Err(error) = saved {
            if let Ok(mut map) = self.cancels.lock() {
                map.remove(&active_key);
                map.remove(&op.id);
            }
            return Err(error);
        }
        let core = self.clone();
        let worker_op = op.clone();
        tokio::spawn(async move {
            let _guard = gate.lock().await;
            let outcome = core
                .login_worker(worker_op.clone(), request, cancel, logout_epoch)
                .await;
            if let Err(error) = outcome {
                let _ = core.progress(&worker_op, "failed", login_failure_result(&error));
            }
            if let Ok(mut map) = core.cancels.lock() {
                map.remove(&active_key);
                map.remove(&worker_op.id);
            }
        });
        Ok(op)
    }
    fn progress(&self, op: &Operation, state: &str, result: Value) -> Result<()> {
        if self
            .store
            .operation(&op.account_id, &op.id)
            .is_ok_and(|current| current.state == "cancelled")
            && state != "cancelled"
        {
            return Ok(());
        }
        let mut next = op.clone();
        next.state = state.into();
        next.result = result;
        next.updated_at = now();
        self.store.save_operation(&next)
    }
    async fn login_worker(
        self: &Arc<Self>,
        op: Operation,
        request: LoginRequest,
        cancel: Arc<AtomicBool>,
        logout_epoch: Option<Vec<u8>>,
    ) -> Result<()> {
        if cancel.load(Ordering::SeqCst) || self.stopping.load(Ordering::SeqCst) {
            self.progress(&op, "cancelled", json!({}))?;
            return Ok(());
        }
        let start = Instant::now();
        let account = op.account_id.clone();
        let previous = self.state_unfenced(&account).ok();
        if let Some(state) = &previous {
            self.check_state_profile(&account, state)?;
        }
        let fenced = self.store.runtime(&account, "logged-out")?.is_some();
        if let Some(saved) = previous
            .as_ref()
            .filter(|_| request.strategy == LoginStrategy::Auto && !fenced)
        {
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
                    self.install_login_operation(
                        &account,
                        &state,
                        logout_epoch.as_deref(),
                        Some(&op.id),
                    )?;
                    let initialize = self.clone();
                    let profile_account = account.clone();
                    let profile_state = state.clone();
                    let profile = blocking(move || {
                        initialize.ensure_managed_profile(&profile_account, &profile_state)
                    })
                    .await?;
                    self.progress(
                        &op,
                        "succeeded",
                        json!({"method":"active_or_renewed","profile":profile}),
                    )?;
                    self.start_workers()?;
                    return Ok(());
                }
                Err(error) if !auth_rejected(&error) => return Err(error),
                Err(_) => {}
            }
        }
        let saved_source = if request.strategy == LoginStrategy::Qr {
            None
        } else {
            self.saved_device_source(&account, previous.as_ref())?
        };
        let saved_attempt =
            if let Some(official_profile::SavedDeviceSource::State(state)) = &saved_source {
                self.progress(&op, "resuming", json!({"method":"saved_device"}))?;
                let state = state.clone();
                let name = request.computer_name.clone();
                match blocking(move || client()?.start_saved_device_login(&state, &name)).await {
                    Ok(attempt) => Some(attempt),
                    Err(error) if saved_trust_rejected(&error) => None,
                    Err(error) => return Err(error),
                }
            } else if let Some(official_profile::SavedDeviceSource::Profile(saved)) = saved_source {
                self.progress(&op, "resuming", json!({"method":"official_saved_device"}))?;
                let mut device = DeviceProfile::generate("UnifiedPCLinux x86_64".into())?;
                device.device_id = saved.identity.request_device_id()?.to_vec();
                let name = request.computer_name.clone();
                let options = self.official_login_options(&account)?;
                match blocking(move || {
                    NativeClient::new(options)?.start_profile_saved_device_login(
                        &device,
                        &saved.identity.username,
                        &saved.auto_auth_key,
                        &saved.cookie,
                        &saved.key_info,
                        &name,
                    )
                })
                .await
                {
                    Ok(attempt) => Some(attempt),
                    Err(error) if saved_trust_rejected(&error) => None,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
        let mut attempt = if let Some(attempt) = saved_attempt {
            attempt
        } else {
            if !request.allow_qr || request.strategy == LoginStrategy::SavedDevice {
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
                let mut device = DeviceProfile::generate("UnifiedPCLinux x86_64".into())?;
                if let Some(binding) = self.identity_binding(&account)? {
                    let identity = self.read_bound_identity(&account, &binding)?;
                    device.device_id = identity.request_device_id()?.to_vec();
                    device.validate()?;
                }
                self.store.put_runtime(
                    &account,
                    "device",
                    &self
                        .vault
                        .seal(&account, "device", &serde_json::to_vec(&device)?)?,
                )?;
                device
            };
            if let Some(binding) = self.identity_binding(&account)? {
                let identity = self.read_bound_identity(&account, &binding)?;
                if identity.request_device_id()? != device.device_id {
                    bail!("account_device_mismatch");
                }
            }
            let name = request.computer_name.clone();
            let prefer_files = self.prefer_profile_login_files(&account, previous.is_some())?;
            let key_info = zeroize::Zeroizing::new(if prefer_files {
                self.official_login_key_info(&account)?
            } else {
                previous
                    .as_ref()
                    .map(|s| s.key_info.clone())
                    .unwrap_or_default()
            });
            let mut options = self.official_login_options(&account)?;
            if let Some(state) = previous.as_ref().filter(|_| !prefer_files) {
                options.long_host = state.long_host.clone();
                options.short_host = state.short_host.clone();
            }
            blocking(move || {
                NativeClient::new(options)?.start_profile_qr_login(device, &name, &key_info)
            })
            .await?
        };
        let mut generation = 1;
        let mut scanned = false;
        let mut poll_failures = 0u32;
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
                // Return the issued challenge even on a failed read. Never lose
                // its cookie/notify state or create a replacement QR on timeout.
                let status = client().and_then(|native| native.poll_login(&mut attempt));
                Ok((attempt, status))
            })
            .await?;
            attempt = result.0;
            let status = match result.1 {
                Ok(status) => {
                    poll_failures = 0;
                    status
                }
                Err(error) if wechat_protocol::is_retryable_login_poll_error(&error) => {
                    poll_failures = poll_failures.saturating_add(1);
                    self.progress(&op, if scanned || attempt.qr_png.is_empty() { "awaiting_approval" } else { "awaiting_scan" },
                        json!({"qrGeneration":generation,"expiresAt":attempt.expires_at,"qrAvailable":!attempt.qr_png.is_empty(),"connection":"retrying","reason":"temporary_transport_failure","pollFailures":poll_failures}))?;
                    // Both the issued challenge expiry and the overall operation
                    // deadline still bound retries. Cancellation keeps its fence.
                    tokio::time::sleep(Duration::from_secs(u64::from(poll_failures.min(5)))).await;
                    continue;
                }
                Err(error) => {
                    let service_code = error.chain().find_map(|cause| {
                        cause
                            .to_string()
                            .strip_prefix("Login poll returned ")
                            .and_then(|s| s.parse::<i32>().ok())
                    });
                    self.progress(&op, "failed", json!({"code":"login_poll_failed","serviceCode":service_code,"retryable":false}))?;
                    return Ok(());
                }
            };
            match status {
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
                    self.install_login_operation(
                        &account,
                        &state,
                        logout_epoch.as_deref(),
                        Some(&op.id),
                    )?;
                    let initialize = self.clone();
                    let profile_account = account.clone();
                    let profile_state = state.clone();
                    let profile = blocking(move || {
                        initialize.ensure_managed_profile(&profile_account, &profile_state)
                    })
                    .await?;
                    self.progress(
                        &op,
                        "succeeded",
                        json!({"method":"approved_login","profile":profile}),
                    )?;
                    self.start_workers()?;
                    return Ok(());
                }
                LoginStatus::Scanned => scanned = true,
                LoginStatus::Expired
                    if !scanned
                        && !attempt.qr_png.is_empty()
                        && request.allow_qr
                        && request.qr_refresh.mode == QrRefreshMode::Auto
                        && generation <= request.qr_refresh.max_refreshes as u32 =>
                {
                    generation += 1;
                    attempt = blocking(move || client()?.refresh_qr_login(&attempt)).await?;
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
        blocking(move || {
            let mut state = core.account_state(&account)?;
            let cursor = core.cursor(&account)?;
            let native = client()?;
            let batch = match native.sync(&mut state, &cursor) {
                Ok(batch) => batch,
                Err(error) if auth_rejected(&error) => {
                    state = native.renew_session(&state, "agent-wechat")?;
                    native.sync(&mut state, &cursor)?
                }
                Err(e) => return Err(e),
            };
            core.save_state(&account, &state)?;
            core.store.put_runtime(
                &account,
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )?;
            core.commit_sync_batch(&account, batch, "shortlink", "manual_or_fallback")
        })
        .await
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
        if self.media_job_by_key(account, key).is_ok() {
            bail!("idempotency_conflict");
        }
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
            if !core.store.claim_send(&account, &id)? {
                return Ok(());
            }
            let result = native.send_text(
                &mut state,
                &payload.thread_id,
                &payload.text,
                payload.created_at,
                payload.client_id,
            );
            // A logout may suppress credential installation during this RPC,
            // but cannot erase an authenticated, accepted send receipt.
            let authorization_saved = core.save_state(&account, &state).is_ok();
            if authorization_saved {
                core.store
                    .mark_validated_if_active(&account, &core.ownership_generation)?;
            }
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
            if updated.state == "accepted" {
                let _ = core.project_text_send(&account, &id);
            }
            Ok(())
        })
        .await
    }
    fn project_text_send(&self, account: &str, id: &str) -> Result<()> {
        let intent = self.store.send(account, id)?;
        let server = intent
            .server_id
            .as_deref()
            .context("missing accepted text identity")?;
        if intent.state != "accepted" {
            bail!("invalid text projection receipt");
        }
        let sealed = self.store.send_payload(account, id)?;
        let payload: SendPayload =
            serde_json::from_slice(&self.vault.open_record(account, "send-payload", &sealed)?)?;
        let source = self.project_accepted_text(
            account,
            &payload.thread_id,
            &payload.text,
            u64::from(payload.created_at),
            server,
            &format!("send-projection:{id}"),
        )?;
        self.store
            .finish_text_projection(account, id, &crate::message_id(&source)?)
    }
    fn recover_text_projections(&self, account: &str) -> Result<usize> {
        let mut done = 0;
        for id in self.store.pending_text_projections(account)? {
            if self.project_text_send(account, &id).is_ok() {
                done += 1;
            }
        }
        Ok(done)
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
        self.account_state(account)?;
        let sent = self.recover_text_projections(account)?;
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
            let mut blockers = Vec::new();
            let mut blocked_count = 0usize;
            for (index, command) in batch.commands.iter().enumerate() {
                let receipt = format!("projected:{batch_id}:{index}");
                if self.store.runtime(account, &receipt)?.is_some() {
                    continue;
                }
                let outcome = self.apply_sync_command(account, command, &receipt, &origin);
                match outcome {
                    Ok(()) => projected += 1,
                    Err(error) => {
                        complete = false;
                        unsupported += 1;
                        blocked_count += 1;
                        if blockers.len() < 50 {
                            blockers.push(sync_projection::projection_blocker(&error).unwrap_or_else(||json!({"commandId":command.id,"reason":"unsupported_or_unavailable_profile"})));
                        }
                    }
                }
            }
            if complete {
                self.store.finish_sync(account, &batch_id)?;
            } else {
                self.store.block_sync(account, &batch_id)?;
                self.store.append_event_once(account,&format!("blocked-v2:{batch_id}"),"projection.blocked",&json!({"batchId":batch_id,"reason":"unsupported_or_unavailable_profile","retryableAfterRepair":true,"commands":blockers,"blockedCommandCount":blocked_count,"truncated":blocked_count>50}))?;
            }
        }
        if self.store.pending_sync_count(account)? == 0 {
            let _ = self.publish_profile_checkpoint(account);
        }
        Ok(
            json!({"projectedCommands":projected,"projectedSends":sent,"pendingCommands":unsupported,"pendingBatches":self.store.pending_sync_count(account)?}),
        )
    }
    pub fn enable_profile_writes(
        &self,
        account: &str,
        own_username: &str,
        mmkv_store: &std::path::Path,
        format_key: &[u8],
    ) -> Result<()> {
        self.bind_profile_identity(account, mmkv_store, format_key)?;
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
        let identity = self.read_bound_identity(account, binding)?;
        if let Ok(state) = self.state_unfenced(account) {
            if identity.username != state.own_username || identity.uin != state.uin {
                bail!("account_identity_mismatch");
            }
            self.check_state_profile(account, &state)?;
        }
        Ok(())
    }
    pub(crate) fn write_options(
        &self,
        account: &str,
        intent: &str,
    ) -> Result<wechat_profile::WriteOptions> {
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
    #[cfg(test)]
    async fn mark_through(
        self: &Arc<Self>,
        account: &str,
        thread: &str,
        through: &str,
        expected: &str,
    ) -> Result<Value> {
        let core = self.clone();
        let account_owned = account.to_owned();
        let thread_owned = thread.to_owned();
        let through = through.to_owned();
        let ids = blocking(move || {
            let reader = core.profile_reader(&account_owned)?;
            let own = core
                .profile_username(&account_owned)?
                .context("profile_identity_unavailable")?;
            let target = crate::source_message_id(&through)?;
            let mut cursor = None;
            let mut found = false;
            let mut ids = Vec::new();
            let mut examined = 0;
            loop {
                let page = reader.messages(&wechat_profile::MessageQuery {
                    chat: Some(thread_owned.clone()),
                    own_username: Some(own.clone()),
                    limit: Some(200),
                    cursor,
                    ..Default::default()
                })?;
                for message in page.items {
                    examined += 1;
                    if examined > 100_000 {
                        bail!("read_boundary_capacity_exceeded");
                    }
                    if message.id == target {
                        found = true;
                    }
                    if found && message.outgoing == Some(false) {
                        ids.push(crate::message_id(&message.id)?);
                    }
                }
                let Some(next) = page.next_cursor else {
                    break;
                };
                cursor = Some(next);
            }
            if !found {
                bail!("invalid read boundary");
            }
            Ok(ids)
        })
        .await?;
        if ids.is_empty() {
            bail!("invalid empty read boundary");
        }
        self.mark_read_set(account, thread, ids, expected).await
    }
    async fn mark_read_set(
        self: &Arc<Self>,
        account: &str,
        thread: &str,
        ids: Vec<String>,
        expected: &str,
    ) -> Result<Value> {
        if ids.is_empty() || ids.len() > 100_000 || expected.len() > 128 {
            bail!("invalid read set");
        }
        let gate = self.gate(account)?;
        let _guard = gate.lock().await;
        let core = self.clone();
        let account = account.to_string();
        let thread = thread.to_string();
        let expected = expected.to_string();
        blocking(move||{
            let reader=core.profile_reader(&account)?;let own=core.profile_username(&account)?.context("profile_identity_unavailable")?;
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
            let sources=sources.into_iter().collect::<Vec<_>>();
            let mut removed=0usize;let mut before=None;let mut after=0i64;
            for chunk in sources.chunks(200) {
                let applied=wechat_profile::mark_read_messages(&location,&keys,&thread,chunk,&options)?;
                before.get_or_insert(applied.unread_count_before);after=applied.unread_count_after;removed+=applied.removed_unread_rows;
            }
            let projection=json!({"removedUnreadRows":removed,"unreadCountBefore":before,"unreadCountAfter":after,"exactIdsOnly":true});
            let reader=core.profile_reader(&account)?;
            let version=format!("{:x}",Sha256::digest(serde_json::to_vec(&(reader.generation(),&thread,&next))?));
            core.store.finish_read_projection(&account,&thread,&next,&durable,&json!({"threadId":thread,"version":version,"messageIds":ids}))?;
            let next=version;
            Ok(json!({"threadId":thread,"version":next,"projection":projection,"phoneReceiptSent":false}))
        }).await
    }
    /// Starts independent serialized account workers. A reconnect does not open
    /// a QR challenge; operator login remains an explicit operation.
    pub fn start_workers(self: &Arc<Self>) -> Result<()> {
        if !self.startup_profiles_recovered.swap(true, Ordering::AcqRel) {
            if let Err(error) = self.recover_managed_profiles() {
                self.startup_profiles_recovered
                    .store(false, Ordering::Release);
                return Err(error);
            }
        }
        self.resume_media_jobs()?;
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
            self.start_push_worker(&account.id)?;
            let core = self.clone();
            tokio::spawn(async move {
                let mut delay = 1;
                loop {
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    if core.stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let desired = core.store.runtime(&account.id, "desired").ok().flatten();
                    let auto_disabled = core
                        .store
                        .runtime(&account.id, "auto-connect")
                        .ok()
                        .flatten()
                        .as_deref()
                        == Some(b"0")
                        && core
                            .store
                            .runtime(&account.id, "validated-generation")
                            .ok()
                            .flatten()
                            .as_deref()
                            != Some(core.ownership_generation.as_bytes());
                    if desired.as_deref() != Some(b"connected") || auto_disabled {
                        delay = 2;
                        continue;
                    }
                    let _ = core.recover_read_projections(&account.id).await;
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
        if self.push_connected(account) {
            // The persistent connection owns account heartbeats and incoming
            // sync while healthy. Keep local recovery and queued sends moving;
            // a failed push connection immediately restores bounded polling.
            let guard = self.lock_account(account).await?;
            let core = self.clone();
            let owned = account.to_owned();
            blocking(move || {
                let _guard = guard;
                let _ = core.recover_text_projections(&owned)?;
                if core.store.pending_sync_count(&owned)? == 0 {
                    let _ = core.publish_profile_checkpoint(&owned)?;
                }
                Ok(())
            })
            .await?;
            self.drain_queued_sends(account).await?;
            return Ok(5);
        }
        let gate = self.gate(account)?;
        let heartbeat = {
            let _guard = gate.lock().await;
            let core = self.clone();
            let account = account.to_string();
            blocking(move || {
                let mut state = core.account_state(&account)?;
                let _ = core.recover_text_projections(&account)?;
                if core.store.pending_sync_count(&account)? == 0 {
                    let _ = core.publish_profile_checkpoint(&account);
                }
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
        self.drain_queued_sends(account).await?;
        Ok((heartbeat.next_interval as u64).clamp(5, 30))
    }
    async fn drain_queued_sends(self: &Arc<Self>, account: &str) -> Result<()> {
        for id in self.store.queued_sends(account)? {
            if self.stopping.load(Ordering::SeqCst)
                || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            {
                break;
            }
            self.perform_send(account, &id).await?;
        }
        Ok(())
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
fn login_failure_result(error: &anyhow::Error) -> Value {
    let message = error.to_string();
    let code = [
        "account_identity_mismatch",
        "account_database_password_mismatch",
        "account_device_mismatch",
        "profile_identity_unavailable",
        "profile_database_password_required",
        "profile_saved_key_info_unavailable",
        "profile_network_routing_unavailable",
        "profile_keys_unavailable",
        "profile_database_unavailable",
        "profile_changed_during_login",
        "profile_resource_limit",
        "profile_cipher_unavailable",
        "profile_io_failed",
        "profile_unavailable",
    ]
    .into_iter()
    .find(|code| message == *code)
    .unwrap_or_else(|| {
        if auth_rejected(error) {
            "auth_required"
        } else {
            "login_failed"
        }
    });
    let mut result = json!({"code":code,"retryable":false});
    if let Some(
        wechat_profile::ProfileError::KeyMismatch(relative)
        | wechat_profile::ProfileError::KeyUnavailable(relative)
        | wechat_profile::ProfileError::DatabaseOpen(relative),
    ) = error.downcast_ref::<wechat_profile::ProfileError>()
    {
        // Relative profile inventory names are useful for repair. Never include
        // source HOME paths, SQL, parameters or credential material.
        if relative.len() <= 4096
            && std::path::Path::new(relative)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            && !relative.contains(['\\', '\0'])
        {
            result["database"] = json!(relative);
        }
    }
    result
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
    #[test]
    fn login_admission_preserves_the_failure_kind_without_disclosing_credential_material() {
        use wechat_profile::ProfileError;
        for (error, code, database) in [
            (
                ProfileError::KeyMismatch("contact/contact.db".into()),
                "account_database_password_mismatch",
                Some("contact/contact.db"),
            ),
            (
                ProfileError::DatabaseOpen("contact/contact.db".into()),
                "profile_database_unavailable",
                Some("contact/contact.db"),
            ),
            (
                ProfileError::SourceChanged,
                "profile_changed_during_login",
                None,
            ),
            (ProfileError::ResourceLimit, "profile_resource_limit", None),
            (
                ProfileError::SqlcipherUnavailable,
                "profile_cipher_unavailable",
                None,
            ),
            (
                ProfileError::Io(std::io::Error::other("private path and secret parameters")),
                "profile_io_failed",
                None,
            ),
        ] {
            let error = super::identity::password_admission_error(error);
            let result = login_failure_result(&error);
            assert_eq!(result["code"], code);
            assert_eq!(result["database"].as_str(), database);
            assert_eq!(result["retryable"], false);
            assert!(!result.to_string().contains("secret"));
            assert!(!result.to_string().contains("private path"));
        }
        for relative in ["/private/home/contact.db", "../other.db", "bad\\path.db"] {
            let error = super::identity::password_admission_error(ProfileError::KeyMismatch(
                relative.into(),
            ));
            assert!(login_failure_result(&error).get("database").is_none());
        }
    }
    pub(super) fn state() -> AccountState {
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
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    #[test]
    fn only_authenticated_saved_trust_rejections_permit_qr_fallback() {
        for code in [-13, -2017] {
            assert!(saved_trust_rejected(&anyhow::anyhow!(
                "Saved-device login returned {code}"
            )));
        }
        for message in [
            "Saved-device login returned -20170",
            "Saved-device login returned -2017 trailing",
            "transport error -2017",
            "record authentication failed",
        ] {
            assert!(!saved_trust_rejected(&anyhow::anyhow!("{message}")));
        }
        assert!(!auth_rejected(&anyhow::anyhow!(
            "Saved-device login returned -2017"
        )));
    }
    #[test]
    fn auth_fallback_requires_a_whole_rejection_from_a_known_protocol_boundary() {
        for prefix in [
            "Native envelope returned ",
            "Heartbeat returned ",
            "Contact service returned ",
            "Automatic authentication returned ",
        ] {
            assert!(auth_rejected(
                &anyhow::anyhow!(format!("{prefix}-13")).context("operation failed")
            ));
            for suffix in ["-130", "-1317", "-13 extra", "-13/secret"] {
                assert!(!auth_rejected(&anyhow::anyhow!(format!(
                    "{prefix}{suffix}"
                ))));
            }
        }
        for message in [
            "network failed -13",
            "checksum -13",
            "profile /private/-13",
            "auth_required",
        ] {
            assert!(!auth_rejected(&anyhow::anyhow!(message.to_owned())));
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
    fn disabled_auto_connect_reports_disconnected_until_explicitly_validated() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Arc::new(Store::open(&root).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.restore_credentials("one", &serde_json::to_vec(&state()).unwrap())
            .unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store.put_runtime("one", "auto-connect", b"0").unwrap();
        assert_eq!(
            core.account_status("one").unwrap()["connection"],
            "disconnected"
        );
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        assert_eq!(core.account_status("one").unwrap()["connection"], "online");
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&root).unwrap());
        let core = Core::new(store.clone(), None).unwrap();
        let status = core.account_status("one").unwrap();
        assert_eq!(status["authenticated"], false);
        assert_eq!(status["connection"], "disconnected");
        store.put_runtime("one", "auto-connect", b"1").unwrap();
        assert_eq!(
            core.account_status("one").unwrap()["connection"],
            "reconnecting"
        );
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
    async fn existing_profile_identity_precedes_login_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Arc::new(Store::open(&root).unwrap());
        let account = store.create_account("one").unwrap();
        let format_key = [9; 16];
        let pair = wechat_profile::create_mmkv_v4(&format_key, [3; 16], 4096).unwrap();
        let accepted = state();
        let pair = wechat_profile::stage_mmkv_values(
            pair.data(),
            pair.metadata(),
            &format_key,
            &[
                (
                    "mmkv_key_user_name".into(),
                    Some(
                        wechat_profile::mmkv_string_value(accepted.own_username.as_bytes())
                            .unwrap()
                            .to_vec(),
                    ),
                ),
                ("mmkv_key_latest_login_uin".into(), Some(vec![1])),
                (
                    "key_wechat_device_id".into(),
                    Some(
                        wechat_profile::mmkv_string_value(&accepted.device.device_id)
                            .unwrap()
                            .to_vec(),
                    ),
                ),
            ],
        )
        .unwrap();
        let mmkv = account.profile_path.join("identity");
        std::fs::write(&mmkv, pair.data()).unwrap();
        std::fs::write(mmkv.with_extension("crc"), pair.metadata()).unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        // No account state exists yet. A populated, unidentified profile must
        // fail at admission without issuing a network request or operation.
        assert!(core
            .login("one", LoginRequest::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("profile_identity_unavailable"));
        core.bind_profile_identity("one", &mmkv, &format_key)
            .unwrap();
        let mut wrong = accepted.clone();
        wrong.uin = 2;
        assert!(core
            .install_login_state("one", &wrong, None)
            .unwrap_err()
            .to_string()
            .contains("account_identity_mismatch"));
        assert!(store.runtime("one", "account-state").unwrap().is_none());
        wrong = accepted.clone();
        wrong.own_username = "another_account".into();
        assert!(core.install_login_state("one", &wrong, None).is_err());
        wrong = accepted.clone();
        wrong.device.device_id[1] ^= 1;
        assert!(core
            .install_login_state("one", &wrong, None)
            .unwrap_err()
            .to_string()
            .contains("account_device_mismatch"));
        core.install_login_state("one", &accepted, None).unwrap();
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&root).unwrap());
        let core = Core::new(store, None).unwrap();
        assert_eq!(core.account_state("one").unwrap().uin, 1);
        assert!(core.save_state("one", &wrong).is_err());
    }
    #[tokio::test]
    async fn durable_batch_projects_before_event_and_exact_read_cas_preserves_other_state() {
        use wechat_protocol::{
            protobuf as p,
            wire::{SyncBatch, SyncCommand},
        };
        let t = tempfile::tempdir().unwrap();
        let profile = t
            .path()
            .join(wechat_profile::official_account_directory("synthetic_self", 1).unwrap());
        let (_, initial_keys, _) =
            wechat_profile::initialize_profile(&profile, b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap();
        let storage = profile.join("db_storage");
        let create = |relative: &str, sql: &str| {
            let db = rusqlite::Connection::open(storage.join(relative)).unwrap();
            db.execute_batch(
                "PRAGMA key='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';PRAGMA cipher_compatibility=4;",
            )
            .unwrap();
            db.execute_batch(sql).unwrap();
        };
        create("contact/contact.db","INSERT INTO name2id(username)VALUES('peer'),('synthetic_self');INSERT INTO contact(id,username,nick_name,remark,alias,local_type)VALUES(1,'peer','Peer','','',1),(2,'synthetic_self','Self','','',1);");
        create("session/session.db","INSERT INTO Name2Id VALUES('peer');INSERT INTO SessionTable VALUES('peer',1,0,0,0,0,0,'Before','Keep draft',7,100,100,99,0,1,0,'','',0);");
        create(
            "message/message_0.db",
            "INSERT INTO Name2Id VALUES('synthetic_self',1),('peer',1);",
        );
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        store.attach("one", &profile, true).unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let mut plain = vec![0];
        let frame = |bytes: &[u8]| {
            assert!(bytes.len() < 128);
            [vec![bytes.len() as u8], bytes.to_vec()].concat()
        };
        for (key, value) in [
            ("mmkv_key_user_name", frame(b"synthetic_self")),
            ("mmkv_key_latest_login_uin", vec![1]),
            ("key_wechat_device_id", frame(&state().device.device_id)),
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
        core.bind_profile_identity("one", &mmkv, &[9; 16]).unwrap();
        core.restore_credentials("one", &serde_json::to_vec(&state()).unwrap())
            .unwrap();
        let keys = initial_keys;
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
        assert!(core
            .enable_profile_writes("one", "wrong", &mmkv, &[9; 16])
            .is_err());
        assert!(core
            .enable_profile_writes("one", "synthetic_self", &mmkv, &[8; 16])
            .is_err());
        core.enable_profile_writes("one", "synthetic_self", &mmkv, &[9; 16])
            .unwrap();
        let mut verified = state();
        verified.database_password = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec();
        core.save_state("one", &verified).unwrap();
        let mut wrong_password = verified.clone();
        wrong_password.database_password = vec![8; 32];
        assert!(core
            .save_state("one", &wrong_password)
            .unwrap_err()
            .to_string()
            .contains("account_database_password_mismatch"));
        assert_eq!(
            core.account_state("one").unwrap().database_password,
            verified.database_password
        );
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
            .mark_read_set("one", "peer", vec![id.clone()], "stale")
            .await
            .is_err());
        let read = core
            .mark_read_set("one", "peer", vec![id.clone()], version)
            .await
            .unwrap();
        assert_eq!(read["projection"]["removedUnreadRows"], 1);
        assert_eq!(
            core.threads("one", &Default::default()).unwrap()["items"][0]["unreadCount"],
            0
        );
        assert!(core
            .mark_read_set("one", "peer", vec![id], version)
            .await
            .is_err());
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
        // Human mark-through follows frozen local order and leaves newer gaps.
        let mut later = Vec::new();
        for i in 2..=4u64 {
            let mut fields = p::decode(&batch.commands[0].payload).unwrap();
            for field in &mut fields {
                if field.number == 12 {
                    field.value = p::Value::Varint(1000 + i);
                }
                if field.number == 9 {
                    field.value = p::Value::Varint(101 + i);
                }
                if field.number == 13 {
                    field.value = p::Value::Varint(12 + i);
                }
            }
            let parts = fields
                .into_iter()
                .map(|field| match field.value {
                    p::Value::Varint(value) => p::uint(field.number, value),
                    p::Value::Bytes(value) => p::bytes(field.number, &value),
                    _ => panic!("unexpected synthetic field"),
                })
                .collect::<Vec<_>>();
            later.push(SyncCommand {
                id: 5,
                payload: p::message(&parts).unwrap(),
            });
        }
        let later = SyncBatch {
            commands: later,
            ..batch.clone()
        };
        store
            .stage_sync(
                "one",
                "later-messages",
                &core
                    .vault
                    .seal(
                        "one",
                        "apply:later-messages",
                        &serde_json::to_vec(&later).unwrap(),
                    )
                    .unwrap(),
                &core
                    .vault
                    .seal("one", "received-cursor", &later.cursor)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            core.recover_projection("one").unwrap()["projectedCommands"],
            3
        );
        let all = core.messages("one", "peer", None, None).unwrap();
        let ids = all["items"]
            .as_array()
            .unwrap()
            .iter()
            .take(3)
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(all["items"][0]["serverId"], "1004");
        let current = core.threads("one", &Default::default()).unwrap();
        core.mark_read_set(
            "one",
            "peer",
            vec![ids[0].clone()],
            current["items"][0]["readStateVersion"].as_str().unwrap(),
        )
        .await
        .unwrap();
        let current = core.threads("one", &Default::default()).unwrap();
        let through = core
            .mark_through(
                "one",
                "peer",
                &ids[2],
                current["items"][0]["readStateVersion"].as_str().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(through["projection"]["removedUnreadRows"], 1);
        assert_eq!(
            core.threads("one", &Default::default()).unwrap()["items"][0]["unreadCount"],
            1
        );
        // Crash after the official mutation but before sidecar completion. The
        // next owner replays the exact set and decrements no already-read rows.
        let current = core.threads("one", &Default::default()).unwrap();
        let intent = json!({"messageIds":[ids[1]],"expectedVersion":current["items"][0]["readStateVersion"]});
        store
            .begin_read(
                "one",
                "peer",
                &store.read_version("one", "peer").unwrap(),
                &intent,
            )
            .unwrap();
        let options = core
            .write_options("one", "synthetic-read-recovery")
            .unwrap();
        let (location, keys) = core.profile_location_keys("one").unwrap();
        wechat_profile::mark_read_messages(
            &location,
            &keys,
            "peer",
            &[crate::source_message_id(&ids[1]).unwrap()],
            &options,
        )
        .unwrap();
        drop(core);
        let core = Core::new(store.clone(), None).unwrap();
        assert_eq!(core.recover_read_projections("one").await.unwrap(), 1);
        assert!(store.pending_read_intents("one").unwrap().is_empty());
        assert_eq!(
            core.threads("one", &Default::default()).unwrap()["items"][0]["unreadCount"],
            0
        );
        // Logout closes the network session, not ownership of the local history.
        core.logout_local("one").unwrap();
        assert!(core.account_state("one").is_err());
        assert_eq!(
            core.messages("one", "peer", None, None).unwrap()["items"][0]["direction"],
            "incoming"
        );
        assert_eq!(
            core.profile_username("one").unwrap().as_deref(),
            Some("synthetic_self")
        );
        let latest = core.threads("one", &Default::default()).unwrap();
        let read = core
            .mark_through(
                "one",
                "peer",
                messages["items"][0]["id"].as_str().unwrap(),
                latest["items"][0]["readStateVersion"].as_str().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read["projection"]["removedUnreadRows"], 0);
        store.clear_runtime("one", "account-state").unwrap();
        assert_eq!(
            core.profile_username("one").unwrap().as_deref(),
            Some("synthetic_self")
        );
        assert_eq!(
            core.message("one", messages["items"][0]["id"].as_str().unwrap())
                .unwrap()["direction"],
            "incoming"
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
    #[test]
    fn accepted_fresh_login_initializes_bound_encrypted_profile_and_reopens() {
        use wechat_protocol::protobuf as p;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Arc::new(Store::open(&root).unwrap());
        store.create_account("fresh").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        core.configure_profile_format_key(&[9; 16]).unwrap();
        let mut accepted = state();
        accepted.database_password = vec![7; 32];
        accepted.key_info = vec![8; 32];
        accepted.auto_auth_key = p::message(&[
            p::bytes(1, &p::buffer(&[1; 16])),
            p::bytes(2, &p::buffer(&[2; 16])),
        ])
        .unwrap();
        core.install_login_state("fresh", &accepted, None).unwrap();
        let created = core.ensure_managed_profile("fresh", &accepted).unwrap();
        assert_eq!(created["state"], "created");
        assert_eq!(created["databases"], 7);
        assert!(store
            .runtime("fresh", "profile-bootstrap")
            .unwrap()
            .is_none());
        assert!(core.profile_reader("fresh").is_ok());
        assert_eq!(core.account_state("fresh").unwrap().uin, 1);
        assert_eq!(
            core.ensure_managed_profile("fresh", &accepted).unwrap()["state"],
            "existing"
        );
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&root).unwrap());
        let core = Core::new(store, None).unwrap();
        assert!(core.profile_reader("fresh").is_ok());
        assert_eq!(core.account_state("fresh").unwrap().uin, 1);
    }
}
