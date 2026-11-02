//! Native personal-call backend. Constructing it performs no login, profile read,
//! network operation or device access. An admitted invitation or reserved owner
//! command starts a worker; current account authority and journaled intents
//! fence every host mutation.

use super::{
    authority::CallAuthorityLease,
    ownership::{AccountAuthority, CallIdentity, Operation},
    service::{
        BackendAudioRequest, BackendOutcome, BackendRequest, BackendSink, CallBackend,
        CallServiceError,
    },
};
use crate::{
    call_types::{CallAudioFormat, CallCapabilities, CallErrorCode},
    runtime::authorization,
    Core,
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    net::{SocketAddr, SocketAddrV4, UdpSocket},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
        Arc, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{oneshot, Notify};
use wechat_calls::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
    playout::FramePacer,
};
use wechat_protocol::{
    calls::{
        app_session::AppAuthority,
        audio_packet::{self, Live96, MediaContext, MediaEpoch, TransmitScope},
        client::{
            CallRpcNotSubmitted, NativePersonalSession, PersonalPhase, PersonalTransport,
            SyncProgress, TaskIds, UnsupportedCallMedia, VoiceParameters,
        },
        host_policy::{
            is_reject_request, is_teardown_request, selected_policy, CallHostPolicy, CallHostRoute,
        },
        relay::{RelayEpoch, RelayProgress, RoomInputs, TwoRttRelay},
        voice_offer::VoiceOffer,
    },
    crypto, protobuf as p, AccountState,
};

#[cfg(not(test))]
use wechat_protocol::NativeOptions;

mod incoming;
mod rtp_order;
mod transport_state;
#[cfg(test)]
mod transport_tests;
use rtp_order::RtpOrder;
use transport_state::CallNetworkState;

type BackendFuture<T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, CallServiceError>> + Send + 'static>>;

/// Transmission is closed by default until key-generation lifetimes are fully
/// validated. The test variant is a single, expiring approved interoperability
/// experiment. It cannot silently authorize later calls or production use.
#[derive(Clone, Default)]
pub enum NativeTransmitScope {
    #[default]
    ReceiveOnly,
    IsolatedTest {
        peer_id: String,
        intent_id: [u8; 32],
        expires_at_unix_ms: u64,
        allow_known_audio_overlap: bool,
    },
}

#[derive(Clone)]
pub struct NativeCallConfig {
    pub account_id: String,
    pub parameters: VoiceParameters,
    pub transmit_scope: NativeTransmitScope,
    pub ring_timeout: Duration,
    pub peer_idle_timeout: Duration,
    pub max_duration: Duration,
}
impl NativeCallConfig {
    pub fn receive_only(account_id: String, parameters: VoiceParameters) -> Self {
        Self {
            account_id,
            parameters,
            transmit_scope: NativeTransmitScope::ReceiveOnly,
            ring_timeout: Duration::from_secs(60),
            peer_idle_timeout: Duration::from_secs(60),
            max_duration: Duration::from_secs(20),
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.account_id.is_empty() && self.account_id.len() <= 200,
            "Invalid native calls account"
        );
        self.parameters.inputs().validate()?;
        ensure!(
            (Duration::from_secs(5)..=Duration::from_secs(60)).contains(&self.ring_timeout)
                && !self.peer_idle_timeout.is_zero()
                && self.peer_idle_timeout <= Duration::from_secs(60)
                && (Duration::from_secs(1)..=Duration::from_secs(1200))
                    .contains(&self.max_duration),
            "Invalid native call time budget"
        );
        if let NativeTransmitScope::IsolatedTest {
            peer_id,
            intent_id,
            expires_at_unix_ms,
            ..
        } = &self.transmit_scope
        {
            ensure!(
                !peer_id.is_empty()
                    && peer_id.len() <= 128
                    && !peer_id.chars().any(char::is_control)
                    && *intent_id != [0; 32]
                    && *expires_at_unix_ms != 0,
                "Missing isolated call test peer or intent"
            );
        }
        Ok(())
    }
    fn tx_ready_at(&self, now: u64) -> bool {
        matches!(self.transmit_scope,NativeTransmitScope::IsolatedTest{expires_at_unix_ms,..} if now<expires_at_unix_ms)
    }
    fn tx_ready(&self) -> bool {
        unix_ms().is_ok_and(|now| self.tx_ready_at(now))
    }
    fn allows_peer(&self, peer: &str) -> bool {
        matches!(&self.transmit_scope,NativeTransmitScope::IsolatedTest{peer_id,..} if peer_id==peer)
    }
}

/// Independent Linux SDK metadata for the selected direct voice subset. It
/// advertises our supported codec/layout, not another client's copied identity.
/// Only returned current capabilities determine whether media may start.
pub fn independent_voice_parameters() -> Result<VoiceParameters> {
    let major = env!("CARGO_PKG_VERSION_MAJOR").parse::<u8>()?;
    let minor = env!("CARGO_PKG_VERSION_MINOR").parse::<u8>()?;
    let patch = env!("CARGO_PKG_VERSION_PATCH").parse::<u8>()?;
    let version = i32::from_be_bytes([major, minor, patch, 0]);
    let offer = VoiceOffer::from_owned_random(version, crypto::random::<32>()?)?;
    let sdk = offer.sdk_info().to_vec();
    let group = p::message(&[
        p::bytes(1, b"wechat"),
        p::uint(2, 0),
        p::bytes(3, b"voip-2p"),
        p::bytes(4, &[]),
        p::uint(5, 1),
        p::bytes(6, &[]),
        p::uint(7, 0),
    ])?;
    let device = p::message(&[
        p::bytes(1, b"agent-wechat"),
        p::bytes(2, b"native"),
        p::bytes(3, env!("CARGO_PKG_VERSION").as_bytes()),
        p::bytes(4, &[]),
        p::bytes(5, b"agent-wechat"),
        p::uint(
            7,
            std::thread::available_parallelism().map_or(1, usize::from) as u64,
        ),
        p::uint(8, 0),
        p::bytes(9, std::env::consts::ARCH.as_bytes()),
    ])?;
    Ok(VoiceParameters {
        sdk,
        group,
        device,
        engine_capability: offer.engine_advertisement().to_vec(),
        channel_capability: offer.advertised_channel_identity().to_vec(),
        network_type: 0,
        sim_type: 0,
        video_length: 0,
        video_ratio: 0,
        nics: vec![],
        cloud_device_id: b"Ldeviceid".to_vec(),
        app_config_version: 1,
    })
}

pub struct NativeCallBackend {
    inner: Arc<Inner>,
}
struct Inner {
    core: Weak<Core>,
    config: NativeCallConfig,
    active: Mutex<Option<Arc<Worker>>>,
    test_spent: AtomicBool,
    #[cfg(test)]
    test_io: Option<Arc<dyn TestCallIo>>,
}
struct Worker {
    identity: CallIdentity,
    lease: CallAuthorityLease,
    stop: AtomicBool,
    media_stop: AtomicBool,
    generation: AtomicU32,
    submission: SendAdmission,
    metrics: MediaMetrics,
    scope_deadline: Instant,
    scope_expires_unix_ms: u64,
    mutations_submitted: AtomicBool,
    incoming: Option<Arc<incoming::Mailbox>>,
    audio: SyncSender<QueuedAudio>,
    done: Mutex<Option<BackendOutcome>>,
    notify: Notify,
}
struct QueuedAudio {
    request: BackendAudioRequest,
    queued: Instant,
}
impl Worker {
    fn scope_current(&self) -> bool {
        scope_current(self.scope_deadline, self.scope_expires_unix_ms)
    }
    fn current(&self) -> bool {
        self.lease.current() && !self.media_stop.load(Ordering::Acquire) && self.scope_current()
    }
    fn finish(&self, outcome: BackendOutcome) {
        if let Ok(mut done) = self.done.lock() {
            *done = Some(outcome);
        }
        self.notify.notify_waiters();
    }
    async fn wait(&self) -> std::result::Result<BackendOutcome, CallServiceError> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = *self
                .done
                .lock()
                .map_err(|_| service_error(CallErrorCode::Closed))?
            {
                return Ok(outcome);
            }
            notified.await;
        }
    }
}
impl NativeCallBackend {
    pub fn new(core: Weak<Core>, config: NativeCallConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                core,
                config,
                active: Mutex::new(None),
                test_spent: AtomicBool::new(false),
                #[cfg(test)]
                test_io: None,
            }),
        })
    }
}
impl CallBackend for NativeCallBackend {
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        if account != self.inner.config.account_id {
            return None;
        }
        let core = self.inner.core.upgrade()?;
        let lease = core.cached_call_authority(account)?;
        lease.current().then(|| lease.authority().clone())
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: account == self.inner.config.account_id
                && self.inner.config.tx_ready()
                && (!self.inner.test_spent.load(Ordering::Acquire)
                    || self.inner.active.lock().ok().is_some_and(|active| {
                        active.as_ref().is_some_and(|worker| {
                            worker.lease.current()
                                && worker.scope_current()
                                && worker.done.lock().ok().is_some_and(|done| done.is_none())
                        })
                    }))
                && self.authority(account).is_some(),
            ..CallCapabilities::default()
        }
    }
    fn incoming_supported(&self, authority: &AccountAuthority, caller: &str) -> bool {
        incoming::supported(&self.inner, authority, caller)
    }
    fn prepare_incoming(
        &self,
        request: crate::calls::notifications::IncomingRequest,
    ) -> BackendFuture<crate::calls::notifications::IncomingPreparation> {
        let inner = self.inner.clone();
        Box::pin(async move { incoming::prepare(inner, request).await })
    }
    fn cancel_incoming(
        &self,
        identity: CallIdentity,
        note: Arc<crate::calls::notifications::IncomingNotification>,
    ) -> BackendFuture<crate::calls::notifications::IncomingCancellation> {
        let inner = self.inner.clone();
        Box::pin(async move { incoming::cancel(inner, identity, note).await })
    }
    fn submit(&self, request: BackendRequest, sink: BackendSink) -> BackendFuture<BackendOutcome> {
        let inner = self.inner.clone();
        Box::pin(async move {
            match request.ticket.operation() {
                Operation::Dial => {}
                Operation::Cancel | Operation::Hangup => {
                    return stop_inner(&inner, request.ticket.identity()).await
                }
                Operation::Answer | Operation::Reject => {
                    return incoming::action(inner, request, sink).await
                }
            }
            if request.call_type != crate::call_types::CallType::Voice
                || !inner.config.tx_ready()
                || !inner.config.allows_peer(&request.peer_id)
            {
                return Ok(BackendOutcome::Ended);
            }
            let Some(core) = inner.core.upgrade() else {
                return Ok(BackendOutcome::Ended);
            };
            let admission_core = Arc::clone(&core);
            let admission_account = inner.config.account_id.clone();
            let Ok(Ok(lease)) = tokio::task::spawn_blocking(move || {
                admission_core.call_authority(&admission_account)
            })
            .await
            else {
                return Ok(BackendOutcome::Ended);
            };
            if !sink.is_current() || lease.authority() != request.ticket.authority() {
                return Ok(BackendOutcome::Ended);
            }
            let now = unix_ms().map_err(|_| service_error(CallErrorCode::Stale))?;
            let scope_expires_unix_ms = match inner.config.transmit_scope {
                NativeTransmitScope::IsolatedTest {
                    expires_at_unix_ms, ..
                } => expires_at_unix_ms,
                NativeTransmitScope::ReceiveOnly => {
                    return Err(service_error(CallErrorCode::Unsupported))
                }
            };
            let remaining = scope_expires_unix_ms
                .checked_sub(now)
                .ok_or_else(|| service_error(CallErrorCode::Stale))?;
            let scope_deadline = Instant::now() + Duration::from_millis(remaining.min(300_000));
            let (audio, rx) = mpsc::sync_channel(32);
            let worker = Arc::new(Worker {
                identity: request.ticket.identity(),
                lease,
                stop: AtomicBool::new(false),
                media_stop: AtomicBool::new(false),
                generation: AtomicU32::new(1),
                submission: SendAdmission::default(),
                metrics: MediaMetrics::default(),
                scope_deadline,
                scope_expires_unix_ms,
                mutations_submitted: AtomicBool::new(false),
                incoming: None,
                audio,
                done: Mutex::new(None),
                notify: Notify::new(),
            });
            {
                let mut active = inner
                    .active
                    .lock()
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
                if active.as_ref().is_some_and(|old| {
                    !matches!(
                        old.done.lock().ok().and_then(|o| *o),
                        Some(BackendOutcome::Ended)
                    )
                }) {
                    return Err(service_error(CallErrorCode::Busy));
                }
                if !consume_test_intent(&inner.test_spent) {
                    return Ok(BackendOutcome::Ended);
                }
                *active = Some(worker.clone());
            }
            let (admitted, rx_admitted) = oneshot::channel();
            let runtime = tokio::runtime::Handle::current();
            let actor_inner = inner.clone();
            let actor_worker = worker.clone();
            if thread::Builder::new()
                .name("wechat-direct-call".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_control(
                            &actor_inner,
                            &actor_worker,
                            request,
                            sink.clone(),
                            rx,
                            admitted,
                            &runtime,
                        )
                    }))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Call worker panicked")));
                    let terminal =
                        result.is_ok() || !actor_worker.mutations_submitted.load(Ordering::Acquire);
                    if result.is_err() {
                        if let Some(core) = actor_inner.core.upgrade() {
                            let _ = core.store.finish_call(
                                &actor_inner.config.account_id,
                                actor_worker.identity.call_id(),
                                terminal,
                            );
                        }
                    }
                    let outcome = if terminal {
                        BackendOutcome::Ended
                    } else {
                        BackendOutcome::Uncertain
                    };
                    actor_worker.media_stop.store(true, Ordering::Release);
                    actor_worker.finish(outcome);
                    if terminal {
                        let _ = runtime.block_on(sink.ended());
                    } else {
                        let _ = runtime.block_on(sink.uncertain());
                    }
                })
                .is_err()
            {
                worker.finish(BackendOutcome::Ended);
                return Ok(BackendOutcome::Ended);
            }
            match rx_admitted.await {
                Ok(outcome) => outcome,
                Err(_) => worker.wait().await,
            }
        })
    }
    fn send_audio(&self, request: BackendAudioRequest) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &request.identity)?;
            if !worker.current()
                || !request.is_current()
                || request.generation != worker.generation.load(Ordering::Acquire)
            {
                return Err(service_error(CallErrorCode::Stale));
            }
            if request.format != CallAudioFormat::default() || request.samples.len() != 320 {
                return Err(service_error(CallErrorCode::InvalidRequest));
            }
            worker
                .audio
                .try_send(QueuedAudio {
                    request,
                    queued: Instant::now(),
                })
                .map_err(|_| service_error(CallErrorCode::Capacity))?;
            worker
                .metrics
                .tx_enqueued_frames
                .fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }
    fn clear_audio(&self, identity: CallIdentity, generation: u32) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &identity)?;
            if generation == 0 {
                return Err(service_error(CallErrorCode::Stale));
            }
            worker
                .submission
                .advance(&worker.generation, generation)
                .map_err(|_| service_error(CallErrorCode::Closed))
        })
    }
    fn stop(&self, identity: CallIdentity) -> BackendFuture<BackendOutcome> {
        let inner = self.inner.clone();
        Box::pin(async move { stop_inner(&inner, identity).await })
    }
}
fn service_error(code: CallErrorCode) -> CallServiceError {
    CallServiceError(code)
}
fn active(
    inner: &Inner,
    identity: &CallIdentity,
) -> std::result::Result<Arc<Worker>, CallServiceError> {
    inner
        .active
        .lock()
        .map_err(|_| service_error(CallErrorCode::Closed))?
        .as_ref()
        .filter(|w| &w.identity == identity)
        .cloned()
        .ok_or_else(|| service_error(CallErrorCode::Stale))
}
async fn stop_inner(
    inner: &Inner,
    identity: CallIdentity,
) -> std::result::Result<BackendOutcome, CallServiceError> {
    let worker = active(inner, &identity)?;
    worker.stop.store(true, Ordering::Release);
    worker.media_stop.store(true, Ordering::Release);
    worker.wait().await
}

enum ControlAdmission {
    Owner(BackendSink),
    Incoming(crate::calls::notifications::IncomingGuard),
    Terminal(BackendSink),
}
impl ControlAdmission {
    fn nonterminal_current(&self) -> bool {
        match self {
            Self::Owner(sink) => sink.is_current(),
            Self::Incoming(guard) => guard.is_current(),
            Self::Terminal(_) => false,
        }
    }
    fn terminal_current(&self) -> bool {
        matches!(self, Self::Terminal(sink) if sink.is_terminal_current())
    }
}
#[cfg(test)]
trait TestCallIo: Send + Sync {
    fn call(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
        handoff: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>>;
}
struct FencedTransport {
    core: Weak<Core>,
    account: String,
    lease: CallAuthorityLease,
    call_id: String,
    ordinal: AtomicU64,
    scope_deadline: Instant,
    scope_expires_unix_ms: u64,
    worker: Arc<Worker>,
    admission: ControlAdmission,
    runtime: tokio::runtime::Handle,
    #[cfg(test)]
    test_io: Option<Arc<dyn TestCallIo>>,
}
impl FencedTransport {
    fn admits(&self, teardown: bool, rejecting: bool) -> bool {
        if teardown {
            return true;
        }
        if self
            .worker
            .incoming
            .as_ref()
            .is_some_and(|mailbox| mailbox.cancellation_pending())
        {
            return false;
        }
        if rejecting {
            return self.admission.terminal_current();
        }
        self.admission.nonterminal_current()
            && scope_current(self.scope_deadline, self.scope_expires_unix_ms)
    }
}
impl FencedTransport {
    /// Operational evidence is independent of account-state adoption. In
    /// particular a valid reply survives a later writer/authority failure.
    fn retain_host_result(
        &self,
        core: &Core,
        action: &str,
        route: CallHostRoute,
        result: &Result<Vec<u8>>,
        handed_off: bool,
    ) -> Result<()> {
        match result {
            Ok(reply) => {
                let label = format!("call-reply:{}:{action}", self.call_id);
                let encrypted = core.vault.seal(&self.account, &label, reply)?;
                core.store.finish_call_action(
                    &self.account,
                    &self.call_id,
                    action,
                    "received",
                    Some(&encrypted),
                )?;
                Ok(())
            }
            Err(error) => {
                let local_stop = error.is::<CallRpcNotSubmitted>() && !handed_off;
                if local_stop {
                    core.store.put_runtime(&self.account,&format!("call-host-diagnostic:{}:{action}",self.call_id),&serde_json::to_vec(&serde_json::json!({"cgi":route.command(),"stage":"presubmit","code":"admission_revoked","mayHaveSubmitted":false}))?)?;
                }
                if let Some(error) =
                    error.downcast_ref::<wechat_protocol::calls::host_transport::CallHostRpcError>()
                {
                    let diagnostic = serde_json::to_vec(
                        &serde_json::json!({"cgi":route.command(),"stage":error.stage.as_str(),"code":error.code,"value":error.value,"mayHaveSubmitted":handed_off || error.may_have_submitted()}),
                    )?;
                    core.store.put_runtime(
                        &self.account,
                        &format!("call-host-diagnostic:{}:{action}", self.call_id),
                        &diagnostic,
                    )?;
                }
                if local_stop {
                    core.store.finish_call_action_not_submitted(
                        &self.account,
                        &self.call_id,
                        action,
                    )?;
                    return Ok(());
                }
                core.store.finish_call_action(
                    &self.account,
                    &self.call_id,
                    action,
                    "needs_review",
                    None,
                )?;
                Ok(())
            }
        }
    }
}
impl PersonalTransport for FencedTransport {
    fn call_rpc(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
    ) -> Result<Vec<u8>> {
        let core = self.core.upgrade().context("Call core closed")?;
        let guard = self.runtime.block_on(core.lock_account(&self.account))?;
        if !self.lease.current() || core.stopping.load(Ordering::Acquire) {
            return Err(CallRpcNotSubmitted.into());
        }
        let teardown = is_teardown_request(route, request)?;
        let rejecting = is_reject_request(route, request)?;
        if !self.admits(teardown, rejecting) {
            return Err(CallRpcNotSubmitted.into());
        }
        let latest = core.account_state(&self.account)?;
        ensure!(
            authorization(&latest)? == authorization(state)?,
            "Call accepted credentials changed"
        );
        let snapshot = CallNetworkState::capture(&core, &self.account, &self.lease, &latest)?;
        *state = latest.clone();
        let mut exchanged = latest;
        ensure!(
            selected_policy(&exchanged.auth_snapshot, route.command())? == CallHostPolicy::Ordinary,
            "Selected call host route unsupported"
        );
        #[cfg(not(test))]
        let native = core.upstream.native(NativeOptions {
            timeout: Duration::from_secs(12),
            ..NativeOptions::default()
        })?;
        if !self.admits(teardown, rejecting) {
            return Err(CallRpcNotSubmitted.into());
        }
        let ordinal = self.ordinal.fetch_add(1, Ordering::AcqRel);
        ensure!(ordinal < 4096, "Call action budget exhausted");
        let action = ordinal.to_string();
        let digest = format!("{:x}", Sha256::digest(request));
        let sealed_request = core.vault.seal(
            &self.account,
            &format!("call-request:{}:{action}", self.call_id),
            request,
        )?;
        ensure!(
            core.store.begin_call_action(
                &self.account,
                &self.call_id,
                &action,
                route.command(),
                &digest,
                &sealed_request
            )?,
            "Call action already spent"
        );
        // The account writer is not a network-lifetime lock. A fresh owner or
        // account change can now be accepted while this one exchange waits.
        drop(guard);
        let handed_off = AtomicBool::new(false);
        let handoff = || {
            if !self.lease.current() || core.stopping.load(Ordering::Acquire) {
                return Err(CallRpcNotSubmitted.into());
            }
            if !self.admits(teardown, rejecting) {
                return Err(CallRpcNotSubmitted.into());
            }
            handed_off.store(true, Ordering::Release);
            if !matches!(route, CallHostRoute::AppAuth | CallHostRoute::LiveSync) {
                self.worker
                    .mutations_submitted
                    .store(true, Ordering::Release);
            }
            Ok(())
        };
        // A unit-test build can never fall through into network transport.
        #[cfg(test)]
        let result = {
            let mut handoff = handoff;
            self.test_io
                .as_ref()
                .context("Missing synthetic call host")
                .and_then(|io| io.call(&mut exchanged, route, request, &mut handoff))
        };
        #[cfg(not(test))]
        let result = native.experimental_call_rpc_guarded(&mut exchanged, route, request, handoff);
        // Retain the response before trying to reacquire: a poisoned/closed
        // writer must not discard an authenticated reply. These are operational
        // sealed receipts, never account/profile state or mutation permission.
        let handed_off = handed_off.load(Ordering::Acquire);
        let retained = self.retain_host_result(&core, &action, route, &result, handed_off);
        let reacquired = self.runtime.block_on(core.lock_account(&self.account));
        retained?;
        let _guard = reacquired?;
        match result {
            Ok(reply) => {
                *state = snapshot.accept(&core, &self.account, &self.lease, &exchanged)?;
                Ok(reply)
            }
            Err(error) if error.is::<CallRpcNotSubmitted>() && !handed_off => Err(error),
            Err(_) => bail!("Call host outcome unknown"),
        }
    }
}

struct Tasks {
    host: u32,
    sdk: u32,
}
impl Tasks {
    fn new() -> Result<Self> {
        let seed = u32::from_le_bytes(crypto::random::<4>()?).max(1);
        ensure!(seed < u32::MAX - 4096, "Call task seed exhausted");
        Ok(Self { host: seed, sdk: 0 })
    }
    fn next(&mut self) -> Result<TaskIds> {
        self.host = self
            .host
            .checked_add(1)
            .context("Call host task exhausted")?;
        self.sdk = self.sdk.checked_add(1).context("Call SDK task exhausted")?;
        Ok(TaskIds {
            host: self.host,
            sdk: self.sdk,
        })
    }
}

fn begin_operational_call(
    inner: &Inner,
    worker: &Worker,
    core: &Core,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let account = &inner.config.account_id;
    let call_id = worker.identity.call_id();
    let _guard = runtime.block_on(core.lock_account(account))?;
    ensure!(worker.lease.current(), "Call authority revoked");
    ensure!(
        core.store
            .begin_call(account, call_id, &worker.lease.authority().generation)?,
        "Previous call requires reconciliation"
    );
    if let NativeTransmitScope::IsolatedTest { intent_id, .. } = &inner.config.transmit_scope {
        let marker = format!("call-test-intent:{:x}", Sha256::digest(intent_id));
        ensure!(
            core.store.runtime(account, &marker)?.is_none(),
            "Isolated call test intent already spent"
        );
        core.store
            .put_runtime(account, &marker, call_id.as_bytes())?;
    }
    Ok(())
}

fn run_control(
    inner: &Arc<Inner>,
    worker: &Arc<Worker>,
    request: BackendRequest,
    sink: BackendSink,
    audio: Receiver<QueuedAudio>,
    admitted: oneshot::Sender<std::result::Result<BackendOutcome, CallServiceError>>,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let core = inner.core.upgrade().context("Call core closed")?;
    let account = &inner.config.account_id;
    let call_id = worker.identity.call_id();
    begin_operational_call(inner, worker, &core, runtime)?;
    let transport = FencedTransport {
        core: inner.core.clone(),
        account: account.clone(),
        lease: worker.lease.clone(),
        call_id: call_id.into(),
        ordinal: AtomicU64::new(1),
        scope_deadline: worker.scope_deadline,
        scope_expires_unix_ms: worker.scope_expires_unix_ms,
        worker: worker.clone(),
        admission: ControlAdmission::Owner(sink.clone()),
        runtime: runtime.clone(),
        #[cfg(test)]
        test_io: inner.test_io.clone(),
    };
    let mut state = core.account_state(account)?;
    let auth = AppAuthority {
        account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
        logout_epoch: 0,
        credential_digest: authorization(&state)?,
    };
    let mut tasks = Tasks::new()?;
    // Retain this fresh identity until the complete call actor (including its
    // media worker) has ended. A startup template is never reused on the wire.
    let voice_offer =
        VoiceOffer::from_owned_random(state.device.client_version, crypto::random::<32>()?)?;
    let mut parameters = inner.config.parameters.clone();
    voice_offer.apply_to(&mut parameters);
    let mut session = NativePersonalSession::authenticate(
        &transport,
        &mut state,
        auth,
        tasks.host,
        crypto::random::<32>()?,
        parameters,
    )?;
    if worker.stop.load(Ordering::Acquire) || !worker.scope_current() {
        core.store.finish_call(account, call_id, true)?;
        let _ = admitted.send(Ok(BackendOutcome::Ended));
        return Ok(());
    }
    if let Err(error) = session.invite(
        &transport,
        &mut state,
        auth,
        &request.peer_id,
        unix_ms()?,
        tasks.next()?,
    ) {
        if error.is::<UnsupportedCallMedia>() {
            worker.stop.store(true, Ordering::Release);
            core.store.put_runtime(
                account,
                &format!("call-media-error:{call_id}"),
                b"unsupported_media",
            )?;
        } else {
            return Err(error);
        }
    }
    if matches!(
        session.phase(),
        PersonalPhase::Finished | PersonalPhase::TerminalPending
    ) {
        worker.stop.store(true, Ordering::Release);
    }
    let _ = admitted.send(Ok(BackendOutcome::Admitted));
    let ringing_end = Instant::now() + inner.config.ring_timeout;
    while !worker.stop.load(Ordering::Acquire) {
        ensure!(worker.lease.current(), "Call authority revoked");
        match session.media_ready(&state, auth) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) if error.is::<UnsupportedCallMedia>() => {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= ringing_end || !worker.scope_current() {
            worker.stop.store(true, Ordering::Release);
            break;
        }
        match session.poll(&transport, &mut state, auth, tasks.next()?) {
            Ok(SyncProgress::Terminal(_)) => {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Ok(_) => {}
            Err(error)
                if error.is::<CallRpcNotSubmitted>() || error.is::<UnsupportedCallMedia>() =>
            {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    if worker.stop.load(Ordering::Acquire) {
        let _ = runtime.block_on(sink.ending());
        let result = if session.phase() == PersonalPhase::Finished {
            Ok(())
        } else if session.phase() == PersonalPhase::TerminalPending {
            session.finish_remote(&transport, &mut state, auth, tasks.next()?, false)
        } else if session.phase() == PersonalPhase::Ringing {
            session.cancel(&transport, &mut state, auth, tasks.next()?)
        } else if session.phase() == PersonalPhase::Joined {
            session.exit(&transport, &mut state, auth, tasks.next()?, false)
        } else {
            bail!("Call termination state unknown")
        };
        core.store.finish_call(account, call_id, result.is_ok())?;
        return result;
    }
    ensure!(
        session.phase() == PersonalPhase::Joined,
        "Peer call admission incomplete"
    );
    let negotiated = session.take_media(&state, auth, 0, 0)?;
    let cfg = inner.config.clone();
    let media_worker = worker.clone();
    let media_sink = sink.clone();
    let media_runtime = runtime.clone();
    let media_thread = thread::Builder::new()
        .name("wechat-call-media".into())
        .spawn(move || {
            let result = run_media(
                &cfg,
                &media_worker,
                negotiated,
                audio,
                &media_sink,
                &media_runtime,
            );
            media_worker.media_stop.store(true, Ordering::Release);
            result
        })?;
    let deadline = Instant::now() + inner.config.max_duration;
    let control_result = (|| -> Result<()> {
        while !worker.stop.load(Ordering::Acquire)
            && !worker.media_stop.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            ensure!(worker.lease.current(), "Call authority revoked");
            if !worker.scope_current() {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            match session.poll(&transport, &mut state, auth, tasks.next()?) {
                Ok(SyncProgress::MediaReady) => {}
                Ok(SyncProgress::Terminal(_)) => {
                    worker.media_stop.store(true, Ordering::Release);
                    break;
                }
                Ok(SyncProgress::Waiting) => bail!("Peer left current voice state"),
                Err(error)
                    if error.is::<CallRpcNotSubmitted>() || error.is::<UnsupportedCallMedia>() =>
                {
                    worker.stop.store(true, Ordering::Release);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })();
    if control_result.is_ok() {
        let _ = runtime.block_on(sink.ending());
    }
    worker.media_stop.store(true, Ordering::Release);
    let media_result = media_thread
        .join()
        .map_err(|_| anyhow::anyhow!("Call media worker failed"))?;
    control_result?;
    // A local media failure still permits one separately journaled Exit from a
    // known joined room. Confirmed server termination and successful audio are
    // distinct; keep the local failure as an operational diagnostic.
    if let Err(error) = &media_result {
        core.store.put_runtime(
            account,
            &format!("call-media-error:{call_id}"),
            b"media_failed",
        )?;
        // Persist only fixed categories and counters, never packet material or
        // dynamic transport errors, so a live failure can be diagnosed safely.
        let message = error.to_string().to_ascii_lowercase();
        let code = if message.contains("peer activity timed out") {
            "peer_inactive"
        } else if message.contains("nonce") {
            "nonce_lifetime"
        } else if message.contains("generation") {
            "generation_changed"
        } else if message.contains("authenticated call control") {
            "unsupported_control"
        } else if message.contains("opus") {
            "opus_codec"
        } else if message.contains("receiver unavailable") {
            "audio_receiver_closed"
        } else {
            "other_media_error"
        };
        let _ = core.store.put_runtime(
            account,
            &format!("call-media-diagnostic:{call_id}"),
            &serde_json::to_vec(&serde_json::json!({
                "code": code,
                "txDatagrams": worker.metrics.tx_submitted_datagrams.load(Ordering::Relaxed),
                "rxFrames": worker.metrics.rx_delivered_frames.load(Ordering::Relaxed),
                "txUnderruns": worker.metrics.tx_input_underruns.load(Ordering::Relaxed)
            }))?,
        );
    }
    ensure!(worker.lease.current(), "Call authority revoked before Exit");
    let ended = match session.phase() {
        PersonalPhase::Finished => Ok(()),
        PersonalPhase::TerminalPending => {
            session.finish_remote(&transport, &mut state, auth, tasks.next()?, false)
        }
        PersonalPhase::Joined => session.exit(&transport, &mut state, auth, tasks.next()?, false),
        _ => bail!("Call termination state unknown"),
    };
    core.store.finish_call(account, call_id, ended.is_ok())?;
    ended
}

fn run_media(
    config: &NativeCallConfig,
    worker: &Arc<Worker>,
    media: wechat_protocol::calls::client::NegotiatedMedia,
    audio: Receiver<QueuedAudio>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let started = Instant::now();
    ensure!(
        matches!(media.frames_per_packet, 1 | 2),
        "Unsupported call packet grouping"
    );
    let clock = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let epoch = MediaEpoch {
        account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
        logout_epoch: 0,
        call_generation: nonzero64(worker.identity.call_id().as_bytes()),
        key_generation: 1,
    };
    let relay_epoch = RelayEpoch {
        account_generation: epoch.account_generation,
        logout_epoch: 0,
        call_generation: epoch.call_generation,
        key_generation: epoch.key_generation,
        socket_generation: 1,
        attempt_id: 1,
    };
    let selected = media.relay;
    let endpoint = selected.endpoint;
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
    // Physical sends are nonblocking while account/clear submission gates are
    // held. Waiting for input happens only outside those gates.
    socket.set_nonblocking(true)?;
    let room = RoomInputs::initial(&selected, 0, clock() as u32, clock() as u32);
    let mut checkin = TwoRttRelay::new(room, selected.room_key, endpoint, relay_epoch, 15_000)?;
    let random = crypto::random::<10>()?;
    let packet = checkin.prepare_initial(
        relay_epoch,
        clock(),
        &random[..checkin.random_bytes_needed()?],
    )?;
    send_datagram(&socket, endpoint, &packet, worker, &[])?;
    // One extra byte distinguishes a maximum-size datagram from truncation.
    let mut buffer = [0u8; 8198];
    while !checkin.is_ready() {
        ensure!(
            worker.current() && !worker.stop.load(Ordering::Acquire),
            "Call stopped during relay setup"
        );
        ensure!(clock() < 15_000, "Call relay deadline exceeded");
        match socket.recv_from(&mut buffer) {
            Ok((n, SocketAddr::V4(source)))
                if source == endpoint && n <= 8197 && buffer[..n].first() == Some(&0xd5) =>
            {
                match checkin.receive(&buffer[..n], source, relay_epoch, clock())? {
                    RelayProgress::FirstCheckin => {
                        let header = clock() as u32;
                        let body = clock() as u32;
                        let random = crypto::random::<10>()?;
                        let n = checkin.continuation_random_bytes_needed(header, body)?;
                        let continuation = checkin.prepare_continuation(
                            relay_epoch,
                            clock(),
                            header,
                            body,
                            &random[..n],
                        )?;
                        send_datagram(&socket, endpoint, &continuation, worker, &[])?;
                    }
                    RelayProgress::Ready
                    | RelayProgress::Opaque
                    | RelayProgress::DuplicateCheckin => {}
                }
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
    }
    let mut channel = Live96::new(
        *selected.media_key,
        selected.room_key,
        MediaContext {
            room_id: selected.room_id,
            own_member: selected.own_member,
            peer_member: media.peer_member,
            connection_id: selected.connection_id,
        },
        epoch,
    )?;
    match &config.transmit_scope {
        NativeTransmitScope::ReceiveOnly => bail!("Call media transmission remains gated"),
        NativeTransmitScope::IsolatedTest {
            intent_id,
            expires_at_unix_ms,
            allow_known_audio_overlap,
            ..
        } => {
            let remaining = expires_at_unix_ms
                .checked_sub(unix_ms()?)
                .context("Isolated call test expired")?;
            channel.authorize_transmit(
                epoch,
                TransmitScope::IsolatedCompatibilityTest {
                    intent_id: *intent_id,
                    deadline_ms: clock().saturating_add(remaining),
                    allow_known_audio_overlap: *allow_known_audio_overlap,
                },
            )?;
        }
    }
    let format = AudioFormat::new(16000, 1, 20)?;
    let mut decoder = OpusDecoder::new(format)?;
    let mut encoder = OpusEncoder::new(format, 20_000)?;
    ensure!(
        worker.current() && !worker.stop.load(Ordering::Acquire),
        "Call stopped before transport admission"
    );
    // Current joined peer AV + selected codec were admitted by take_media;
    // Ready establishes this transport. Peer speech is not a prerequisite for
    // transmitting; actual decoded/heard audio remains a separate observation.
    runtime
        .block_on(sink.connected())
        .map_err(|_| anyhow::anyhow!("Call owner unavailable"))?;
    let mut encoder_generation = worker.generation.load(Ordering::Acquire);
    let mut pacer = FramePacer::new(format, started.elapsed());
    let mut pending: Option<(Vec<u8>, QueuedAudio)> = None;
    let mut rtp_sequence = 0u32;
    let mut rtp_timestamp = 0u64;
    let mut rx_order = RtpOrder::new();
    let mut last_peer_activity = Instant::now();
    let media_result = (|| -> Result<()> {
        while worker.current() && !worker.stop.load(Ordering::Acquire) {
            ensure!(
                last_peer_activity.elapsed() < config.peer_idle_timeout,
                "Call peer activity timed out"
            );
            match socket.recv_from(&mut buffer) {
                Ok((n, SocketAddr::V4(source))) if source == endpoint && n <= 8197 => {
                    if buffer[..n].first() == Some(&0x96) {
                        let opened = match channel.open(epoch, &buffer[..n]) {
                            Ok(v) => v,
                            Err(e) => {
                                if e.to_string().contains("nonce")
                                    || e.to_string().contains("generation")
                                {
                                    return Err(e);
                                }
                                None
                            }
                        };
                        if let Some(opened) = opened {
                            // Only a fresh, authenticated packet from the selected
                            // peer renews liveness. Relay traffic and bad/replayed
                            // packets do not establish that the peer is still here.
                            last_peer_activity = Instant::now();
                            if opened.packed_kind == 0x70 {
                                if let Some(reply) =
                                    channel.reply_to_probe(epoch, clock(), &opened)?
                                {
                                    send_datagram(&socket, endpoint, &reply, worker, &[])?;
                                }
                            } else if let Ok(primary) =
                                audio_packet::parse_ordinary95(opened.payload())
                            {
                                let admission = match rx_order.admit(
                                    primary.last_sequence,
                                    primary.last_timestamp,
                                    primary.primaries.len(),
                                ) {
                                    Ok(admission) => Some(admission),
                                    Err(_) => {
                                        worker
                                            .metrics
                                            .rx_invalid_order
                                            .fetch_add(1, Ordering::Relaxed);
                                        None
                                    }
                                };
                                if let Some(admission) = admission {
                                    worker
                                        .metrics
                                        .rx_skipped_frames
                                        .fetch_add(admission.skip as u64, Ordering::Relaxed);
                                    worker
                                        .metrics
                                        .rx_gaps
                                        .fetch_add(u64::from(admission.gap), Ordering::Relaxed);
                                    for packet in primary.primaries.into_iter().skip(admission.skip)
                                    {
                                        ensure!(
                                            worker.current(),
                                            "Call authority revoked before decode"
                                        );
                                        let pcm = decoder.decode(packet)?;
                                        ensure!(
                                            worker.current(),
                                            "Call authority revoked before audio delivery"
                                        );
                                        runtime.block_on(sink.audio(&pcm)).map_err(|_| {
                                            anyhow::anyhow!("Call audio receiver unavailable")
                                        })?;
                                        worker
                                            .metrics
                                            .rx_delivered_frames
                                            .fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
            synchronize_encoder_generation(
                &mut encoder,
                &mut encoder_generation,
                &mut pending,
                worker.generation.load(Ordering::Acquire),
            )?;
            if pending
                .as_ref()
                .is_some_and(|(_, item)| !audio_current(worker, item))
            {
                pending = None;
                worker
                    .metrics
                    .tx_stale_frames
                    .fetch_add(1, Ordering::Relaxed);
                encoder.reset()?;
            }
            if !pacer.poll(started.elapsed())? {
                continue;
            }
            let item = loop {
                match audio.try_recv() {
                    Ok(item) if audio_current(worker, &item) => break Some(item),
                    Ok(_) => {
                        worker
                            .metrics
                            .tx_stale_frames
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Err(TryRecvError::Empty) => break None,
                    Err(TryRecvError::Disconnected) => break None,
                }
            };
            if let Some(item) = item {
                ensure!(
                    rtp_sequence <= u32::from(u16::MAX) && rtp_timestamp <= u64::from(u32::MAX),
                    "Call RTP sequence exhausted"
                );
                synchronize_encoder_generation(
                    &mut encoder,
                    &mut encoder_generation,
                    &mut pending,
                    item.request.generation,
                )?;
                let encoded = encoder.encode(&item.request.samples)?;
                let sequence = rtp_sequence as u16;
                let timestamp = rtp_timestamp as u32;
                rtp_sequence += 1;
                rtp_timestamp += 320;
                if !audio_current(worker, &item) {
                    worker
                        .metrics
                        .tx_stale_frames
                        .fetch_add(1 + u64::from(pending.is_some()), Ordering::Relaxed);
                    pending = None;
                    encoder.reset()?;
                    continue;
                }
                if media.frames_per_packet == 1 {
                    let conf = audio_packet::ordinary95_single(0, sequence, timestamp, &encoded)?;
                    let datagram = channel.seal_audio(epoch, clock(), &conf)?;
                    if !send_datagram(&socket, endpoint, &datagram, worker, &[&item])? {
                        encoder.reset()?;
                    }
                } else if let Some((first, first_item)) = pending.take() {
                    if audio_current(worker, &first_item) && audio_current(worker, &item) {
                        let conf = audio_packet::ordinary95_pair(
                            0, sequence, timestamp, &first, &encoded,
                        )?;
                        let datagram = channel.seal_audio(epoch, clock(), &conf)?;
                        if !send_datagram(
                            &socket,
                            endpoint,
                            &datagram,
                            worker,
                            &[&first_item, &item],
                        )? {
                            encoder.reset()?;
                        }
                    } else {
                        worker
                            .metrics
                            .tx_stale_frames
                            .fetch_add(2, Ordering::Relaxed);
                        encoder.reset()?;
                    }
                } else {
                    pending = Some((encoded, item));
                }
            } else {
                worker
                    .metrics
                    .tx_input_underruns
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    })();
    channel.close();
    worker.media_stop.store(true, Ordering::Release);
    media_result
}
fn audio_current(worker: &Worker, item: &QueuedAudio) -> bool {
    worker.current()
        && item.request.is_current()
        && item.request.identity == worker.identity
        && item.request.generation == worker.generation.load(Ordering::Acquire)
        && item.queued.elapsed() <= Duration::from_millis(500)
}
#[derive(Default)]
struct MediaMetrics {
    rx_delivered_frames: AtomicU64,
    rx_skipped_frames: AtomicU64,
    rx_gaps: AtomicU64,
    rx_invalid_order: AtomicU64,
    tx_enqueued_frames: AtomicU64,
    tx_stale_frames: AtomicU64,
    tx_submitted_datagrams: AtomicU64,
    tx_backpressure: AtomicU64,
    tx_submission_dropped_frames: AtomicU64,
    tx_input_underruns: AtomicU64,
}

/// Clear's acknowledgment and physical UDP handoff share this gate. A business
/// HTTP handoff uses a separate account action guard and is never retracted here.
#[derive(Default)]
struct SendAdmission {
    gate: Mutex<()>,
}
impl SendAdmission {
    fn advance(&self, generation: &AtomicU32, next: u32) -> Result<()> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("Call submission gate unavailable"))?;
        generation.fetch_max(next, Ordering::AcqRel);
        Ok(())
    }
    fn submit<T>(
        &self,
        allowed: impl FnOnce() -> Result<bool>,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("Call submission gate unavailable"))?;
        if !allowed()? {
            return Ok(None);
        }
        write().map(Some)
    }
}
fn synchronize_encoder_generation<T>(
    encoder: &mut impl AudioEncoder,
    current: &mut u32,
    pending: &mut Option<T>,
    next: u32,
) -> Result<()> {
    if *current != next {
        *pending = None;
        encoder.reset()?;
        *current = next;
    }
    Ok(())
}
fn send_datagram(
    socket: &UdpSocket,
    endpoint: SocketAddrV4,
    packet: &[u8],
    worker: &Worker,
    audio: &[&QueuedAudio],
) -> Result<bool> {
    // No Core/service locks, filesystem work, codec work or waits inside this
    // callback. Revocation waits for an already-entered nonblocking handoff.
    let submitted = worker
        .submission
        .submit(
            || {
                ensure!(
                    worker.current() && !worker.stop.load(Ordering::Acquire),
                    "Call authority revoked before send"
                );
                Ok(audio.iter().all(|item| audio_current(worker, item)))
            },
            || {
                worker
                    .lease
                    .with_current(|| {
                        ensure!(
                            worker.current() && !worker.stop.load(Ordering::Acquire),
                            "Call authority revoked before send"
                        );
                        if !audio.iter().all(|item| audio_current(worker, item)) {
                            return Ok(false);
                        }
                        let written = match socket.send_to(packet, endpoint) {
                            Ok(written) => written,
                            Err(error)
                                if !audio.is_empty()
                                    && error.kind() == std::io::ErrorKind::WouldBlock =>
                            {
                                worker
                                    .metrics
                                    .tx_backpressure
                                    .fetch_add(1, Ordering::Relaxed);
                                return Ok(false);
                            }
                            Err(error) => return Err(error.into()),
                        };
                        ensure!(
                            written == packet.len(),
                            "Call UDP submission outcome unknown"
                        );
                        if !audio.is_empty() {
                            worker
                                .metrics
                                .tx_submitted_datagrams
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(true)
                    })
                    .ok_or_else(|| anyhow::anyhow!("Call authority revoked before send"))?
            },
        )
        .map(|submitted| submitted.unwrap_or(false))?;
    if !submitted && !audio.is_empty() {
        worker
            .metrics
            .tx_submission_dropped_frames
            .fetch_add(audio.len() as u64, Ordering::Relaxed);
    }
    Ok(submitted)
}
fn consume_test_intent(spent: &AtomicBool) -> bool {
    spent
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}
fn scope_current(deadline: Instant, expires_at_unix_ms: u64) -> bool {
    Instant::now() < deadline && unix_ms().is_ok_and(|now| now < expires_at_unix_ms)
}
fn nonzero64(value: &[u8]) -> u64 {
    let digest = Sha256::digest(value);
    u64::from_le_bytes(digest[..8].try_into().expect("digest width")).max(1)
}
fn unix_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_generation_discards_pending_and_resets_encoder_lookahead() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let mut encoder = OpusEncoder::new(format, 20_000).unwrap();
        let mut impulse = vec![0; 320];
        impulse[..160].fill(12_000);
        let old = encoder.encode(&impulse).unwrap();
        let mut pending = Some(old);
        let mut generation = 1;
        synchronize_encoder_generation(&mut encoder, &mut generation, &mut pending, 2).unwrap();
        assert_eq!(generation, 2);
        assert!(pending.is_none());
        let reset_zeros = encoder.encode(&[0; 320]).unwrap();
        let mut fresh = OpusEncoder::new(format, 20_000).unwrap();
        assert_eq!(reset_zeros, fresh.encode(&[0; 320]).unwrap());
        // Same-generation encoding keeps its ordinary stream state.
        pending = Some(vec![1]);
        synchronize_encoder_generation(&mut encoder, &mut generation, &mut pending, 2).unwrap();
        assert!(pending.is_some());
    }
    #[test]
    fn clear_ack_waits_for_entered_handoff_and_later_old_frames_never_submit() {
        let admission = Arc::new(SendAdmission::default());
        let generation = Arc::new(AtomicU32::new(1));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer_admission = admission.clone();
        let writer_generation = generation.clone();
        let writer = thread::spawn(move || {
            writer_admission
                .submit(
                    || Ok(writer_generation.load(Ordering::Acquire) == 1),
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    },
                )
                .unwrap()
        });
        entered_rx.recv().unwrap();
        let (clear_started_tx, clear_started_rx) = std::sync::mpsc::channel();
        let (acked_tx, acked_rx) = std::sync::mpsc::channel();
        let clear_admission = admission.clone();
        let clear_generation = generation.clone();
        let clear = thread::spawn(move || {
            clear_started_tx.send(()).unwrap();
            clear_admission.advance(&clear_generation, 2).unwrap();
            acked_tx.send(()).unwrap();
        });
        clear_started_rx.recv().unwrap();
        assert!(matches!(
            acked_rx.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(()).unwrap();
        assert!(writer.join().unwrap().is_some());
        acked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        clear.join().unwrap();
        assert!(admission
            .submit(
                || Ok(generation.load(Ordering::Acquire) == 1),
                || -> Result<()> { panic!("stale audio submitted") }
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_scope_is_peer_bound_expiring_and_one_shot() {
        let mut config =
            NativeCallConfig::receive_only("test".into(), independent_voice_parameters().unwrap());
        assert!(!config.tx_ready_at(0));
        assert!(!config.allows_peer("only-peer"));
        config.transmit_scope = NativeTransmitScope::IsolatedTest {
            peer_id: "only-peer".into(),
            intent_id: [7; 32],
            expires_at_unix_ms: 100,
            allow_known_audio_overlap: false,
        };
        config.validate().unwrap();
        assert!(config.allows_peer("only-peer"));
        assert!(!config.allows_peer("another-peer"));
        assert!(config.tx_ready_at(99));
        assert!(!config.tx_ready_at(100));
        let spent = AtomicBool::new(false);
        assert!(consume_test_intent(&spent));
        assert!(!consume_test_intent(&spent));
        assert!(!scope_current(
            Instant::now() - Duration::from_millis(1),
            u64::MAX
        ));
        assert!(!scope_current(Instant::now() + Duration::from_secs(1), 0));
    }
}
