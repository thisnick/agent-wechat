//! Account data owned by NativeClient. Network workers take temporary snapshots;
//! they never hold the state lock while waiting for transport.
use super::{AccountState, NativeClient, NativeOptions};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::sync::Notify;
use zeroize::Zeroizing;

#[path = "account_components.rs"]
mod components;

/// The operation stopped before business bytes were handed to transport. This
/// marker is never attached to a timeout, rejected reply or post-handoff fence.
#[derive(Debug)]
pub struct NativeRequestNotSubmitted;

impl std::fmt::Display for NativeRequestNotSubmitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Native request stopped before business submission")
    }
}

impl std::error::Error for NativeRequestNotSubmitted {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLifecycle {
    Active,
    Stopped,
    Transferred,
}

/// Non-secret account lifecycle information. Credentials require an explicit
/// export; status/debug output must never contain an AccountState.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub lifecycle: AccountLifecycle,
    pub revision: u64,
    pub credential_generation: u64,
    pub in_flight: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountIdentity {
    pub uin: u32,
    pub own_username: String,
}

/// Cheap authority check for account workers. It carries no credentials and
/// becomes invalid on shutdown or any accepted credential renewal. It cannot
/// replace the final submission guard on an authenticated signaling request.
#[derive(Clone)]
pub struct AccountLease {
    authority: Arc<Authority>,
    credential_generation: u64,
}
struct Authority {
    active: AtomicBool,
    credential_generation: AtomicU64,
}
impl AccountLease {
    pub fn current(&self) -> bool {
        self.authority.active.load(Ordering::Acquire)
            && self.authority.credential_generation.load(Ordering::Acquire)
                == self.credential_generation
    }
}

/// Correlation values must be recorded with the application's send intent before
/// submission. Neither a task ID nor a transport error authorizes a blind retry.
#[derive(Clone, Debug)]
pub struct TextSendOptions {
    pub created_at: u32,
    pub client_message_id: u32,
    pub source: Option<String>,
}

/// Persist an accepted account update before it becomes visible to other client
/// operations. Implementations store data only: they must not call back into the
/// client, perform network operations, or implement authentication/retry policy.
/// A failed write leaves in-memory state unchanged and never retries a request.
pub trait AccountPersistence: Send + Sync {
    fn persist(&self, previous: &AccountState, accepted: &AccountState) -> Result<()>;
}

/// A verified response could not commit its account update. The response may
/// still be retained independently; this error never authorizes resubmission.
#[derive(Debug)]
pub struct AccountPersistenceFailed;
impl std::fmt::Display for AccountPersistenceFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Accepted account update could not be persisted")
    }
}
impl std::error::Error for AccountPersistenceFailed {}

/// One reserved terminal operation. Reservation stops account admission without
/// sending anything. An embedding daemon can commit local logout inhibition
/// before consuming this token to submit its optional remote logout.
/// Dropping the token leaves the account stopped and sends no request.
pub struct PendingLogout {
    exchange: Arc<Exchange>,
    state: AccountState,
    options: NativeOptions,
}

impl PendingLogout {
    pub async fn submit(self) -> Result<super::LogoutReply> {
        self.submit_guarded(|| Ok(())).await
    }

    /// Recheck the embedding application's committed logout operation immediately
    /// before business handoff. This token is consumed even on failure; neither
    /// a cancelled waiter nor an uncertain response permits automatic resubmission.
    pub async fn submit_guarded<F>(self, before_submit: F) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        self.exchange.account.drain_to(1).await?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        tokio::task::spawn_blocking(move || {
            let Self {
                exchange,
                mut state,
                options,
            } = self;
            let mut transport = NativeClient::new(options)?;
            let before_submit = Mutex::new(Some(before_submit));
            transport.submission_guard = Some(Arc::new(move || {
                let check = (|| {
                    ensure!(
                        !cancellation.load(Ordering::Acquire),
                        "Account logout cancelled before submission"
                    );
                    exchange.check_logout()
                })();
                check.map_err(|error| error.context(NativeRequestNotSubmitted))?;
                let guard = before_submit
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Logout guard unavailable"))?
                    .take()
                    .context("Logout already submitted")?;
                guard().map_err(|error| error.context(NativeRequestNotSubmitted))
            }));
            transport.logout_snapshot(&mut state)
        })
        .await
        .context("Account logout worker failed")?
    }
}

pub(super) struct OwnedAccount {
    data: Mutex<AccountData>,
    changed: Notify,
    authority: Arc<Authority>,
    components: components::AccountComponents,
}

struct AccountData {
    state: Option<AccountState>,
    revision: u64,
    credential_generation: u64,
    lifecycle: AccountLifecycle,
    in_flight: usize,
    persistence: Option<Arc<dyn AccountPersistence>>,
}

struct Exchange {
    account: Arc<OwnedAccount>,
    revision: u64,
    credential_generation: u64,
    non_cookie: [u8; 32],
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl OwnedAccount {
    fn lock(&self) -> Result<MutexGuard<'_, AccountData>> {
        self.data
            .lock()
            .map_err(|_| anyhow::anyhow!("Account state lock poisoned"))
    }

    fn begin(self: &Arc<Self>) -> Result<(Exchange, AccountState)> {
        self.begin_with_terminal(false)
    }

    fn begin_with_terminal(self: &Arc<Self>, terminal: bool) -> Result<(Exchange, AccountState)> {
        let mut data = self.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Active
                && self.authority.active.load(Ordering::Acquire),
            "Account is stopped"
        );
        let state = data
            .state
            .as_ref()
            .context("Account state transferred")?
            .clone();
        let non_cookie = non_cookie_digest(&state)?;
        data.in_flight = data
            .in_flight
            .checked_add(1)
            .context("Too many account requests")?;
        let exchange = Exchange {
            account: self.clone(),
            revision: data.revision,
            credential_generation: data.credential_generation,
            non_cookie,
        };
        if terminal {
            data.lifecycle = AccountLifecycle::Stopped;
            self.authority.active.store(false, Ordering::Release);
        }
        drop(data);
        if terminal {
            self.changed.notify_waiters();
        }
        Ok((exchange, state))
    }

    fn stop(&self) -> Result<()> {
        self.authority.active.store(false, Ordering::Release);
        {
            let mut data = self.lock()?;
            if data.lifecycle == AccountLifecycle::Active {
                data.lifecycle = AccountLifecycle::Stopped;
            }
        }
        self.changed.notify_waiters();
        Ok(())
    }

    async fn stopped(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self
                .lock()
                .is_ok_and(|data| data.lifecycle == AccountLifecycle::Active)
            {
                return;
            }
            notified.await;
        }
    }

    async fn stop_and_drain(&self) -> Result<()> {
        self.stop()?;
        self.drain_to(0).await
    }

    async fn drain_to(&self, remaining: usize) -> Result<()> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock()?.in_flight == remaining {
                return Ok(());
            }
            notified.await;
        }
    }
}

impl Exchange {
    fn check_logout(&self) -> Result<()> {
        let data = self.account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Stopped && data.in_flight == 1,
            "Logout is not the exclusive account operation"
        );
        let current = data.state.as_ref().context("Account state transferred")?;
        ensure!(
            data.credential_generation == self.credential_generation
                && non_cookie_digest(current)? == self.non_cookie,
            "Account authentication superseded logout"
        );
        Ok(())
    }
    /// Check immediately before handing business bytes to transport. A request
    /// already handed off cannot be recalled by a later shutdown.
    fn check(&self) -> Result<()> {
        let data = self.account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Active,
            "Account stopped during request"
        );
        let current = data.state.as_ref().context("Account state transferred")?;
        ensure!(
            data.credential_generation == self.credential_generation
                && non_cookie_digest(current)? == self.non_cookie,
            "Account credentials changed during request"
        );
        Ok(())
    }

    fn accept_cookie(&self, returned: &AccountState) -> Result<()> {
        returned.validate()?;
        ensure!(
            non_cookie_digest(returned)? == self.non_cookie,
            "Unexpected account credential change"
        );
        let mut data = self.account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Active,
            "Account stopped during request"
        );
        let current = data.state.as_ref().context("Account state transferred")?;
        ensure!(
            data.credential_generation == self.credential_generation
                && non_cookie_digest(current)? == self.non_cookie,
            "Account credentials changed during request"
        );
        // A concurrent response already adopted a newer cookie. Preserve that
        // whole accepted state rather than overwriting it with a stale snapshot.
        if data.revision == self.revision && current.cookie != returned.cookie {
            let revision = data
                .revision
                .checked_add(1)
                .context("Account revision exhausted")?;
            if let Some(storage) = &data.persistence {
                storage
                    .persist(current, returned)
                    .map_err(|error| error.context(AccountPersistenceFailed))?;
            }
            data.state
                .as_mut()
                .context("Account state transferred")?
                .cookie
                .clone_from(&returned.cookie);
            data.revision = revision;
        }
        Ok(())
    }

    fn accept_credentials(&self, returned: AccountState) -> Result<()> {
        returned.validate()?;
        let mut data = self.account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Active,
            "Account stopped during authentication"
        );
        let current = data.state.as_ref().context("Account state transferred")?;
        ensure!(
            data.credential_generation == self.credential_generation
                && non_cookie_digest(current)? == self.non_cookie,
            "Account authentication superseded"
        );
        ensure!(
            current.uin == returned.uin
                && current.own_username == returned.own_username
                && current.device.device_id == returned.device.device_id
                && current.device.device_type == returned.device.device_type
                && current.device.client_version == returned.device.client_version,
            "Account authentication identity mismatch"
        );
        let revision = data
            .revision
            .checked_add(1)
            .context("Account revision exhausted")?;
        let generation = data
            .credential_generation
            .checked_add(1)
            .context("Account credential generation exhausted")?;
        if let Some(storage) = &data.persistence {
            storage
                .persist(current, &returned)
                .map_err(|error| error.context(AccountPersistenceFailed))?;
        }
        data.state = Some(returned);
        data.revision = revision;
        data.credential_generation = generation;
        self.account
            .authority
            .credential_generation
            .store(generation, Ordering::Release);
        Ok(())
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        // Release even after a worker panic. Other access still fails closed
        // when the mutex is poisoned; waiters must nevertheless be awakened.
        let mut data = self
            .account
            .data
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        data.in_flight -= 1;
        drop(data);
        self.account.changed.notify_waiters();
    }
}

fn non_cookie_digest(state: &AccountState) -> Result<[u8; 32]> {
    let mut without_cookie = state.clone();
    zeroize::Zeroize::zeroize(&mut without_cookie.cookie);
    without_cookie.cookie.clear();
    let encoded = Zeroizing::new(serde_json::to_vec(&without_cookie)?);
    Ok(openssl::sha::sha256(&encoded))
}

impl NativeClient {
    pub(super) fn check_snapshot_transport(&self) -> Result<()> {
        ensure!(
            self.account.is_none(),
            "Explicit-state transport cannot use a bound account client"
        );
        Ok(())
    }

    pub(super) fn check_submission(&self) -> Result<()> {
        if let Some(check) = &self.submission_guard {
            check()?;
        }
        Ok(())
    }
    /// Bind an accepted account to this client. Clones share the same owner;
    /// construction performs no network operation or credential persistence.
    pub fn from_account(state: AccountState, options: NativeOptions) -> Result<Self> {
        state.validate()?;
        let mut client = Self::new(options)?;
        client.account = Some(Arc::new(OwnedAccount {
            data: Mutex::new(AccountData {
                state: Some(state),
                revision: 0,
                credential_generation: 0,
                lifecycle: AccountLifecycle::Active,
                in_flight: 0,
                persistence: None,
            }),
            changed: Notify::new(),
            authority: Arc::new(Authority {
                active: AtomicBool::new(true),
                credential_generation: AtomicU64::new(0),
            }),
            components: components::AccountComponents::default(),
        }));
        Ok(client)
    }

    /// Use a data-only storage callback for accepted updates. The supplied state
    /// must already be loaded from accepted storage; construction does not write
    /// it back or treat importing bytes as a new authentication acceptance.
    pub fn from_account_with_persistence(
        state: AccountState,
        options: NativeOptions,
        persistence: Arc<dyn AccountPersistence>,
    ) -> Result<Self> {
        let client = Self::from_account(state, options)?;
        client.owned_account()?.lock()?.persistence = Some(persistence);
        Ok(client)
    }

    fn owned_account(&self) -> Result<&Arc<OwnedAccount>> {
        self.account
            .as_ref()
            .context("Native client has no bound account")
    }

    pub fn account_status(&self) -> Result<AccountStatus> {
        let data = self.owned_account()?.lock()?;
        Ok(AccountStatus {
            lifecycle: data.lifecycle,
            revision: data.revision,
            credential_generation: data.credential_generation,
            in_flight: data.in_flight,
        })
    }

    pub fn account_identity(&self) -> Result<AccountIdentity> {
        let data = self.owned_account()?.lock()?;
        let state = data.state.as_ref().context("Account state transferred")?;
        Ok(AccountIdentity {
            uin: state.uin,
            own_username: state.own_username.clone(),
        })
    }

    pub fn account_lease(&self) -> Result<AccountLease> {
        let account = self.owned_account()?;
        let data = account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Active
                && account.authority.active.load(Ordering::Acquire),
            "Account is stopped"
        );
        Ok(AccountLease {
            authority: account.authority.clone(),
            credential_generation: data.credential_generation,
        })
    }

    /// Export secret state for persistence. This snapshot does not transfer
    /// ownership or authorize another writer; use quiesce_and_export for handoff.
    pub fn export_account_state(&self) -> Result<AccountState> {
        self.owned_account()?
            .lock()?
            .state
            .as_ref()
            .cloned()
            .context("Account state transferred")
    }

    /// Stop account-bound admission and wait for admitted network workers to
    /// finish. Cancelling this future does not reopen admission.
    pub async fn shutdown(&self) -> Result<()> {
        self.owned_account()?.stop_and_drain().await
    }

    /// Fence new submissions immediately. Network requests already handed off
    /// retain their uncertain outcome; shutdown waits for their workers to exit.
    pub fn stop(&self) -> Result<()> {
        self.owned_account()?.stop()
    }

    /// Reserve one terminal operation before the embedding application commits
    /// its own local inhibition. No network work is started by reservation.
    pub fn prepare_logout(&self) -> Result<PendingLogout> {
        let account = self.owned_account()?;
        let (exchange, state) = account
            .begin_with_terminal(true)
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        Ok(PendingLogout {
            exchange: Arc::new(exchange),
            state,
            options: self.options.clone(),
        })
    }

    /// Completes when this account owner stops, including through another clone.
    pub async fn stopped(&self) -> Result<()> {
        self.owned_account()?.stopped().await;
        Ok(())
    }

    /// Stop and drain the client, then move its credentials out exactly once.
    /// All cloned handles remain stopped and cannot export the transferred state.
    pub async fn quiesce_and_export(&self) -> Result<AccountState> {
        let account = self.owned_account()?;
        account.stop_and_drain().await?;
        let mut data = account.lock()?;
        let state = data
            .state
            .take()
            .context("Account state already transferred")?;
        data.lifecycle = AccountLifecycle::Transferred;
        Ok(state)
    }

    async fn account_request<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>
            + Send
            + 'static,
    {
        let (exchange, state) = self
            .owned_account()?
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let options = self.options.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        tokio::task::spawn_blocking(move || {
            Self::execute_account_request(options, exchange, state, cancellation, operation)
        })
        .await
        .context("Account request worker failed")?
    }

    fn account_request_blocking<T, F>(&self, operation: F) -> Result<T>
    where
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>,
    {
        let (exchange, state) = self
            .owned_account()?
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        Self::execute_account_request(
            self.options.clone(),
            exchange,
            state,
            Arc::new(AtomicBool::new(false)),
            operation,
        )
    }

    fn execute_account_request<T, F>(
        options: NativeOptions,
        exchange: Exchange,
        mut state: AccountState,
        cancellation: Arc<AtomicBool>,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>,
    {
        let exchange = Arc::new(exchange);
        // This transport gets a temporary request snapshot, not another owner.
        let mut transport = NativeClient::new(options)?;
        let submission_exchange = exchange.clone();
        let prior_admission = AtomicBool::new(false);
        transport.submission_guard = Some(Arc::new(move || {
            let check = (|| {
                ensure!(
                    !cancellation.load(Ordering::Acquire),
                    "Account request cancelled before submission"
                );
                submission_exchange.check()
            })();
            match check {
                Ok(()) => {
                    prior_admission.store(true, Ordering::Release);
                    Ok(())
                }
                Err(error) if !prior_admission.load(Ordering::Acquire) => {
                    Err(error.context(NativeRequestNotSubmitted))
                }
                // A logical operation can contain several chunks. Stopping a
                // later chunk never proves the entire operation was unsent.
                Err(error) => Err(error),
            }
        }));
        let result = operation(&transport, &mut state, &|| transport.check_submission());
        // Cookie adoption still follows verified transport when a decoded
        // application result rejects the operation. Preserve original uncertainty.
        let accepted = exchange.accept_cookie(&state);
        match result {
            Ok(value) => {
                accepted?;
                Ok(value)
            }
            Err(error) => match accepted {
                Ok(()) => Err(error),
                Err(account_error) => Err(error.context(account_error)),
            },
        }
    }
}

#[path = "account_request.rs"]
mod request;
pub use request::AccountRequest;

#[path = "account_push.rs"]
mod push;
pub use push::AccountPushConnection;

#[path = "account_operations.rs"]
mod operations;

async fn wait_for_poll<R, C, S>(reply: R, cancellation: C, stopped: S) -> Result<Vec<u8>>
where
    R: Future<Output = Result<Vec<u8>>>,
    C: Future<Output = ()>,
    S: Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = stopped => Err(crate::calls::host_transport::CallLiveSyncInterrupted.into()),
        () = cancellation => Err(crate::calls::host_transport::CallLiveSyncInterrupted.into()),
        result = reply => result,
    }
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
