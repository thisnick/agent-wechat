//! Native personal-call backend. Constructing it performs no login, profile read,
//! network operation or device access. An admitted invitation or reserved owner
//! command starts a worker; current account authority and journaled intents
//! fence every host mutation.

use super::{
    authority::CallAuthorityLease,
    ownership::{AccountAuthority, CallIdentity, Operation},
    service::{
        BackendAudioRequest, BackendInviteRequest, BackendOutcome, BackendRequest, BackendSink,
        CallBackend, CallServiceError,
    },
};
use crate::{
    call_types::{
        CallAudioFormat, CallCapabilities, CallConferenceInvitation, CallErrorCode,
        CallInvitationResult,
    },
    runtime::authorization,
    Core,
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    net::{SocketAddrV4, UdpSocket},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{oneshot, Notify};
use wechat_calls::session::{
    CallLiveSyncInterrupted, CallRpcNotSubmitted, NativePersonalSession, PersonalPhase,
    PersonalTransport, SyncProgress, TaskIds, UnsupportedCallMedia, VoiceParameters,
};
use wechat_protocol::{
    calls::{
        app_session::AppAuthority,
        audio_packet::MediaEpoch,
        host_policy::{
            is_reject_request, is_teardown_request, selected_policy, CallHostPolicy, CallHostRoute,
        },
        voice_offer::VoiceOffer,
    },
    crypto, AccountState,
};

#[cfg(test)]
use std::net::SocketAddr;
#[cfg(test)]
use wechat_calls::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
};
#[cfg(test)]
use wechat_protocol::calls::audio_packet::{self, Live96, MediaContext, TransmitScope};
#[cfg(test)]
use wechat_protocol::protobuf as p;

#[cfg(not(test))]
use wechat_protocol::NativeOptions;

mod conference;
mod incoming;
mod media;
use media::run as run_media;
use wechat_calls::native_media::SubmissionGate as SendAdmission;
mod transport_state;
#[cfg(test)]
mod transport_tests;
use transport_state::CallNetworkState;

type BackendFuture<T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, CallServiceError>> + Send + 'static>>;

#[derive(Clone)]
struct NativeCallConfig {
    conference_join_profile: bool,
    conference_probe_mode: conference::ProbeMode,
    ring_timeout: Duration,
    peer_idle_timeout: Duration,
    max_duration: Duration,
}
impl Default for NativeCallConfig {
    fn default() -> Self {
        Self {
            conference_join_profile: true,
            conference_probe_mode: match std::env::var("WX_CALL_CONFERENCE_SPIKE").as_deref() {
                Ok("duplex-tone") => conference::ProbeMode::DuplexTone,
                Ok("multi-receive") => conference::ProbeMode::MultiReceive,
                Ok("receive-audio") => conference::ProbeMode::Receive,
                Ok("join-profile") => conference::ProbeMode::Profile,
                _ => conference::ProbeMode::Application,
            },
            ring_timeout: Duration::from_secs(60),
            peer_idle_timeout: Duration::from_secs(60),
            // The supported media counter must not roll over without rekeying.
            max_duration: Duration::from_secs(1200),
        }
    }
}

pub use wechat_calls::session::independent_voice_parameters;

pub struct NativeCallBackend {
    inner: Arc<Inner>,
}
struct Inner {
    core: Weak<Core>,
    config: NativeCallConfig,
    active: Mutex<Option<Arc<Worker>>>,
    conferences: Mutex<BTreeMap<(String, String), Arc<conference::Pending>>>,
    #[cfg(test)]
    test_io: Option<Arc<dyn TestCallIo>>,
}
struct Worker {
    identity: CallIdentity,
    peer_id: String,
    lease: CallAuthorityLease,
    account_lease: std::sync::OnceLock<wechat_protocol::AccountLease>,
    stop: AtomicBool,
    media_stop: AtomicBool,
    locally_closed: AtomicBool,
    generation: AtomicU32,
    submission: SendAdmission,
    metrics: MediaMetrics,
    mutations_submitted: AtomicBool,
    incoming: Option<Arc<incoming::Mailbox>>,
    conference: Option<Arc<conference::Pending>>,
    conference_start: Option<Vec<String>>,
    conference_commands: Mutex<std::collections::VecDeque<conference::Command>>,
    audio: SyncSender<QueuedAudio>,
    done: Mutex<Option<BackendOutcome>>,
    notify: Notify,
}
struct QueuedAudio {
    request: BackendAudioRequest,
    queued: Instant,
}
impl Worker {
    fn has_commands(&self) -> bool {
        self.conference_commands
            .lock()
            .is_ok_and(|commands| !commands.is_empty())
    }
    fn is_conference(&self) -> bool {
        self.conference.is_some() || self.conference_start.is_some()
    }
    fn reusable(&self) -> bool {
        self.done.lock().is_ok_and(|done| {
            done.is_some_and(|outcome| {
                matches!(outcome, BackendOutcome::Ended)
                    || self.locally_closed.load(Ordering::Acquire)
            })
        })
    }

    fn authorized(&self) -> bool {
        self.lease.current()
            && self
                .account_lease
                .get()
                .is_none_or(wechat_protocol::AccountLease::current)
    }
    fn current(&self) -> bool {
        self.authorized() && !self.media_stop.load(Ordering::Acquire)
    }
    fn finish(&self, outcome: BackendOutcome) {
        if let Ok(mut commands) = self.conference_commands.lock() {
            for command in commands.drain(..) {
                let _ = command
                    .reply
                    .send(Err(service_error(CallErrorCode::Closed)));
            }
        }
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
    pub fn new(core: Weak<Core>) -> Self {
        Self {
            inner: Arc::new(Inner {
                core,
                config: NativeCallConfig::default(),
                active: Mutex::new(None),
                conferences: Mutex::new(BTreeMap::new()),
                #[cfg(test)]
                test_io: None,
            }),
        }
    }
}
impl CallBackend for NativeCallBackend {
    fn conference_invitations(
        &self,
        authority: &AccountAuthority,
    ) -> Vec<CallConferenceInvitation> {
        conference::invitations(&self.inner, authority)
    }
    fn invite(&self, request: BackendInviteRequest) -> BackendFuture<Vec<CallInvitationResult>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &request.identity)?;
            if !worker.is_conference() || !worker.current() || worker.stop.load(Ordering::Acquire) {
                return Err(service_error(CallErrorCode::Stale));
            }
            let (reply, received) = oneshot::channel();
            {
                let mut commands = worker
                    .conference_commands
                    .lock()
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
                if commands.len() >= 16 {
                    return Err(service_error(CallErrorCode::Capacity));
                }
                commands.push_back(conference::Command {
                    action: conference::CommandAction::Invite(request.contacts),
                    reply,
                });
            }
            match received
                .await
                .map_err(|_| service_error(CallErrorCode::Closed))??
            {
                conference::CommandResult::Invitations(results) => Ok(results),
                conference::CommandResult::Muted => {
                    Err(service_error(CallErrorCode::InvalidRequest))
                }
            }
        })
    }
    fn set_muted(&self, identity: CallIdentity, muted: bool) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &identity)?;
            if !worker.current() || worker.stop.load(Ordering::Acquire) {
                return Err(service_error(CallErrorCode::Stale));
            }
            if !worker.is_conference() {
                return Ok(());
            }
            let (reply, received) = oneshot::channel();
            {
                let mut commands = worker
                    .conference_commands
                    .lock()
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
                if commands.len() >= 16 {
                    return Err(service_error(CallErrorCode::Capacity));
                }
                commands.push_back(conference::Command {
                    action: conference::CommandAction::Mute(muted),
                    reply,
                });
            }
            match received
                .await
                .map_err(|_| service_error(CallErrorCode::Closed))??
            {
                conference::CommandResult::Muted => Ok(()),
                conference::CommandResult::Invitations(_) => {
                    Err(service_error(CallErrorCode::InvalidRequest))
                }
            }
        })
    }
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        let core = self.inner.core.upgrade()?;
        let lease = core.cached_call_authority(account)?;
        lease.current().then(|| lease.authority().clone())
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: self.authority(account).is_some(),
            conference_join: self.inner.config.conference_join_profile
                && self.authority(account).is_some(),
            conference_create: self.inner.config.conference_probe_mode
                == conference::ProbeMode::Application
                && self.authority(account).is_some(),
            conference_audio: self.inner.config.conference_probe_mode
                == conference::ProbeMode::Application
                && self.authority(account).is_some(),
            conference_invite: self.inner.config.conference_probe_mode
                == conference::ProbeMode::Application
                && self.authority(account).is_some(),
            ..CallCapabilities::default()
        }
    }
    fn observe_conference(
        &self,
        note: Arc<super::notifications::IncomingNotification>,
        ttl: Duration,
    ) {
        conference::observe(&self.inner, note, ttl);
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
            if request.call_type != crate::call_types::CallType::Voice {
                return Ok(BackendOutcome::Ended);
            }
            let Some(core) = inner.core.upgrade() else {
                return Ok(BackendOutcome::Ended);
            };
            let admission_core = Arc::clone(&core);
            let admission_account = request.ticket.authority().account_id.clone();
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
            let conference = if request.peer_id.ends_with("@chatroom")
                && request.conference_invitees.is_none()
            {
                let Some(expected) = request.conference_invitation.as_ref() else {
                    return Ok(BackendOutcome::Ended);
                };
                let Some(pending) =
                    conference::claim(&inner, &request.peer_id, lease.authority(), expected)
                else {
                    return Ok(BackendOutcome::Ended);
                };
                Some(pending)
            } else {
                None
            };
            let (audio, rx) = mpsc::sync_channel(32);
            let worker = Arc::new(Worker {
                identity: request.ticket.identity(),
                peer_id: request.peer_id.clone(),
                lease,
                account_lease: std::sync::OnceLock::new(),
                stop: AtomicBool::new(false),
                media_stop: AtomicBool::new(false),
                locally_closed: AtomicBool::new(false),
                generation: AtomicU32::new(1),
                submission: SendAdmission::default(),
                metrics: MediaMetrics::default(),
                mutations_submitted: AtomicBool::new(false),
                incoming: None,
                conference,
                conference_start: request.conference_invitees.clone(),
                conference_commands: Mutex::new(std::collections::VecDeque::new()),
                audio,
                done: Mutex::new(None),
                notify: Notify::new(),
            });
            {
                let mut active = inner
                    .active
                    .lock()
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
                if active.as_ref().is_some_and(|old| !old.reusable()) {
                    return Err(service_error(CallErrorCode::Busy));
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
                    let no_mutation = !actor_worker.mutations_submitted.load(Ordering::Acquire);
                    let terminal = result.is_ok() || no_mutation;
                    if result.is_err() {
                        if let Some(core) = actor_inner.core.upgrade() {
                            if let Err(error) = &result {
                                conference::record_failure(&core, &actor_worker, error);
                            }
                            let _ = if no_mutation {
                                core.store.close_call_locally(
                                    &actor_worker.lease.authority().account_id,
                                    actor_worker.identity.call_id(),
                                    wechat_store::CallLocalEndReason::NoRemoteMutation,
                                )
                            } else {
                                core.store.finish_call(
                                    &actor_worker.lease.authority().account_id,
                                    actor_worker.identity.call_id(),
                                    false,
                                )
                            };
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
    fn close_local(
        &self,
        identity: CallIdentity,
        reason: wechat_store::CallLocalEndReason,
    ) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = inner
                .active
                .lock()
                .map_err(|_| service_error(CallErrorCode::Closed))?
                .as_ref()
                .filter(|worker| worker.identity == identity)
                .cloned();
            if let Some(worker) = &worker {
                let done = worker
                    .done
                    .lock()
                    .map_err(|_| service_error(CallErrorCode::Closed))?
                    .is_some();
                if !done && reason != wechat_store::CallLocalEndReason::ServerShutdown {
                    return Err(service_error(CallErrorCode::OutcomeUnknown));
                }
            }
            let worker = worker.ok_or_else(|| service_error(CallErrorCode::Stale))?;
            let core = inner
                .core
                .upgrade()
                .ok_or_else(|| service_error(CallErrorCode::Closed))?;
            let _guard = core
                .lock_account(&worker.lease.authority().account_id)
                .await
                .map_err(|_| service_error(CallErrorCode::Closed))?;
            core.store
                .close_call_locally(
                    &worker.lease.authority().account_id,
                    identity.call_id(),
                    reason,
                )
                .map_err(|_| service_error(CallErrorCode::Closed))?;
            worker.locally_closed.store(true, Ordering::Release);
            Ok(())
        })
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
    fn park_incoming_sync(&self) -> bool {
        false
    }
    fn call(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
        handoff: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>>;
}
const CALL_RPC_TIMEOUT: Duration = Duration::from_secs(12);

struct FencedTransport {
    core: Weak<Core>,
    account: String,
    lease: CallAuthorityLease,
    call_id: String,
    ordinal: AtomicU64,
    worker: Arc<Worker>,
    admission: ControlAdmission,
    runtime: tokio::runtime::Handle,
    timeout: Duration,
    #[cfg(test)]
    test_io: Option<Arc<dyn TestCallIo>>,
}
impl FencedTransport {
    fn pre_answer_poll(&self, route: CallHostRoute) -> bool {
        route == CallHostRoute::LiveSync && matches!(self.admission, ControlAdmission::Incoming(_))
    }
    fn wait_for_conference_sync<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.runtime.block_on(async {
            tokio::pin!(exchange);
            let mut stop_check = tokio::time::interval(Duration::from_millis(20));
            loop {
                tokio::select! {
                    biased;
                    reply = &mut exchange => return reply,
                    _ = stop_check.tick() => {
                        if !self.worker.current() || self.worker.stop.load(Ordering::Acquire) || self.worker.has_commands() {
                            return Err(CallLiveSyncInterrupted.into());
                        }
                    }
                }
            }
        })
    }
    fn wait_for_incoming_sync<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let mailbox = self
            .worker
            .incoming
            .as_ref()
            .context("Incoming mailbox missing")?;
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                reply = exchange => reply,
                _ = mailbox.answer_queued.notified() => Err(CallLiveSyncInterrupted.into()),
            }
        })
    }
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
        self.admission.nonterminal_current() && self.worker.authorized()
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
                        &serde_json::json!({"cgi":route.command(),"stage":error.stage.as_str(),"code":error.code,"value":error.value,"cryptoStep":error.crypto_step,"mayHaveSubmitted":handed_off || error.may_have_submitted()}),
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
                if route == CallHostRoute::LiveSync {
                    // Missing a read-only poll reply cannot revoke the known
                    // room's separately fenced cleanup. Retain its uncertainty
                    // without acknowledging it or granting a retry.
                    core.store.finish_interrupted_live_sync(
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
        if !self.worker.authorized() || core.stopping.load(Ordering::Acquire) {
            return Err(CallRpcNotSubmitted.into());
        }
        if route == CallHostRoute::ConferenceJoin
            && !self
                .worker
                .conference
                .as_ref()
                .is_some_and(|pending| pending.current())
        {
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
        let native = core.native_account(
            &self.account,
            NativeOptions {
                timeout: self.timeout,
                ..NativeOptions::default()
            },
        )?;
        #[cfg(not(test))]
        if self.worker.account_lease.get().is_none() {
            let _ = self.worker.account_lease.set(native.account_lease()?);
        }
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
            if !self.worker.authorized() || core.stopping.load(Ordering::Acquire) {
                return Err(CallRpcNotSubmitted.into());
            }
            if !self.admits(teardown, rejecting) {
                return Err(CallRpcNotSubmitted.into());
            }
            if route == CallHostRoute::ConferenceJoin
                && !self
                    .worker
                    .conference
                    .as_ref()
                    .is_some_and(|pending| pending.current())
            {
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
                .and_then(|io| {
                    if self.pre_answer_poll(route) && io.park_incoming_sync() {
                        handoff()?;
                        self.wait_for_incoming_sync(std::future::pending())
                    } else {
                        io.call(&mut exchanged, route, request, &mut handoff)
                    }
                })
        };
        #[cfg(not(test))]
        let reply_retained = AtomicBool::new(false);
        #[cfg(not(test))]
        let result = native.request_blocking(|operation| {
            let retain = |reply: &[u8]| {
                self.retain_host_result(&core, &action, route, &Ok(reply.to_vec()), true)?;
                reply_retained.store(true, Ordering::Release);
                Ok(())
            };
            if self.pre_answer_poll(route) {
                operation.call_live_sync(
                    request,
                    self.timeout,
                    handoff,
                    |exchange| self.wait_for_incoming_sync(exchange),
                    retain,
                )
            } else if route == CallHostRoute::LiveSync && self.worker.is_conference() {
                operation.call_live_sync(
                    request,
                    Duration::from_secs(60),
                    handoff,
                    |exchange| self.wait_for_conference_sync(exchange),
                    retain,
                )
            } else {
                operation.call_rpc(route, request, handoff, retain)
            }
        });
        #[cfg(not(test))]
        if let Ok(accepted) = native.export_account_state() {
            exchanged = accepted;
        }
        // Retain the response before trying to reacquire: a poisoned/closed
        // writer must not discard an authenticated reply. These are operational
        // sealed receipts, never account/profile state or mutation permission.
        let handed_off = handed_off.load(Ordering::Acquire);
        if route == CallHostRoute::LiveSync
            && handed_off
            && result
                .as_ref()
                .is_err_and(|error| error.is::<CallLiveSyncInterrupted>())
        {
            core.store
                .finish_interrupted_live_sync(&self.account, &self.call_id, &action)?;
            return result;
        }
        #[cfg(test)]
        let retained = self.retain_host_result(&core, &action, route, &result, handed_off);
        #[cfg(not(test))]
        let retained = if reply_retained.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.retain_host_result(&core, &action, route, &result, handed_off)
        };
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
    worker: &Worker,
    core: &Core,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let account = &worker.lease.authority().account_id;
    let call_id = worker.identity.call_id();
    let _guard = runtime.block_on(core.lock_account(account))?;
    ensure!(worker.authorized(), "Call authority revoked");
    ensure!(
        core.store
            .begin_call(account, call_id, &worker.lease.authority().generation)?,
        "Previous call requires reconciliation"
    );
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
    let account = &worker.lease.authority().account_id;
    let call_id = worker.identity.call_id();
    begin_operational_call(worker, &core, runtime)?;
    let transport = FencedTransport {
        core: inner.core.clone(),
        account: account.clone(),
        lease: worker.lease.clone(),
        call_id: call_id.into(),
        ordinal: AtomicU64::new(1),
        worker: worker.clone(),
        admission: ControlAdmission::Owner(sink.clone()),
        runtime: runtime.clone(),
        timeout: CALL_RPC_TIMEOUT,
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
    let mut parameters = independent_voice_parameters()?;
    parameters.apply_offer(&voice_offer);
    if worker.is_conference() {
        return conference::run(
            &transport,
            state,
            auth,
            parameters,
            tasks,
            conference::RunInput {
                admitted,
                probe: inner.config.conference_probe_mode,
                audio,
                request_id: request.request_id,
            },
        );
    }
    let mut session = NativePersonalSession::authenticate(
        &transport,
        &mut state,
        auth,
        tasks.host,
        crypto::random::<32>()?,
        parameters,
    )?;
    if worker.stop.load(Ordering::Acquire) || !worker.authorized() {
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
        ensure!(worker.authorized(), "Call authority revoked");
        match session.media_ready(&state, auth) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) if error.is::<UnsupportedCallMedia>() => {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= ringing_end || !worker.authorized() {
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
    if let Some(muted) = session.peer_microphone_muted() {
        let _ = runtime.block_on(sink.peer_microphone(muted));
    }
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
            ensure!(worker.authorized(), "Call authority revoked");
            if !worker.authorized() {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            match session.poll(&transport, &mut state, auth, tasks.next()?) {
                Ok(SyncProgress::MediaReady) => {
                    if let Some(muted) = session.peer_microphone_muted() {
                        let _ = runtime.block_on(sink.peer_microphone(muted));
                    }
                }
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
    ensure!(worker.authorized(), "Call authority revoked before Exit");
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

fn send_datagram(
    socket: &UdpSocket,
    endpoint: SocketAddrV4,
    packet: &[u8],
    worker: &Worker,
    audio: &[&QueuedAudio],
) -> Result<bool> {
    send_datagram_generation(socket, endpoint, packet, worker, audio, None)
}
fn send_datagram_generation(
    socket: &UdpSocket,
    endpoint: SocketAddrV4,
    packet: &[u8],
    worker: &Worker,
    audio: &[&QueuedAudio],
    generation: Option<u32>,
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
                Ok(
                    generation.is_none_or(|next| worker.generation.load(Ordering::Acquire) == next)
                        && audio.iter().all(|item| audio_current(worker, item)),
                )
            },
            || {
                worker
                    .lease
                    .with_current(|| {
                        ensure!(
                            worker.current() && !worker.stop.load(Ordering::Acquire),
                            "Call authority revoked before send"
                        );
                        if generation
                            .is_some_and(|next| worker.generation.load(Ordering::Acquire) != next)
                            || !audio.iter().all(|item| audio_current(worker, item))
                        {
                            return Ok(false);
                        }
                        let written = match socket.send_to(packet, endpoint) {
                            Ok(written) => written,
                            Err(error)
                                if (!audio.is_empty() || generation.is_some())
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
fn nonzero64(value: &[u8]) -> u64 {
    let digest = Sha256::digest(value);
    u64::from_le_bytes(digest[..8].try_into().expect("digest width")).max(1)
}
fn unix_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
