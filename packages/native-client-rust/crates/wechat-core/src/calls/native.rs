//! Native personal-call backend. Constructing it performs no login, profile read,
//! network operation or device access. An admitted invitation or reserved owner
//! command starts a worker; current account authority and journaled intents
//! fence every host mutation.

use super::{
    authority::CallAuthorityLease,
    ownership::{AccountAuthority, CallIdentity, Operation},
    service::{
        BackendAudioRequest, BackendInviteRequest, BackendOutcome, BackendRequest, BackendSink,
        BackendVideoRequest, CallBackend, CallServiceError,
    },
};
use crate::{
    call_types::{
        CallAudioFormat, CallCapabilities, CallConferenceInvitation, CallErrorCode,
        CallInvitationResult, CallType,
    },
    runtime::authorization,
    Core,
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    future::Future,
    net::{SocketAddr, UdpSocket},
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
    independent_video_parameters, CallLiveSyncInterrupted, CallRpcNotSubmitted,
    NativePersonalSession, PersonalPhase, PersonalTransport, SyncProgress, TaskIds,
    UnsupportedCallMedia, VoiceParameters,
};
use wechat_protocol::{
    calls::{
        app_session::{AppAuthority, AppSession},
        audio_packet::MediaEpoch,
        host_policy::{
            is_reject_request, is_teardown_request, selected_policy, CallHostPolicy, CallHostRoute,
        },
        personal::CallMediaMode,
        relay::{RelayEpoch, RelayTarget},
        voice_offer::{VideoOfferOptions, VoiceOffer},
    },
    crypto, AccountState,
};

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
use wechat_calls::native_media::{
    preconnect_relay, PreconnectedRelay, SubmissionGate as SendAdmission,
};
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
    prefer_live98: bool,
    ring_timeout: Duration,
    peer_idle_timeout: Duration,
    max_duration: Duration,
    direct_camera_off_grace: Duration,
    direct_camera_off_recovery_grace: Duration,
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
            prefer_live98: std::env::var("WX_CALL_LIVE98").as_deref() == Ok("1"),
            ring_timeout: Duration::from_secs(60),
            peer_idle_timeout: Duration::from_secs(60),
            // The supported media counter must not roll over without rekeying.
            max_duration: Duration::from_secs(1200),
            // Direct video calls join/invite with the video status bit set so the
            // relay forwards initial keyframe requests and peers establish video;
            // when the user chose camera-off, SwitchAV clears the camera bit once
            // the first inbound frame is ready, after `direct_camera_off_grace` if
            // no remote video is arriving, or after `direct_camera_off_recovery_grace`
            // if remote video is still waiting on its initial keyframe.
            direct_camera_off_grace: Duration::from_secs(3),
            direct_camera_off_recovery_grace: Duration::from_secs(10),
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
    app_sessions: Mutex<BTreeMap<String, CachedAppSession>>,
    #[cfg(test)]
    test_io: Option<Arc<dyn TestCallIo>>,
}
const APP_SESSION_TTL: Duration = Duration::from_secs(15 * 60);

struct CachedAppSession {
    session: AppSession,
    cached_at: Instant,
    cached_at_wall: SystemTime,
    lease: CallAuthorityLease,
}

impl CachedAppSession {
    fn current(&self) -> bool {
        self.lease.current()
            && self.cached_at.elapsed() < APP_SESSION_TTL
            && self
                .cached_at_wall
                .elapsed()
                .is_ok_and(|elapsed| elapsed < APP_SESSION_TTL)
    }
}

impl Inner {
    fn cached_app_session(&self, account: &str, authority: AppAuthority) -> Option<AppSession> {
        let mut sessions = self.app_sessions.lock().ok()?;
        sessions.retain(|_, cached| cached.current());
        if let Some(cached) = sessions.get(account) {
            if cached.session.validate_authority(authority).is_ok() {
                return Some(cached.session.clone());
            }
            sessions.remove(account);
        }
        None
    }

    fn store_app_session(&self, lease: &CallAuthorityLease, session: AppSession) {
        if let Ok(mut sessions) = self.app_sessions.lock() {
            if lease.current() {
                sessions.insert(
                    lease.authority().account_id.clone(),
                    CachedAppSession {
                        session,
                        cached_at: Instant::now(),
                        cached_at_wall: SystemTime::now(),
                        lease: lease.clone(),
                    },
                );
            }
        }
    }

    fn invalidate_app_session(&self, account: &str) {
        if let Ok(mut sessions) = self.app_sessions.lock() {
            sessions.remove(account);
        }
    }

    fn invalidate_app_session_on_error<T>(&self, account: &str, result: Result<T>) -> Result<T> {
        result.inspect_err(|error| {
            if !error.is::<CallRpcNotSubmitted>() {
                self.invalidate_app_session(account);
            }
        })
    }
}

struct Worker {
    identity: CallIdentity,
    peer_id: String,
    call_type: CallType,
    lease: CallAuthorityLease,
    account_lease: std::sync::OnceLock<wechat_protocol::AccountLease>,
    stop: AtomicBool,
    media_stop: AtomicBool,
    locally_closed: AtomicBool,
    generation: AtomicU32,
    video_generation: AtomicU32,
    muted: AtomicBool,
    camera_enabled: AtomicBool,
    initial_camera_off_pending: AtomicBool,
    initial_camera_off_deadlines: Mutex<Option<(Instant, Instant)>>,
    rx_video_packets_seen: Arc<AtomicBool>,
    rx_video_keyframe_seen: Arc<AtomicBool>,
    joined_commands_ready: AtomicBool,
    submission: SendAdmission,
    metrics: MediaMetrics,
    mutations_submitted: AtomicBool,
    incoming: Option<Arc<incoming::Mailbox>>,
    conference: Option<Arc<conference::Pending>>,
    conference_start: Option<Vec<String>>,
    conference_commands: Mutex<std::collections::VecDeque<conference::Command>>,
    direct_commands: Mutex<std::collections::VecDeque<DirectCommand>>,
    audio: SyncSender<QueuedAudio>,
    video: SyncSender<QueuedVideo>,
    video_keyframe_requests: SyncSender<u32>,
    done: Mutex<Option<BackendOutcome>>,
    notify: Notify,
}
struct DirectCommand {
    sets_camera: bool,
    reply: oneshot::Sender<std::result::Result<(), CallServiceError>>,
}
struct QueuedAudio {
    request: BackendAudioRequest,
    queued: Instant,
}
struct QueuedVideo {
    request: BackendVideoRequest,
    queued: Instant,
}
impl Worker {
    fn has_commands(&self) -> bool {
        self.conference_commands
            .lock()
            .is_ok_and(|commands| !commands.is_empty())
    }
    fn has_direct_commands(&self) -> bool {
        self.direct_commands
            .lock()
            .is_ok_and(|commands| !commands.is_empty())
    }
    fn schedule_initial_camera_off(&self) {
        self.initial_camera_off_pending
            .store(true, Ordering::Release);
    }
    fn arm_initial_camera_off(&self, idle_grace: Duration, recovery_grace: Duration) {
        if !self.initial_camera_off_pending.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        if let Ok(mut deadlines) = self.initial_camera_off_deadlines.lock() {
            *deadlines = Some((now + idle_grace, now + recovery_grace.max(idle_grace)));
        }
    }
    fn cancel_initial_camera_off(&self) {
        self.initial_camera_off_pending
            .store(false, Ordering::Release);
        if let Ok(mut deadlines) = self.initial_camera_off_deadlines.lock() {
            *deadlines = None;
        }
    }
    fn initial_camera_off_due(&self) -> bool {
        if !self.initial_camera_off_pending.load(Ordering::Acquire) {
            return false;
        }
        if self.rx_video_keyframe_seen.load(Ordering::Acquire) {
            return true;
        }
        let Some((idle_deadline, recovery_deadline)) = self
            .initial_camera_off_deadlines
            .lock()
            .ok()
            .and_then(|guard| *guard)
        else {
            return false;
        };
        let now = Instant::now();
        if self.rx_video_packets_seen.load(Ordering::Acquire) {
            now >= recovery_deadline
        } else {
            now >= idle_deadline
        }
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
        if let Ok(mut commands) = self.direct_commands.lock() {
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
                app_sessions: Mutex::new(BTreeMap::new()),
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
                conference::CommandResult::Muted | conference::CommandResult::Camera => {
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
            worker.muted.store(muted, Ordering::Release);
            if !worker.is_conference() {
                let (reply, received) = oneshot::channel();
                {
                    let mut commands = worker
                        .direct_commands
                        .lock()
                        .map_err(|_| service_error(CallErrorCode::Closed))?;
                    if commands.len() >= 16 {
                        return Err(service_error(CallErrorCode::Capacity));
                    }
                    commands.push_back(DirectCommand {
                        sets_camera: false,
                        reply,
                    });
                }
                return received
                    .await
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
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
                    action: conference::CommandAction::Mute,
                    reply,
                });
            }
            match received
                .await
                .map_err(|_| service_error(CallErrorCode::Closed))??
            {
                conference::CommandResult::Muted => Ok(()),
                conference::CommandResult::Invitations(_) | conference::CommandResult::Camera => {
                    Err(service_error(CallErrorCode::InvalidRequest))
                }
            }
        })
    }
    fn set_camera(&self, identity: CallIdentity, enabled: bool) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &identity)?;
            if !worker.current() || worker.stop.load(Ordering::Acquire) {
                return Err(service_error(CallErrorCode::Stale));
            }
            worker.camera_enabled.store(enabled, Ordering::Release);
            worker.cancel_initial_camera_off();
            if !worker.is_conference() {
                let (reply, received) = oneshot::channel();
                {
                    let mut commands = worker
                        .direct_commands
                        .lock()
                        .map_err(|_| service_error(CallErrorCode::Closed))?;
                    if commands.len() >= 16 {
                        return Err(service_error(CallErrorCode::Capacity));
                    }
                    commands.push_back(DirectCommand {
                        sets_camera: true,
                        reply,
                    });
                }
                return received
                    .await
                    .map_err(|_| service_error(CallErrorCode::Closed))?;
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
                    action: conference::CommandAction::Camera,
                    reply,
                });
            }
            match received
                .await
                .map_err(|_| service_error(CallErrorCode::Closed))??
            {
                conference::CommandResult::Camera => Ok(()),
                conference::CommandResult::Invitations(_) | conference::CommandResult::Muted => {
                    Err(service_error(CallErrorCode::InvalidRequest))
                }
            }
        })
    }
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        let core = self.inner.core.upgrade()?;
        let authority = core
            .cached_call_authority(account)
            .filter(CallAuthorityLease::current)
            .map(|lease| lease.authority().clone());
        if authority.is_none() {
            self.inner.invalidate_app_session(account);
        }
        authority
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        let authorized = self.authority(account).is_some();
        let app_mode =
            self.inner.config.conference_probe_mode == conference::ProbeMode::Application;
        CallCapabilities {
            direct_audio: authorized,
            direct_video: authorized,
            conference_join: self.inner.config.conference_join_profile && authorized,
            conference_create: app_mode && authorized,
            conference_audio: app_mode && authorized,
            conference_video: app_mode && authorized,
            conference_invite: app_mode && authorized,
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
            let (video, video_rx) = mpsc::sync_channel(8);
            let (video_keyframe_requests, keyframe_rx) = mpsc::sync_channel(8);
            let worker = Arc::new(Worker {
                identity: request.ticket.identity(),
                peer_id: request.peer_id.clone(),
                call_type: request.call_type,
                lease,
                account_lease: std::sync::OnceLock::new(),
                stop: AtomicBool::new(false),
                media_stop: AtomicBool::new(false),
                locally_closed: AtomicBool::new(false),
                generation: AtomicU32::new(1),
                video_generation: AtomicU32::new(1),
                muted: AtomicBool::new(request.muted),
                camera_enabled: AtomicBool::new(request.camera_enabled),
                initial_camera_off_pending: AtomicBool::new(false),
                initial_camera_off_deadlines: Mutex::new(None),
                rx_video_packets_seen: Arc::new(AtomicBool::new(false)),
                rx_video_keyframe_seen: Arc::new(AtomicBool::new(false)),
                joined_commands_ready: AtomicBool::new(false),
                submission: SendAdmission::default(),
                metrics: MediaMetrics::default(),
                mutations_submitted: AtomicBool::new(false),
                incoming: None,
                conference,
                conference_start: request.conference_invitees.clone(),
                conference_commands: Mutex::new(std::collections::VecDeque::new()),
                direct_commands: Mutex::new(std::collections::VecDeque::new()),
                audio,
                video,
                video_keyframe_requests,
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
                            video_rx,
                            keyframe_rx,
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
            if !worker.authorized()
                || !request.is_current()
                || request.generation != worker.generation.load(Ordering::Acquire)
            {
                return Err(service_error(CallErrorCode::Stale));
            }
            if request.format != CallAudioFormat::default() || request.samples.len() != 320 {
                return Err(service_error(CallErrorCode::InvalidRequest));
            }
            if worker.media_stop.load(Ordering::Acquire) {
                return Ok(());
            }
            match worker.audio.try_send(QueuedAudio {
                request,
                queued: Instant::now(),
            }) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    // Media worker already exited or is exiting while authority is still valid;
                    // absorb late PCM until the control actor finishes Ending/Ended teardown
                    // instead of dropping the control client prematurely.
                    if worker.authorized() {
                        return Ok(());
                    }
                    return Err(service_error(CallErrorCode::Stale));
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    return Err(service_error(CallErrorCode::Capacity));
                }
            }
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
    fn send_video(&self, request: BackendVideoRequest) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &request.identity)?;
            if !worker.authorized()
                || !request.is_current()
                || request.generation != worker.video_generation.load(Ordering::Acquire)
            {
                return Err(service_error(CallErrorCode::Stale));
            }
            if !worker.camera_enabled.load(Ordering::Acquire)
                || worker.media_stop.load(Ordering::Acquire)
            {
                return Ok(());
            }
            match worker.video.try_send(QueuedVideo {
                request,
                queued: Instant::now(),
            }) {
                Ok(()) => Ok(()),
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    if worker.authorized() {
                        Ok(())
                    } else {
                        Err(service_error(CallErrorCode::Stale))
                    }
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    Err(service_error(CallErrorCode::Capacity))
                }
            }
        })
    }
    fn clear_video(&self, identity: CallIdentity, generation: u32) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &identity)?;
            if generation == 0 {
                return Err(service_error(CallErrorCode::Stale));
            }
            worker
                .video_generation
                .fetch_max(generation, Ordering::AcqRel);
            Ok(())
        })
    }
    fn request_video_keyframe(&self, identity: CallIdentity, track_id: u32) -> BackendFuture<()> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let worker = active(&inner, &identity)?;
            if !worker.current() || worker.stop.load(Ordering::Acquire) {
                return Err(service_error(CallErrorCode::Stale));
            }
            let _ = worker.video_keyframe_requests.try_send(track_id);
            Ok(())
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

#[derive(Clone)]
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
    /// Resolves when a joined conference should stop its live-sync poll.
    fn conference_interrupt(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        live_sync_interrupt(self.worker.clone(), |worker| worker.has_commands())
    }
    /// Resolves when a direct video call should stop its live-sync poll.
    fn direct_interrupt(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        live_sync_interrupt(self.worker.clone(), |worker| {
            worker.has_direct_commands() || worker.initial_camera_off_due()
        })
    }
    /// Resolves when an answer is queued for a ringing incoming call.
    fn incoming_interrupt(&self) -> Result<impl std::future::Future<Output = ()> + Send + 'static> {
        let mailbox = self
            .worker
            .incoming
            .clone()
            .context("Incoming mailbox missing")?;
        Ok(async move {
            mailbox.answer_queued.notified().await;
            // Let a reply that is already ready win, as it did before.
            tokio::task::yield_now().await;
        })
    }
    #[cfg(test)]
    fn wait_for_interrupt<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
        interrupt: impl std::future::Future<Output = ()>,
    ) -> Result<T> {
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                reply = exchange => reply,
                () = interrupt => Err(CallLiveSyncInterrupted.into()),
            }
        })
    }
    #[cfg(test)]
    fn wait_for_conference_sync<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.wait_for_interrupt(exchange, self.conference_interrupt())
    }
    #[cfg(test)]
    fn wait_for_direct_sync<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.wait_for_interrupt(exchange, self.direct_interrupt())
    }
    #[cfg(test)]
    fn wait_for_incoming_sync<T>(
        &self,
        exchange: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.wait_for_interrupt(exchange, self.incoming_interrupt()?)
    }
    fn admits(&self, teardown: bool, rejecting: bool) -> bool {
        admission_open(&self.worker, &self.admission, teardown, rejecting)
    }
}

/// Resolves when the worker stops, loses authority, or (once joined) has
/// `pending` commands to send, checked every 20 ms. It yields once before
/// resolving so a live-sync reply that is already ready still wins.
async fn live_sync_interrupt(worker: Arc<Worker>, pending: fn(&Worker) -> bool) {
    let mut stop_check = tokio::time::interval(Duration::from_millis(20));
    loop {
        stop_check.tick().await;
        if !worker.current()
            || worker.stop.load(Ordering::Acquire)
            || (worker.joined_commands_ready.load(Ordering::Acquire) && pending(&worker))
        {
            tokio::task::yield_now().await;
            return;
        }
    }
}

/// Whether a call request may still be submitted under `admission`.
fn admission_open(
    worker: &Worker,
    admission: &ControlAdmission,
    teardown: bool,
    rejecting: bool,
) -> bool {
    if teardown {
        return true;
    }
    if worker
        .incoming
        .as_ref()
        .is_some_and(|mailbox| mailbox.cancellation_pending())
    {
        return false;
    }
    if rejecting {
        return admission.terminal_current();
    }
    admission.nonterminal_current() && worker.authorized()
}
impl FencedTransport {
    /// Operational evidence is independent of account-state adoption. In
    /// particular a valid reply survives a later writer/authority failure.
    fn retain_host_result(
        account: &str,
        call_id: &str,
        core: &Core,
        action: &str,
        route: CallHostRoute,
        result: &Result<Vec<u8>>,
        handed_off: bool,
    ) -> Result<()> {
        match result {
            Ok(reply) => {
                let label = format!("call-reply:{}:{action}", call_id);
                let encrypted = core.vault.seal(account, &label, reply)?;
                core.store.finish_call_action(
                    account,
                    call_id,
                    action,
                    "received",
                    Some(&encrypted),
                )?;
                Ok(())
            }
            Err(error) => {
                let local_stop = error.is::<CallRpcNotSubmitted>() && !handed_off;
                if local_stop {
                    core.store.put_runtime(account,&format!("call-host-diagnostic:{}:{action}",call_id),&serde_json::to_vec(&serde_json::json!({"cgi":route.command(),"stage":"presubmit","code":"admission_revoked","mayHaveSubmitted":false}))?)?;
                }
                if let Some(error) =
                    error.downcast_ref::<wechat_protocol::calls::host_transport::CallHostRpcError>()
                {
                    let diagnostic = serde_json::to_vec(
                        &serde_json::json!({"cgi":route.command(),"stage":error.stage.as_str(),"code":error.code,"value":error.value,"cryptoStep":error.crypto_step,"mayHaveSubmitted":handed_off || error.may_have_submitted()}),
                    )?;
                    core.store.put_runtime(
                        account,
                        &format!("call-host-diagnostic:{}:{action}", call_id),
                        &diagnostic,
                    )?;
                }
                if local_stop {
                    core.store
                        .finish_call_action_not_submitted(account, call_id, action)?;
                    return Ok(());
                }
                if route == CallHostRoute::LiveSync {
                    // Missing a read-only poll reply cannot revoke the known
                    // room's separately fenced cleanup. Retain its uncertainty
                    // without acknowledging it or granting a retry.
                    core.store
                        .finish_interrupted_live_sync(account, call_id, action)?;
                    return Ok(());
                }
                core.store
                    .finish_call_action(account, call_id, action, "needs_review", None)?;
                Ok(())
            }
        }
    }
}
fn call_timing_enabled() -> bool {
    std::env::var("WX_CALL_TIMING").as_deref() == Ok("1")
}

struct RpcTiming {
    started: Instant,
    exchange_started: Option<Instant>,
    exchange_finished: Option<Instant>,
    lock2_started: Option<Instant>,
}

impl PersonalTransport for FencedTransport {
    fn call_rpc(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
    ) -> Result<Vec<u8>> {
        let mut timing = call_timing_enabled().then(|| RpcTiming {
            started: Instant::now(),
            exchange_started: None,
            exchange_finished: None,
            lock2_started: None,
        });
        let result = self.call_rpc_inner(state, route, request, &mut timing);
        if let Some(t) = timing {
            let end = Instant::now();
            eprintln!(
                "[wechat-call-timing] rpc route={route:?} lock1={}ms exchange={}ms lock2={}ms total={}ms ok={}",
                t.exchange_started.unwrap_or(end).duration_since(t.started).as_millis(),
                t.exchange_started.map_or(0, |start| t.exchange_finished.unwrap_or(end).duration_since(start).as_millis()),
                t.lock2_started.map_or(0, |start| end.duration_since(start).as_millis()),
                end.duration_since(t.started).as_millis(),
                result.is_ok(),
            );
        }
        result
    }
}

impl FencedTransport {
    fn call_rpc_inner(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
        timing: &mut Option<RpcTiming>,
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
        // A pre-answer poll needs the incoming mailbox; check before spending
        // a call action so a missing one leaves nothing half recorded.
        #[cfg(not(test))]
        let incoming = if self.pre_answer_poll(route) {
            Some(self.incoming_interrupt()?)
        } else {
            None
        };
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
        if let Some(t) = timing {
            t.exchange_started = Some(Instant::now());
        }
        // The final check runs right before transport handoff, possibly on a
        // detached task, so it owns what it reads.
        let handed_off = Arc::new(AtomicBool::new(false));
        let handoff = {
            let (worker, admission, core, handed_off) = (
                self.worker.clone(),
                self.admission.clone(),
                core.clone(),
                handed_off.clone(),
            );
            move || {
                crate::async_bridge::blocking_section(|| {
                    if !worker.authorized() || core.stopping.load(Ordering::Acquire) {
                        return Err(CallRpcNotSubmitted.into());
                    }
                    if !admission_open(&worker, &admission, teardown, rejecting) {
                        return Err(CallRpcNotSubmitted.into());
                    }
                    if route == CallHostRoute::ConferenceJoin
                        && !worker
                            .conference
                            .as_ref()
                            .is_some_and(|pending| pending.current())
                    {
                        return Err(CallRpcNotSubmitted.into());
                    }
                    handed_off.store(true, Ordering::Release);
                    if !matches!(route, CallHostRoute::AppAuth | CallHostRoute::LiveSync) {
                        worker.mutations_submitted.store(true, Ordering::Release);
                    }
                    Ok(())
                })
            }
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
        let reply_retained = Arc::new(AtomicBool::new(false));
        #[cfg(not(test))]
        let result = {
            let retain = {
                let (core, account, call_id, action, retained) = (
                    core.clone(),
                    self.account.clone(),
                    self.call_id.clone(),
                    action.clone(),
                    reply_retained.clone(),
                );
                move |reply: &[u8]| {
                    crate::async_bridge::blocking_section(|| {
                        Self::retain_host_result(
                            &account,
                            &call_id,
                            &core,
                            &action,
                            route,
                            &Ok(reply.to_vec()),
                            true,
                        )
                    })?;
                    retained.store(true, Ordering::Release);
                    Ok(())
                }
            };
            let request = request.to_vec();
            crate::async_bridge::block_on_with(
                &self.runtime,
                native.request(async |operation| {
                    if let Some(incoming) = incoming {
                        operation
                            .call_live_sync(request, self.timeout, handoff, incoming, retain)
                            .await
                    } else if route == CallHostRoute::LiveSync && self.worker.is_conference() {
                        operation
                            .call_live_sync(
                                request,
                                Duration::from_secs(60),
                                handoff,
                                self.conference_interrupt(),
                                retain,
                            )
                            .await
                    } else if route == CallHostRoute::LiveSync {
                        operation
                            .call_live_sync(
                                request,
                                self.timeout,
                                handoff,
                                self.direct_interrupt(),
                                retain,
                            )
                            .await
                    } else {
                        operation.call_rpc(route, request, handoff, retain).await
                    }
                }),
            )
        };
        if let Some(t) = timing {
            t.exchange_finished = Some(Instant::now());
        }
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
        let retained = Self::retain_host_result(
            &self.account,
            &self.call_id,
            &core,
            &action,
            route,
            &result,
            handed_off,
        );
        #[cfg(not(test))]
        let retained = if reply_retained.load(Ordering::Acquire) {
            Ok(())
        } else {
            Self::retain_host_result(
                &self.account,
                &self.call_id,
                &core,
                &action,
                route,
                &result,
                handed_off,
            )
        };
        if let Some(t) = timing {
            t.lock2_started = Some(Instant::now());
        }
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

/// Maximum time to complete the relay handshake while ringing.
const PRECONNECT_HANDSHAKE_BUDGET: Duration = Duration::from_secs(4);
/// Maximum wait to reuse an in-flight incoming relay handshake after joining.
const INCOMING_PRECONNECT_HANDOFF_WAIT: Duration = Duration::from_millis(250);
/// Maximum wait to reuse an in-flight caller relay handshake after peer answer.
const CALLER_PRECONNECT_HANDOFF_WAIT: Duration = Duration::from_millis(600);
/// Stop keeping a pre-connected socket alive once it exceeds direct media's reuse age limit.
const PRECONNECT_MAX_IDLE_AGE: Duration = Duration::from_secs(20);

struct PreconnectHandle {
    target: RelayTarget,
    ready: Arc<AtomicBool>,
    answering: Arc<AtomicBool>,
    handoff: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<Option<PreconnectedRelay>>>,
}

impl PreconnectHandle {
    fn start(
        worker: &Arc<Worker>,
        target: RelayTarget,
        role: &'static str,
        admission_current: impl Fn(bool) -> bool + Send + 'static,
    ) -> Option<Self> {
        let worker = worker.clone();
        let ready_flag = Arc::new(AtomicBool::new(false));
        let answering = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(AtomicBool::new(false));
        let thread_ready = ready_flag.clone();
        let thread_answering = answering.clone();
        let thread_handoff = handoff.clone();
        let thread_abort = abort.clone();
        let thread_target = target.clone();
        let relay_epoch = RelayEpoch {
            account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
            logout_epoch: 0,
            call_generation: nonzero64(worker.identity.call_id().as_bytes()),
            key_generation: 1,
            socket_generation: 1,
            attempt_id: 1,
        };
        let handle = thread::Builder::new()
            .name("wechat-call-preconnect".into())
            .spawn(move || -> Option<PreconnectedRelay> {
                let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
                let started = Instant::now();
                let can_continue = || {
                    !thread_abort.load(Ordering::Acquire)
                        && worker.current()
                        && !worker.stop.load(Ordering::Acquire)
                        && admission_current(thread_answering.load(Ordering::Acquire))
                };
                let handshake_current =
                    || can_continue() && started.elapsed() < PRECONNECT_HANDSHAKE_BUDGET;
                let mut ready = match preconnect_relay(
                    &thread_target,
                    relay_epoch,
                    started,
                    handshake_current,
                    |socket, endpoint, packet| {
                        ensure!(handshake_current(), "Call relay pre-connect stopped");
                        ensure!(
                            send_datagram(socket, endpoint, packet, &worker, &[])?,
                            "Call relay pre-connect handoff stopped"
                        );
                        Ok(())
                    },
                ) {
                    Ok(ready) => ready,
                    Err(error) => {
                        if video_trace {
                            eprintln!(
                                "[wechat-call-video] {role} relay preconnect skipped/failed endpoint={}: {error:#}",
                                thread_target.endpoint
                            );
                        }
                        return None;
                    }
                };
                thread_ready.store(true, Ordering::Release);
                if video_trace {
                    eprintln!(
                        "[wechat-call-video] {role} relay preconnected endpoint={} conn={} elapsed_ms={} early_pkts={}",
                        ready.endpoint(),
                        ready.connection_id(),
                        started.elapsed().as_millis(),
                        ready.early_packet_count()
                    );
                }
                while !thread_handoff.load(Ordering::Acquire) {
                    if !can_continue() || ready.age() >= PRECONNECT_MAX_IDLE_AGE {
                        return None;
                    }
                    ready.drain_early_packets();
                    thread::sleep(Duration::from_millis(5));
                }
                if !can_continue() {
                    return None;
                }
                ready.drain_early_packets();
                Some(ready)
            })
            .ok()?;
        Some(Self {
            target,
            ready: ready_flag,
            answering,
            handoff,
            abort,
            handle: Some(handle),
        })
    }

    fn mark_answering(&self) {
        self.answering.store(true, Ordering::Release);
    }

    fn claim(mut self, handoff_wait: Duration) -> Option<PreconnectedRelay> {
        self.mark_answering();
        self.handoff.store(true, Ordering::Release);
        let handle = self.handle.take()?;
        let wait_started = Instant::now();
        while !self.ready.load(Ordering::Acquire) && !handle.is_finished() {
            if wait_started.elapsed() >= handoff_wait {
                self.abort.store(true, Ordering::Release);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let relay = handle.join().ok().flatten();
        if self.abort.load(Ordering::Acquire) {
            return None;
        }
        relay
    }
}

impl Drop for PreconnectHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.abort.store(true, Ordering::Release);
            let _ = handle.join();
        }
    }
}

fn start_caller_preconnect(
    worker: &Arc<Worker>,
    session: &NativePersonalSession,
    state: &AccountState,
    auth: AppAuthority,
    sink: &BackendSink,
) -> Option<PreconnectHandle> {
    if session.phase() != PersonalPhase::Ringing
        || worker.stop.load(Ordering::Acquire)
        || !worker.current()
        || !sink.is_current()
    {
        return None;
    }
    let candidates = session.ringing_relay_candidates().ok()?;
    if candidates.is_empty() {
        return None;
    }
    let (connection_index, udp_index) = media::select_relay_indices(&candidates);
    let target = session
        .ringing_relay_target(state, auth, connection_index, udp_index)
        .ok()??;
    let sink = sink.clone();
    PreconnectHandle::start(worker, target, "caller", move |_| sink.is_current())
}

#[allow(clippy::too_many_arguments)]
fn run_control(
    inner: &Arc<Inner>,
    worker: &Arc<Worker>,
    request: BackendRequest,
    sink: BackendSink,
    audio: Receiver<QueuedAudio>,
    video: Receiver<QueuedVideo>,
    keyframe_rx: Receiver<u32>,
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
    let is_video = request.call_type == CallType::Video;
    // Retain this fresh identity until the complete call actor (including its
    // media worker) has ended. A startup template is never reused on the wire.
    let (voice_offer, mut parameters) = if is_video {
        (
            VoiceOffer::from_owned_random_video(
                state.device.client_version,
                crypto::random::<32>()?,
                VideoOfferOptions {
                    video_flags: if worker.is_conference() { 100 } else { 178 },
                    ..VideoOfferOptions::default()
                },
            )?,
            independent_video_parameters(worker.is_conference())?,
        )
    } else {
        (
            VoiceOffer::from_owned_random(state.device.client_version, crypto::random::<32>()?)?,
            independent_voice_parameters()?,
        )
    };
    parameters.apply_offer(&voice_offer);
    if worker.is_conference() {
        inner.invalidate_app_session(account);
        return conference::run(
            &transport,
            state,
            auth,
            parameters,
            tasks,
            conference::RunInput {
                admitted,
                probe: inner.config.conference_probe_mode,
                prefer_live98: inner.config.prefer_live98,
                audio,
                video,
                keyframe_rx,
                request_id: request.request_id,
            },
        );
    }
    let cached = inner.cached_app_session(account, auth);
    let cached_app = cached.is_some();
    let timing = call_timing_enabled();
    let auth_started = (timing && !cached_app).then(Instant::now);
    let mut session = match cached {
        Some(app) => NativePersonalSession::from_authenticated(&state, auth, app, parameters)?,
        None => match NativePersonalSession::authenticate(
            &transport,
            &mut state,
            auth,
            tasks.host,
            crypto::random::<32>()?,
            parameters,
        ) {
            Ok(session) => {
                inner.store_app_session(&worker.lease, session.app_session().clone());
                session
            }
            Err(error) => {
                if let Some(start) = auth_started {
                    eprintln!(
                        "[wechat-call-timing] dial cached_app=false auth={}ms invite=0ms",
                        start.elapsed().as_millis()
                    );
                }
                return Err(error);
            }
        },
    };
    let auth_ms = auth_started.map_or(0, |start| start.elapsed().as_millis());
    if worker.stop.load(Ordering::Acquire) || !worker.authorized() {
        core.store.finish_call(account, call_id, true)?;
        let _ = admitted.send(Ok(BackendOutcome::Ended));
        return Ok(());
    }
    let mode = if is_video {
        if !request.camera_enabled {
            worker.schedule_initial_camera_off();
        }
        CallMediaMode::Video {
            camera_enabled: true,
        }
    } else {
        CallMediaMode::Voice
    };
    let invite_started = timing.then(Instant::now);
    let invited = session.invite_with_mode(
        &transport,
        &mut state,
        auth,
        &request.peer_id,
        mode,
        unix_ms()?,
        tasks.next()?,
    );
    let invite_ms = invite_started.map_or(0, |start| start.elapsed().as_millis());
    if timing {
        eprintln!(
            "[wechat-call-timing] dial cached_app={cached_app} auth={auth_ms}ms invite={invite_ms}ms"
        );
    }
    if let Err(error) = invited {
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
    if let Some(code) = session.peer_member_status() {
        let _ = runtime.block_on(sink.peer_status(code));
    }
    let preconnect = start_caller_preconnect(worker, &session, &state, auth, &sink);
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
                if let Some(code) = session.peer_member_status() {
                    let _ = runtime.block_on(sink.peer_status(code));
                }
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Ok(_) => {
                if let Some(code) = session.peer_member_status() {
                    let _ = runtime.block_on(sink.peer_status(code));
                }
            }
            Err(error) if error.is::<CallLiveSyncInterrupted>() => continue,
            Err(error)
                if error.is::<CallRpcNotSubmitted>() || error.is::<UnsupportedCallMedia>() =>
            {
                worker.stop.store(true, Ordering::Release);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let prepared_media = if worker.stop.load(Ordering::Acquire) {
        drop(preconnect);
        None
    } else {
        ensure!(
            session.phase() == PersonalPhase::Joined,
            "Peer call admission incomplete"
        );
        let (connection_index, udp_index) =
            media::select_relay_indices(&session.relay_candidates()?);
        let negotiated = session.take_media(&state, auth, connection_index, udp_index)?;
        let preconnected = preconnect
            .filter(|pre| pre.target == negotiated.relay.target())
            .and_then(|pre| pre.claim(CALLER_PRECONNECT_HANDOFF_WAIT));
        Some((negotiated, preconnected))
    };
    if worker.stop.load(Ordering::Acquire) {
        drop(prepared_media);
        let _ = runtime.block_on(sink.ending());
        let result = if session.phase() == PersonalPhase::Finished {
            Ok(())
        } else if session.phase() == PersonalPhase::TerminalPending {
            inner.invalidate_app_session_on_error(
                account,
                session.finish_remote(&transport, &mut state, auth, tasks.next()?, false),
            )
        } else if session.phase() == PersonalPhase::Ringing {
            session.cancel(&transport, &mut state, auth, tasks.next()?)
        } else if session.phase() == PersonalPhase::Joined {
            inner.invalidate_app_session_on_error(
                account,
                session.exit(&transport, &mut state, auth, tasks.next()?, false),
            )
        } else {
            bail!("Call termination state unknown")
        };
        core.store.finish_call(account, call_id, result.is_ok())?;
        return result;
    }
    ensure!(worker.current(), "Call authority revoked");
    let (negotiated, preconnected) = prepared_media.context("Peer call admission incomplete")?;
    if let Some(code) = session.peer_member_status() {
        let _ = runtime.block_on(sink.peer_status(code));
    }
    if let Some(muted) = session.peer_microphone_muted() {
        let _ = runtime.block_on(sink.peer_microphone(muted));
    }
    if let Some(camera) = session.peer_camera_enabled() {
        let _ = runtime.block_on(sink.peer_camera(camera));
    }
    let cfg = inner.config.clone();
    let media_worker = worker.clone();
    let media_sink = sink.clone();
    let media_runtime = runtime.clone();
    let media_thread = thread::Builder::new()
        .name("wechat-call-media".into())
        .spawn(move || {
            let result = media::run_with_preconnected_relay(
                &cfg,
                &media_worker,
                negotiated,
                audio,
                is_video.then_some(video),
                keyframe_rx,
                preconnected,
                &media_sink,
                &media_runtime,
            );
            media_worker.media_stop.store(true, Ordering::Release);
            result
        })?;
    let deadline = Instant::now() + inner.config.max_duration;
    let mut last_av_ts = 0u64;
    if is_video {
        worker.arm_initial_camera_off(
            inner.config.direct_camera_off_grace,
            inner.config.direct_camera_off_recovery_grace,
        );
    }
    if worker.muted.load(Ordering::Acquire) {
        if let Ok(mut queue) = worker.direct_commands.lock() {
            let (reply, _) = oneshot::channel();
            queue.push_back(DirectCommand {
                sets_camera: false,
                reply,
            });
        }
    }
    worker.joined_commands_ready.store(true, Ordering::Release);
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
            drain_direct_switch_av(worker, &mut |cam| {
                let mic = !worker.muted.load(Ordering::Acquire);
                let cam = is_video && cam;
                let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros())?;
                last_av_ts = last_av_ts.saturating_add(1).max(now);
                inner.invalidate_app_session_on_error(
                    account,
                    session.switch_av(
                        &transport,
                        &mut state,
                        auth,
                        mic,
                        cam,
                        last_av_ts,
                        tasks.next()?,
                    ),
                )
            })?;
            match session.poll(&transport, &mut state, auth, tasks.next()?) {
                Ok(SyncProgress::MediaReady) => {
                    if let Some(code) = session.peer_member_status() {
                        let _ = runtime.block_on(sink.peer_status(code));
                    }
                    if let Some(muted) = session.peer_microphone_muted() {
                        let _ = runtime.block_on(sink.peer_microphone(muted));
                    }
                    if let Some(camera) = session.peer_camera_enabled() {
                        let _ = runtime.block_on(sink.peer_camera(camera));
                    }
                }
                Ok(SyncProgress::Terminal(_)) => {
                    if let Some(code) = session.peer_member_status() {
                        let _ = runtime.block_on(sink.peer_status(code));
                    }
                    break;
                }
                Ok(SyncProgress::Waiting) => {
                    if let Some(code) = session.peer_member_status() {
                        let _ = runtime.block_on(sink.peer_status(code));
                    }
                    bail!("Peer left current voice state")
                }
                Err(error) if error.is::<CallLiveSyncInterrupted>() => continue,
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
        record_media_failure(&core, account, call_id, worker, error)?;
    }
    ensure!(worker.authorized(), "Call authority revoked before Exit");
    let ended = match session.phase() {
        PersonalPhase::Finished => Ok(()),
        PersonalPhase::TerminalPending => inner.invalidate_app_session_on_error(
            account,
            session.finish_remote(&transport, &mut state, auth, tasks.next()?, false),
        ),
        PersonalPhase::Joined => inner.invalidate_app_session_on_error(
            account,
            session.exit(&transport, &mut state, auth, tasks.next()?, false),
        ),
        _ => bail!("Call termination state unknown"),
    };
    core.store.finish_call(account, call_id, ended.is_ok())?;
    ended
}

fn drain_direct_switch_av(
    worker: &Worker,
    send_switch: &mut impl FnMut(bool) -> Result<()>,
) -> Result<()> {
    let commands: Vec<DirectCommand> = {
        let mut queue = worker
            .direct_commands
            .lock()
            .map_err(|_| anyhow::anyhow!("Direct command queue closed"))?;
        queue.drain(..).collect()
    };
    let deferred_due = worker.initial_camera_off_due();
    let send_deferred = commands.is_empty() && deferred_due;
    if commands.is_empty() && !send_deferred {
        return Ok(());
    }
    let has_camera_command = commands.iter().any(|command| command.sets_camera);
    let preserve_startup_video_bit = !commands.is_empty()
        && !has_camera_command
        && !deferred_due
        && worker.initial_camera_off_pending.load(Ordering::Acquire);
    let wire_camera = if preserve_startup_video_bit {
        true
    } else {
        worker.camera_enabled.load(Ordering::Acquire)
    };
    let raw_result = send_switch(wire_camera);
    let not_submitted = raw_result
        .as_ref()
        .is_err_and(|error| error.is::<CallRpcNotSubmitted>());
    if send_deferred {
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[wechat-call-video] deferred initial switch_av(camera={wire_camera}) ok={} not_submitted={} rx_kf={}",
                raw_result.is_ok(),
                not_submitted,
                worker.rx_video_keyframe_seen.load(Ordering::Acquire),
            );
        }
        if not_submitted {
            // Keep `initial_camera_off_pending` armed if the RPC was fenced
            // pre-submit (for example by a post-Join Cancel that later drains
            // as Ignored). Real stop/revocation exits before the next poll.
            return Ok(());
        }
        // Clear pending so a possibly-accepted post-handoff SwitchAV is never
        // blindly retried, and keep the active call running (matching how a
        // failed user-requested SwitchAV does not tear down the call).
        worker.cancel_initial_camera_off();
        return Ok(());
    }
    if !preserve_startup_video_bit && !not_submitted {
        worker.cancel_initial_camera_off();
    }
    if let Err(error) = &raw_result {
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[wechat-call-video] direct switch_av(camera={wire_camera}) failed: {error:#}"
            );
        }
    }
    let result = raw_result.map_err(|error| {
        if error.is::<CallRpcNotSubmitted>() {
            service_error(CallErrorCode::Stale)
        } else {
            service_error(CallErrorCode::OutcomeUnknown)
        }
    });
    for command in commands {
        let _ = command.reply.send(result);
    }
    Ok(())
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

fn record_media_failure(
    core: &Core,
    account: &str,
    call_id: &str,
    worker: &Worker,
    error: &anyhow::Error,
) -> Result<()> {
    core.store.put_runtime(
        account,
        &format!("call-media-error:{call_id}"),
        b"media_failed",
    )?;
    // Persist only fixed categories and counters, never packet material or
    // dynamic transport errors, so a live failure can be diagnosed safely.
    let chain_has = |needle: &str| {
        error
            .chain()
            .any(|cause| cause.to_string().to_ascii_lowercase().contains(needle))
    };
    let code = if chain_has("peer activity timed out") {
        "peer_inactive"
    } else if chain_has("nonce") {
        "nonce_lifetime"
    } else if chain_has("generation") {
        "generation_changed"
    } else if !chain_has("stopped during relay setup")
        && !chain_has("relay handoff stopped")
        && chain_has("relay")
    {
        "relay_setup_failed"
    } else if chain_has("authenticated call control") {
        "unsupported_control"
    } else if chain_has("opus") {
        "opus_codec"
    } else if chain_has("receiver unavailable") {
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
    Ok(())
}

fn send_datagram(
    socket: &UdpSocket,
    endpoint: SocketAddr,
    packet: &[u8],
    worker: &Worker,
    audio: &[&QueuedAudio],
) -> Result<bool> {
    send_datagram_generation(socket, endpoint, packet, worker, audio, None)
}
fn send_datagram_generation(
    socket: &UdpSocket,
    endpoint: SocketAddr,
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
