//! Account data owned by NativeClient. Network workers take temporary snapshots;
//! they never hold the state lock while waiting for transport.
use super::{AccountState, NativeClient, NativeOptions};
use crate::errors::AccountInterrupted;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Condvar;
use std::{
    borrow::Cow,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};
use tokio::sync::Notify;
use zeroize::Zeroizing;

#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub trait AccountPersistence: Send + Sync {
    fn persist(&self, previous: &AccountState, accepted: &AccountState) -> Result<()>;
}

/// Persist an accepted account update before it becomes visible to other client
/// operations. Implementations store data only: they must not call back into the
/// client, perform network operations, or implement authentication/retry policy.
/// A failed write leaves in-memory state unchanged and never retries a request.
#[cfg(target_arch = "wasm32")]
pub trait AccountPersistence {
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
    transport: Option<crate::transport::ProtocolTransport>,
}

impl PendingLogout {
    /// Wait for requests admitted before the reservation, then submit the logout
    /// once on the configured async transport or Tokio's blocking pool.
    pub async fn submit(self) -> Result<super::LogoutReply> {
        self.submit_guarded(|| Ok(())).await
    }

    /// Recheck the embedding application's committed logout operation immediately
    /// before business handoff. This token is consumed even on failure; neither
    /// a cancelled waiter nor an uncertain response permits automatic resubmission.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn submit_guarded<F>(self, before_submit: F) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        self.exchange.account.drain_to(1).await?;
        if self.transport.is_some() {
            return self.submit_async(before_submit).await;
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        tokio::task::spawn_blocking(move || self.submit_on_worker(cancellation, before_submit))
            .await
            .context("Account logout worker failed")?
    }

    /// Recheck the embedding application's committed logout operation immediately
    /// before business handoff. This token is consumed even on failure; neither
    /// a cancelled waiter nor an uncertain response permits automatic resubmission.
    #[cfg(target_arch = "wasm32")]
    pub async fn submit_guarded<F>(self, before_submit: F) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + 'static,
    {
        self.exchange.account.drain_to(1).await?;
        self.submit_async(before_submit).await
    }

    async fn submit_async<F>(self, before_submit: F) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + crate::transport::MaybeSend + 'static,
    {
        let Self {
            exchange,
            state,
            options,
            transport,
        } = self;
        let transport = transport.context("Native client has no async transport configured")?;
        run_detached_exchange(&transport.clone(), move |cancellation| async move {
            let request = super::logout::request(&state)?;
            let session = crate::mmtls::AsyncSession::connect(
                &state.long_host,
                options.timeout,
                options.receive_budget,
                &*transport.streams,
                transport.clock.clone(),
            )
            .await?;
            let short = session.short.clone();
            let _ = session.close().await;
            let packet = crate::business::AuthenticatedPacket::prepare(
                &state,
                super::logout::CGI,
                &request,
            )?;
            if cancellation.load(Ordering::Acquire) {
                return Err(
                    anyhow::anyhow!("Account logout cancelled before submission")
                        .context(NativeRequestNotSubmitted),
                );
            }
            exchange
                .check_logout()
                .map_err(|error| error.context(NativeRequestNotSubmitted))?;
            before_submit().map_err(|error| error.context(NativeRequestNotSubmitted))?;
            let reply = crate::mmtls::short_exchange_async(
                &short,
                &state.short_host,
                super::logout::PATH,
                &packet.packet,
                options.timeout,
                &*transport.http,
                &*transport.clock,
            )
            .await?;
            let (plain, _cookie) = packet.decode_verified_transport_reply(&reply)?;
            super::logout::response(&plain)
        })
        .await
    }

    /// As [`Self::submit`], on the caller's blocking worker. No Tokio runtime is
    /// needed. Do not call it on an async executor thread or from inside an
    /// account request: it waits for every earlier request to finish.
    ///
    /// Clients built with a [`crate::transport::ProtocolTransport`] must use
    /// [`Self::submit`] instead; calling this method refuses with
    /// [`NativeRequestNotSubmitted`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn submit_blocking(self) -> Result<super::LogoutReply> {
        self.submit_guarded_blocking(|| Ok(()))
    }

    /// As [`Self::submit_guarded`], on the caller's blocking worker. The token
    /// is consumed even on failure, and an uncertain response never permits
    /// automatic resubmission.
    ///
    /// Clients built with a [`crate::transport::ProtocolTransport`] must use
    /// [`Self::submit_guarded`] instead; calling this method refuses with
    /// [`NativeRequestNotSubmitted`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn submit_guarded_blocking<F>(self, before_submit: F) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        if self.transport.is_some() {
            return Err(anyhow::anyhow!(
                "Blocking logout cannot bypass a configured async transport"
            )
            .context(NativeRequestNotSubmitted));
        }
        self.exchange.account.drain_to_blocking(1)?;
        self.submit_on_worker(Arc::new(AtomicBool::new(false)), before_submit)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn submit_on_worker<F>(
        self,
        cancellation: Arc<AtomicBool>,
        before_submit: F,
    ) -> Result<super::LogoutReply>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        let Self {
            exchange,
            mut state,
            options,
            transport: _,
        } = self;
        let mut transport = NativeClient::new(options)?;
        transport.submission_guard = Some(logout_guard(exchange, cancellation, before_submit));
        transport.logout_snapshot(&mut state)
    }
}

/// Final check before the logout bytes are handed to transport. It passes at
/// most once; every refusal is marked as not submitted.
#[cfg(not(target_arch = "wasm32"))]
fn logout_guard<F>(
    exchange: Arc<Exchange>,
    cancellation: Arc<AtomicBool>,
    before_submit: F,
) -> Arc<dyn Fn() -> Result<()> + Send + Sync>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    let before_submit = Mutex::new(Some(before_submit));
    Arc::new(move || {
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
    })
}

pub(super) struct OwnedAccount {
    data: Mutex<AccountData>,
    changed: Notify,
    // Wakes blocking waiters for the same changes as `changed`.
    #[cfg(not(target_arch = "wasm32"))]
    changed_blocking: Condvar,
    authority: Arc<Authority>,
    #[cfg(not(target_arch = "wasm32"))]
    components: components::AccountComponents,
    /// Short-link ticket shared by this owner's request transports.
    short_link: Arc<crate::mmtls::ShortLinkCache>,
}

struct AccountData {
    state: Option<AccountState>,
    revision: u64,
    credential_generation: u64,
    lifecycle: AccountLifecycle,
    in_flight: usize,
    persistence: Option<Arc<dyn AccountPersistence>>,
}

impl AccountData {
    /// Reason a request cannot use this account in its current lifecycle.
    fn interruption(&self) -> AccountInterrupted {
        match self.lifecycle {
            AccountLifecycle::Transferred => AccountInterrupted::Transferred,
            AccountLifecycle::Active | AccountLifecycle::Stopped => AccountInterrupted::Stopped,
        }
    }

    fn ensure_active(&self, message: &'static str) -> Result<()> {
        if self.lifecycle == AccountLifecycle::Active {
            Ok(())
        } else {
            Err(self.interruption().because(message))
        }
    }
}

fn transferred() -> anyhow::Error {
    AccountInterrupted::Transferred.because("Account state transferred")
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

async fn run_detached_exchange<T, F>(
    transport: &crate::transport::ProtocolTransport,
    build_future: impl FnOnce(Arc<AtomicBool>) -> F,
) -> Result<T>
where
    T: crate::transport::MaybeSend + 'static,
    F: Future<Output = Result<T>> + crate::transport::MaybeSend + 'static,
{
    let cancellation = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let fut = build_future(cancellation);
    if let Some(spawner) = transport.spawner() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        spawner.spawn(Box::pin(async move {
            let _ = tx.send(fut.await);
        }));
        return rx
            .await
            .context("Account request task terminated without returning a result")?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        handle.spawn(async move {
            let _ = tx.send(fut.await);
        });
        return rx
            .await
            .context("Account request task terminated without returning a result")?;
    }
    fut.await
}

impl OwnedAccount {
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    fn new(state: AccountState, persistence: Option<Arc<dyn AccountPersistence>>) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(AccountData {
                state: Some(state),
                revision: 0,
                credential_generation: 0,
                lifecycle: AccountLifecycle::Active,
                in_flight: 0,
                persistence,
            }),
            changed: Notify::new(),
            #[cfg(not(target_arch = "wasm32"))]
            changed_blocking: Condvar::new(),
            authority: Arc::new(Authority {
                active: AtomicBool::new(true),
                credential_generation: AtomicU64::new(0),
            }),
            #[cfg(not(target_arch = "wasm32"))]
            components: components::AccountComponents::default(),
            short_link: Arc::default(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, AccountData>> {
        self.data
            .lock()
            .map_err(|_| anyhow::anyhow!("Account state lock poisoned"))
    }

    /// Call after releasing the data lock, once lifecycle or `in_flight` changed.
    fn notify_changed(&self) {
        self.changed.notify_waiters();
        #[cfg(not(target_arch = "wasm32"))]
        self.changed_blocking.notify_all();
    }

    fn begin(self: &Arc<Self>) -> Result<(Exchange, AccountState)> {
        self.begin_with_terminal(false)
    }

    fn begin_with_terminal(self: &Arc<Self>, terminal: bool) -> Result<(Exchange, AccountState)> {
        let mut data = self.lock()?;
        if !self.authority.active.load(Ordering::Acquire) {
            return Err(data.interruption().because("Account is stopped"));
        }
        data.ensure_active("Account is stopped")?;
        let state = data.state.as_ref().ok_or_else(transferred)?.clone();
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
            self.notify_changed();
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
        self.notify_changed();
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

#[cfg(not(target_arch = "wasm32"))]
impl OwnedAccount {
    /// Blocking form of `drain_to`. Counts only change under the data lock, so
    /// a waiter cannot miss the wakeup that follows a change.
    fn drain_to_blocking(&self, remaining: usize) -> Result<()> {
        let mut data = self.lock()?;
        while data.in_flight != remaining {
            data = self
                .changed_blocking
                .wait(data)
                .map_err(|_| anyhow::anyhow!("Account state lock poisoned"))?;
        }
        Ok(())
    }

    fn stop_and_drain_blocking(&self) -> Result<()> {
        self.stop()?;
        self.drain_to_blocking(0)
    }
}

impl Exchange {
    fn check_logout(&self) -> Result<()> {
        let data = self.account.lock()?;
        ensure!(
            data.lifecycle == AccountLifecycle::Stopped && data.in_flight == 1,
            "Logout is not the exclusive account operation"
        );
        let current = data.state.as_ref().ok_or_else(transferred)?;
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
        data.ensure_active("Account stopped during request")?;
        let current = data.state.as_ref().ok_or_else(transferred)?;
        if data.credential_generation != self.credential_generation
            || non_cookie_digest(current)? != self.non_cookie
        {
            return Err(AccountInterrupted::CredentialsSuperseded
                .because("Account credentials changed during request"));
        }
        Ok(())
    }

    fn accept_cookie(&self, returned: &AccountState) -> Result<()> {
        returned.validate()?;
        ensure!(
            non_cookie_digest(returned)? == self.non_cookie,
            "Unexpected account credential change"
        );
        let mut data = self.account.lock()?;
        data.ensure_active("Account stopped during request")?;
        let current = data.state.as_ref().ok_or_else(transferred)?;
        if data.credential_generation != self.credential_generation
            || non_cookie_digest(current)? != self.non_cookie
        {
            return Err(AccountInterrupted::CredentialsSuperseded
                .because("Account credentials changed during request"));
        }
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
                .ok_or_else(transferred)?
                .cookie
                .clone_from(&returned.cookie);
            data.revision = revision;
        }
        Ok(())
    }

    fn accept_credentials(&self, returned: AccountState) -> Result<()> {
        returned.validate()?;
        let mut data = self.account.lock()?;
        data.ensure_active("Account stopped during authentication")?;
        let current = data.state.as_ref().ok_or_else(transferred)?;
        if data.credential_generation != self.credential_generation
            || non_cookie_digest(current)? != self.non_cookie
        {
            return Err(AccountInterrupted::CredentialsSuperseded
                .because("Account authentication superseded"));
        }
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
        self.account.notify_changed();
    }
}

fn non_cookie_digest(state: &AccountState) -> Result<[u8; 32]> {
    let mut without_cookie = state.clone();
    zeroize::Zeroize::zeroize(&mut without_cookie.cookie);
    without_cookie.cookie.clear();
    let encoded = Zeroizing::new(serde_json::to_vec(&without_cookie)?);
    Ok(crate::crypto::sha256_array(&encoded))
}

impl NativeClient {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn check_snapshot_transport(&self) -> Result<()> {
        ensure!(
            self.account.is_none(),
            "Explicit-state transport cannot use a bound account client"
        );
        ensure!(
            self.transport.is_none(),
            "Blocking transport methods cannot bypass a configured async transport"
        );
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn check_submission(&self) -> Result<()> {
        if let Some(check) = &self.submission_guard {
            check()?;
        }
        Ok(())
    }

    /// Bind an accepted account to this client. Clones share the same owner;
    /// construction performs no network operation or credential persistence.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_account(state: AccountState, options: NativeOptions) -> Result<Self> {
        state.validate()?;
        let mut client = Self::new(options)?;
        client.account = Some(OwnedAccount::new(state, None));
        Ok(client)
    }

    /// Use a data-only storage callback for accepted updates. The supplied state
    /// must already be loaded from accepted storage; construction does not write
    /// it back or treat importing bytes as a new authentication acceptance.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_account_with_persistence(
        state: AccountState,
        options: NativeOptions,
        persistence: Arc<dyn AccountPersistence>,
    ) -> Result<Self> {
        let client = Self::from_account(state, options)?;
        client.owned_account()?.lock()?.persistence = Some(persistence);
        Ok(client)
    }

    /// Bind an accepted account to a client backed by a pluggable
    /// [`crate::transport::ProtocolTransport`].
    ///
    /// Clones share the same account owner, short-link ticket cache, and
    /// transport. Construction performs no network operation or persistence.
    pub fn from_account_with_transport(
        state: AccountState,
        options: NativeOptions,
        transport: crate::transport::ProtocolTransport,
    ) -> Result<Self> {
        state.validate()?;
        let mut client = Self::with_transport(options, transport)?;
        client.account = Some(OwnedAccount::new(state, None));
        Ok(client)
    }

    /// Bind an accepted account with both a pluggable
    /// [`crate::transport::ProtocolTransport`] and a data-only persistence
    /// callback. Construction performs no network operation or initial write.
    pub fn from_account_with_transport_and_persistence(
        state: AccountState,
        options: NativeOptions,
        transport: crate::transport::ProtocolTransport,
        persistence: Arc<dyn AccountPersistence>,
    ) -> Result<Self> {
        state.validate()?;
        let mut client = Self::with_transport(options, transport)?;
        client.account = Some(OwnedAccount::new(state, Some(persistence)));
        Ok(client)
    }

    /// Change transport deadlines/budgets for a handle while retaining the same
    /// account owner, configured transport, and admission rules. This never
    /// imports credential state.
    pub fn with_request_options(&self, options: NativeOptions) -> Result<Self> {
        let mut handle = Self::from_validated_options(options, self.transport.clone())?;
        handle.account = Some(self.owned_account()?.clone());
        Ok(handle)
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
        let state = data.state.as_ref().ok_or_else(transferred)?;
        Ok(AccountIdentity {
            uin: state.uin,
            own_username: state.own_username.clone(),
        })
    }

    pub fn account_lease(&self) -> Result<AccountLease> {
        let account = self.owned_account()?;
        let data = account.lock()?;
        if !account.authority.active.load(Ordering::Acquire) {
            return Err(data.interruption().because("Account is stopped"));
        }
        data.ensure_active("Account is stopped")?;
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
            .ok_or_else(transferred)
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
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub fn prepare_logout(&self) -> Result<PendingLogout> {
        let account = self.owned_account()?;
        let (exchange, state) = account
            .begin_with_terminal(true)
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        Ok(PendingLogout {
            exchange: Arc::new(exchange),
            state,
            options: self.options.clone(),
            transport: self.transport.clone(),
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

    async fn async_short_session(
        options: &NativeOptions,
        transport: &crate::transport::ProtocolTransport,
        short_link: &crate::mmtls::ShortLinkCache,
        host: &str,
    ) -> Result<crate::mmtls::ShortSession> {
        let now_ms = transport.clock.monotonic_ms();
        if let Some(short) = short_link.get_at_ms(host, now_ms) {
            return Ok(short);
        }
        let session = crate::mmtls::AsyncSession::connect(
            host,
            options.timeout,
            options.receive_budget,
            &*transport.streams,
            transport.clock.clone(),
        )
        .await?;
        let short = session.short.clone();
        let _ = session.close().await;
        let issued_ms = transport.clock.monotonic_ms();
        short_link.put_at_ms(host, short.clone(), issued_ms);
        Ok(short)
    }

    #[allow(clippy::too_many_arguments)]
    async fn async_authenticated_rpc_step<F>(
        options: &NativeOptions,
        transport: &crate::transport::ProtocolTransport,
        short_link: &crate::mmtls::ShortLinkCache,
        exchange: &Exchange,
        cancellation: &AtomicBool,
        state: &mut AccountState,
        prior_admission: &mut bool,
        cgi: u16,
        path: &str,
        request: &[u8],
        before_submit: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()>,
    {
        let short =
            Self::async_short_session(options, transport, short_link, &state.long_host).await?;
        let packet = crate::business::AuthenticatedPacket::prepare(state, cgi, request)?;
        let check = (|| {
            ensure!(
                !cancellation.load(Ordering::Acquire),
                "Account request cancelled before submission"
            );
            exchange.check()
        })();
        match check {
            Ok(()) => {}
            Err(error) if !*prior_admission => {
                return Err(error.context(NativeRequestNotSubmitted));
            }
            Err(error) => return Err(error),
        }
        before_submit()?;
        *prior_admission = true;
        let reply = crate::mmtls::short_exchange_async(
            &short,
            &state.short_host,
            path,
            &packet.packet,
            options.timeout,
            &*transport.http,
            &*transport.clock,
        )
        .await
        .inspect_err(|_| {
            // Never retried here: the request may have reached the server.
            // The next request performs a new handshake instead.
            short_link.invalidate();
        })?;
        let (plain, cookie) = packet.decode_verified_transport_reply(&reply)?;
        if !cookie.is_empty() {
            state.cookie = cookie;
        }
        Ok(plain)
    }

    async fn account_single_rpc<T, Build, Decode>(&self, build: Build, decode: Decode) -> Result<T>
    where
        T: crate::transport::MaybeSend + 'static,
        Build: FnOnce(&AccountState) -> Result<(u16, Cow<'static, str>, Vec<u8>)>
            + crate::transport::MaybeSend
            + 'static,
        Decode: FnOnce(&AccountState, Vec<u8>) -> Result<T> + crate::transport::MaybeSend + 'static,
    {
        self.account_single_rpc_guarded(build, decode, || Ok(()))
            .await
    }

    async fn account_single_rpc_guarded<T, Build, Decode, Guard>(
        &self,
        build: Build,
        decode: Decode,
        before_submit: Guard,
    ) -> Result<T>
    where
        T: crate::transport::MaybeSend + 'static,
        Build: FnOnce(&AccountState) -> Result<(u16, Cow<'static, str>, Vec<u8>)>
            + crate::transport::MaybeSend
            + 'static,
        Decode: FnOnce(&AccountState, Vec<u8>) -> Result<T> + crate::transport::MaybeSend + 'static,
        Guard: FnOnce() -> Result<()> + crate::transport::MaybeSend + 'static,
    {
        if let Some(transport) = self.transport.clone() {
            let options = self.options.clone();
            let runner = transport.clone();
            return self
                .run_account_exchange(&runner, move |mut parts| async move {
                    let result = async {
                        let (cgi, path, request) = build(&parts.state)?;
                        let request = Zeroizing::new(request);
                        let reply = Self::async_authenticated_rpc_step(
                            &options,
                            &transport,
                            &parts.owner.short_link,
                            &parts.exchange,
                            &parts.cancellation,
                            &mut parts.state,
                            &mut parts.prior_admission,
                            cgi,
                            path.as_ref(),
                            &request,
                            before_submit,
                        )
                        .await?;
                        decode(&parts.state, reply)
                    }
                    .await;
                    (parts, result)
                })
                .await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.account_request(move |transport, state, _| {
                let (cgi, path, request) = build(state)?;
                let request = Zeroizing::new(request);
                let reply = transport.authenticated_rpc_guarded(
                    state,
                    cgi,
                    path.as_ref(),
                    &request,
                    before_submit,
                )?;
                decode(state, reply)
            })
            .await
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (build, decode, before_submit);
            anyhow::bail!("Native client has no async transport configured")
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn account_request<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>
            + Send
            + 'static,
    {
        let owner = self.owned_account()?;
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let short_link = owner.short_link.clone();
        let options = self.options.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        tokio::task::spawn_blocking(move || {
            Self::execute_account_request(
                options,
                exchange,
                state,
                short_link,
                cancellation,
                operation,
            )
        })
        .await
        .context("Account request worker failed")?
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn account_request_blocking<T, F>(&self, operation: F) -> Result<T>
    where
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>,
    {
        if self.transport.is_some() {
            return Err(anyhow::anyhow!(
                "Blocking account operations cannot bypass a configured async transport"
            )
            .context(NativeRequestNotSubmitted));
        }
        let owner = self.owned_account()?;
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        Self::execute_account_request(
            self.options.clone(),
            exchange,
            state,
            owner.short_link.clone(),
            Arc::new(AtomicBool::new(false)),
            operation,
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn execute_account_request<T, F>(
        options: NativeOptions,
        exchange: Exchange,
        mut state: AccountState,
        short_link: Arc<crate::mmtls::ShortLinkCache>,
        cancellation: Arc<AtomicBool>,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(&NativeClient, &mut AccountState, &dyn Fn() -> Result<()>) -> Result<T>,
    {
        let exchange = Arc::new(exchange);
        // This transport gets a temporary request snapshot, not another owner.
        let mut transport = NativeClient::new(options)?;
        transport.short_link = Some(short_link);
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

fn finish_account_exchange<T>(
    exchange: Exchange,
    state: &AccountState,
    result: Result<T>,
) -> Result<T> {
    // Cookie adoption still follows verified transport when a decoded
    // application result rejects the operation. Preserve original uncertainty.
    let accepted = exchange.accept_cookie(state);
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

#[path = "account_scoped.rs"]
mod scoped;
pub use scoped::AsyncAccountRequest;
pub(crate) use scoped::RequestScope;

#[cfg(not(target_arch = "wasm32"))]
#[path = "account_request.rs"]
mod request;
#[cfg(not(target_arch = "wasm32"))]
pub use request::AccountRequest;

#[path = "account_push.rs"]
mod push;
#[cfg(not(target_arch = "wasm32"))]
pub use push::AccountPushConnection;
pub use push::AsyncPushConnection;

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

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "account_tests.rs"]
mod tests;
