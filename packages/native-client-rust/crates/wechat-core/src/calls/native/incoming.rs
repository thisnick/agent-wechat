//! One source-bound callee actor. The broker chooses the Answer/Reject winner;
//! this actor serializes that intent with authenticated cancellation and sync.
use super::*;
use crate::calls::notifications::{
    IncomingCancellation, IncomingNotification, IncomingPreparation, IncomingRequest,
};
use std::sync::mpsc::RecvTimeoutError;
use wechat_calls::{
    native_media::{preconnect_relay, PreconnectedRelay},
    session::{IncomingPhase, NativeIncomingSession},
};
use wechat_protocol::calls::{incoming::NotificationPayload, relay::RelayEpoch};

type PreparationReply =
    Arc<Mutex<Option<oneshot::Sender<std::result::Result<IncomingPreparation, CallServiceError>>>>>;
const FIRST_ANSWER_WINDOW: Duration = Duration::from_secs(3);

pub(super) fn supported(inner: &Inner, authority: &AccountAuthority, caller: &str) -> bool {
    !caller.is_empty()
        && caller.len() <= 128
        && !caller.chars().any(char::is_control)
        && inner.core.upgrade().is_some_and(|core| {
            core.cached_call_authority(&authority.account_id)
                .is_some_and(|lease| lease.current() && lease.authority() == authority)
        })
}

fn known_invitation(note: &IncomingNotification) -> bool {
    // Only the selected conference bytes describe the admitted operation.
    // Opaque outer-carrier fields remain in the original notification; the
    // The verified ACK binds the room and our member before an offer is published.
    matches!(&note.control.payload, NotificationPayload::Invite(invite) if invite.unknown_fields.is_empty())
        && note.control.conference.unknown_fields.is_empty()
}

pub(super) async fn prepare(
    inner: Arc<Inner>,
    request: IncomingRequest,
) -> std::result::Result<IncomingPreparation, CallServiceError> {
    if !known_invitation(&request.notification)
        || !supported(
            &inner,
            &request.authority,
            &request.notification.caller_username,
        )
        || !request.guard.is_current()
        || !request.notification.lease.current()
        || request.notification.lease.authority() != &request.authority
        || Instant::now() >= request.expires
    {
        return Ok(IncomingPreparation::Ignored);
    }
    let Some(core) = inner.core.upgrade() else {
        return Ok(IncomingPreparation::Ignored);
    };
    if !request
        .notification
        .core
        .upgrade()
        .is_some_and(|source| Arc::ptr_eq(&source, &core))
        || !matches!(
            request.notification.control.payload,
            NotificationPayload::Invite(_)
        )
    {
        return Ok(IncomingPreparation::Ignored);
    }
    let NotificationPayload::Invite(invite) = &request.notification.control.payload else {
        return Ok(IncomingPreparation::Ignored);
    };
    let (Some(room), Some(room_key), Some(member)) =
        (invite.room_id, invite.room_key, invite.member_id)
    else {
        return Ok(IncomingPreparation::Ignored);
    };
    let is_video = invite.room_type == Some(1);
    let call_type = if is_video {
        CallType::Video
    } else {
        CallType::Voice
    };
    let (mailbox, commands) = Mailbox::new(room, room_key, member);
    let (audio, receive_audio) = mpsc::sync_channel(32);
    let (video, receive_video) = mpsc::sync_channel(8);
    let (video_keyframe_requests, keyframe_rx) = mpsc::sync_channel(8);
    let worker = Arc::new(Worker {
        identity: request.identity.clone(),
        peer_id: request.notification.caller_username.clone(),
        call_type,
        lease: request.notification.lease.clone(),
        account_lease: std::sync::OnceLock::new(),
        stop: AtomicBool::new(false),
        media_stop: AtomicBool::new(false),
        locally_closed: AtomicBool::new(false),
        generation: AtomicU32::new(1),
        video_generation: AtomicU32::new(1),
        muted: AtomicBool::new(false),
        camera_enabled: AtomicBool::new(false),
        initial_camera_off_pending: AtomicBool::new(false),
        initial_camera_off_deadlines: Mutex::new(None),
        rx_video_packets_seen: Arc::new(AtomicBool::new(false)),
        rx_video_keyframe_seen: Arc::new(AtomicBool::new(false)),
        joined_commands_ready: AtomicBool::new(false),
        submission: SendAdmission::default(),
        metrics: MediaMetrics::default(),
        mutations_submitted: AtomicBool::new(false),
        incoming: Some(mailbox.clone()),
        conference: None,
        conference_start: None,
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
            return Ok(IncomingPreparation::Ignored);
        }
        // Installation precedes profile proof and the first Auth wait. A Cancel
        // queued immediately after reservation therefore has an actor mailbox.
        *active = Some(worker.clone());
    }
    if !request.registration.mark_registered() {
        worker.finish(BackendOutcome::Ended);
        return Ok(IncomingPreparation::Ignored);
    }
    let (send, receive) = oneshot::channel();
    let prepared = Arc::new(Mutex::new(Some(send)));
    let runtime = tokio::runtime::Handle::current();
    let actor_inner = inner.clone();
    let actor_worker = worker.clone();
    if thread::Builder::new()
        .name("wechat-incoming-call".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Actor::new(
                    actor_inner.clone(),
                    actor_worker.clone(),
                    request,
                    mailbox,
                    commands,
                    receive_audio,
                    receive_video,
                    keyframe_rx,
                    prepared.clone(),
                    runtime,
                )?
                .run()
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Incoming call worker panicked")));
            actor_worker.media_stop.store(true, Ordering::Release);
            let no_mutation = !actor_worker.mutations_submitted.load(Ordering::Acquire);
            let core = actor_inner.core.upgrade();
            let mut confirmed = result.is_ok();
            if let Some(core) = &core {
                let retained = if no_mutation {
                    core.store.close_call_locally(
                        &actor_worker.lease.authority().account_id,
                        actor_worker.identity.call_id(),
                        wechat_store::CallLocalEndReason::NoRemoteMutation,
                    )
                } else {
                    core.store.finish_call(
                        &actor_worker.lease.authority().account_id,
                        actor_worker.identity.call_id(),
                        confirmed,
                    )
                };
                if retained.is_err() {
                    confirmed = false;
                }
            }
            let terminal = confirmed || no_mutation;
            actor_worker.finish(if terminal {
                BackendOutcome::Ended
            } else {
                BackendOutcome::Uncertain
            });
            let service = core.and_then(|core| {
                core.call_service(&actor_worker.lease.authority().account_id)
                    .ok()
            });
            if let Some(service) = service {
                if confirmed {
                    let _ = service.verified_incoming_terminal(
                        actor_worker.lease.authority(),
                        &actor_worker.identity,
                    );
                } else if !terminal {
                    let _ = service
                        .incoming_uncertain(actor_worker.lease.authority(), &actor_worker.identity);
                }
            }
            let initial = if confirmed {
                IncomingPreparation::Ended
            } else if terminal {
                IncomingPreparation::Ignored
            } else {
                IncomingPreparation::Uncertain
            };
            reply_prepared(&prepared, initial);
        })
        .is_err()
    {
        worker.finish(BackendOutcome::Ended);
        return Ok(IncomingPreparation::Ignored);
    }
    receive.await.unwrap_or(Ok(IncomingPreparation::Uncertain))
}

fn reply_prepared(prepared: &PreparationReply, outcome: IncomingPreparation) -> bool {
    prepared
        .lock()
        .ok()
        .and_then(|mut send| send.take())
        .is_some_and(|send| send.send(Ok(outcome)).is_ok())
}

/// Maximum time a ringing pre-connect UDP handshake may run before giving up
/// and letting post-`Join` media startup run its own relay check-in.
const PRECONNECT_HANDSHAKE_BUDGET: Duration = Duration::from_secs(4);
/// Maximum time `start_media` waits for an in-flight matching pre-connect
/// handshake before aborting it and letting the dedicated media worker connect.
const PRECONNECT_HANDOFF_WAIT: Duration = Duration::from_millis(250);

struct PreconnectHandle {
    target: wechat_protocol::calls::relay::RelayTarget,
    ready: Arc<AtomicBool>,
    answering: Arc<AtomicBool>,
    handoff: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    handle: thread::JoinHandle<Option<PreconnectedRelay>>,
}

struct Actor {
    inner: Arc<Inner>,
    worker: Arc<Worker>,
    core: Arc<Core>,
    request: IncomingRequest,
    mailbox: Arc<Mailbox>,
    commands: Receiver<Command>,
    audio: Option<Receiver<QueuedAudio>>,
    video: Option<Receiver<QueuedVideo>>,
    keyframe_rx: Option<Receiver<u32>>,
    prepared: PreparationReply,
    transport: FencedTransport,
    state: AccountState,
    authority: AppAuthority,
    tasks: Tasks,
    session: Option<NativeIncomingSession>,
    sink: Option<BackendSink>,
    preconnect: Option<PreconnectHandle>,
    media: Option<thread::JoinHandle<Result<()>>>,
    joined_deadline: Option<Instant>,
    last_av_ts: u64,
    ignored_cancellations: u64,
}
impl Actor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        inner: Arc<Inner>,
        worker: Arc<Worker>,
        request: IncomingRequest,
        mailbox: Arc<Mailbox>,
        commands: Receiver<Command>,
        audio: Receiver<QueuedAudio>,
        video: Receiver<QueuedVideo>,
        keyframe_rx: Receiver<u32>,
        prepared: PreparationReply,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self> {
        let core = inner.core.upgrade().context("Call core closed")?;
        let admitted = core.call_authority(&worker.lease.authority().account_id)?;
        ensure!(
            admitted.current()
                && admitted.authority() == worker.lease.authority()
                && request.guard.is_current(),
            "Incoming account admission revoked"
        );
        begin_operational_call(&worker, &core, &runtime)?;
        let state = core.account_state(&worker.lease.authority().account_id)?;
        ensure!(
            state.own_username == request.notification.own_username,
            "Incoming account identity changed"
        );
        let authority = AppAuthority {
            account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
            logout_epoch: 0,
            credential_digest: authorization(&state)?,
        };
        let transport = FencedTransport {
            core: inner.core.clone(),
            account: worker.lease.authority().account_id.clone(),
            lease: worker.lease.clone(),
            call_id: worker.identity.call_id().into(),
            ordinal: AtomicU64::new(1),
            worker: worker.clone(),
            admission: ControlAdmission::Incoming(request.guard.clone()),
            runtime,
            timeout: CALL_RPC_TIMEOUT,
            #[cfg(test)]
            test_io: inner.test_io.clone(),
        };
        Ok(Self {
            inner,
            worker,
            core,
            request,
            mailbox,
            commands,
            audio: Some(audio),
            video: Some(video),
            keyframe_rx: Some(keyframe_rx),
            prepared,
            transport,
            state,
            authority,
            tasks: Tasks::new()?,
            session: None,
            sink: None,
            preconnect: None,
            media: None,
            joined_deadline: None,
            last_av_ts: 0,
            ignored_cancellations: 0,
        })
    }
    fn run(mut self) -> Result<()> {
        if self.drain_commands()? {
            return Ok(());
        }
        let NotificationPayload::Invite(invite) = &self.request.notification.control.payload else {
            bail!("Incoming invitation unavailable")
        };
        let is_video = self.worker.call_type == CallType::Video;
        let (offer, mut parameters) = if is_video {
            (
                VoiceOffer::from_owned_random_video(
                    self.state.device.client_version,
                    crypto::random::<32>()?,
                    VideoOfferOptions::default(),
                )?,
                independent_video_parameters(false)?,
            )
        } else {
            (
                VoiceOffer::from_owned_random(
                    self.state.device.client_version,
                    crypto::random::<32>()?,
                )?,
                independent_voice_parameters()?,
            )
        };
        parameters.apply_offer(&offer);
        let session = NativeIncomingSession::authenticate(
            &self.transport,
            &mut self.state,
            self.authority,
            self.tasks.host,
            crypto::random::<32>()?,
            parameters,
            invite,
            &self.request.notification.caller_username,
        );
        match session {
            Ok(session) => self.session = Some(session),
            Err(error) => {
                if self.drain_commands()? {
                    return Ok(());
                }
                return Err(error);
            }
        }
        if self.drain_commands()? {
            return Ok(());
        }
        let ack = self
            .session
            .as_mut()
            .expect("incoming session")
            .ringing_ack(
                &self.transport,
                &mut self.state,
                self.authority,
                self.tasks.next()?,
            );
        if let Err(error) = ack {
            if error.is::<CallRpcNotSubmitted>() && self.drain_commands()? {
                return Ok(());
            }
            return Err(error);
        }
        if self.phase() == IncomingPhase::Finished {
            return Ok(());
        }
        self.register_member()?;
        if self.drain_commands()? {
            return Ok(());
        }
        ensure!(
            reply_prepared(&self.prepared, IncomingPreparation::Acked),
            "Incoming preparation receiver closed"
        );
        self.start_preconnect();
        // Give the offered client a chance to answer before a blocking poll
        // starts. The actor still serializes Join, cancellation and LiveSync.
        let wait = FIRST_ANSWER_WINDOW.min(
            self.request
                .expires
                .saturating_duration_since(Instant::now()),
        );
        if self.wait_for_commands(wait)? {
            return Ok(());
        }
        let mut first_poll = true;
        loop {
            if self.drain_commands()? {
                return Ok(());
            }
            if self.awaiting_reject() {
                // Broker admission precedes asynchronous backend delivery.
                // Keep the actor, but do not retry its already fenced poll.
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            ensure!(
                self.worker.authorized(),
                "Incoming account authority revoked"
            );
            if self.phase() == IncomingPhase::Joined {
                if self.local_stop_requested()
                    || !self.worker.authorized()
                    || self
                        .joined_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return self.exit();
                }
                if let Err(error) = self.start_media() {
                    if error.is::<UnsupportedCallMedia>() {
                        return self.exit();
                    }
                    return Err(error);
                }
                if self.media.is_some() && self.worker.media_stop.load(Ordering::Acquire) {
                    return self.exit();
                }
                if self.worker.call_type == CallType::Video {
                    self.worker
                        .joined_commands_ready
                        .store(true, Ordering::Release);
                    let transport = &self.transport;
                    let state = &mut self.state;
                    let authority = self.authority;
                    let tasks = &mut self.tasks;
                    let session = self.session.as_mut().expect("incoming session");
                    let worker = &self.worker;
                    let last_av_ts = &mut self.last_av_ts;
                    drain_direct_switch_av(worker, &mut |cam| {
                        let mic = !worker.muted.load(Ordering::Acquire);
                        let now = u64::try_from(
                            SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros(),
                        )?;
                        *last_av_ts = last_av_ts.saturating_add(1).max(now);
                        session.switch_av(
                            transport,
                            state,
                            authority,
                            mic,
                            cam,
                            *last_av_ts,
                            tasks.next()?,
                        )
                    })?;
                }
            } else {
                // No controller owns an unanswered offer. Neither local expiry
                // nor disappearing listeners grants a user Reject operation.
                ensure!(
                    self.worker.authorized()
                        && self.request.guard.is_current()
                        && !self.worker.stop.load(Ordering::Acquire),
                    "Unowned incoming admission expired"
                );
            }
            let poll = if first_poll && self.phase() == IncomingPhase::Acknowledged {
                let timeout =
                    std::mem::replace(&mut self.transport.timeout, Duration::from_secs(60));
                let result = self.poll();
                self.transport.timeout = timeout;
                result
            } else {
                self.poll()
            };
            first_poll = false;
            match poll {
                Ok(true) => return self.finish_remote(),
                Ok(false) => {}
                Err(error) if error.is::<CallLiveSyncInterrupted>() => continue,
                Err(error) if error.is::<CallRpcNotSubmitted>() => {
                    if self.drain_commands()? {
                        return Ok(());
                    }
                    if self.phase() == IncomingPhase::Joined
                        && (self.local_stop_requested() || !self.worker.authorized())
                    {
                        return self.exit();
                    }
                    return Err(error);
                }
                Err(error)
                    if error.is::<UnsupportedCallMedia>()
                        && self.phase() == IncomingPhase::Joined =>
                {
                    return self.exit()
                }
                Err(error) => return Err(error),
            }
            // A malformed peer must not turn a zero-timeout server into a busy
            // loop; this also bounds local command latency between polls.
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn phase(&self) -> IncomingPhase {
        self.session
            .as_ref()
            .map_or(IncomingPhase::Invited, NativeIncomingSession::phase)
    }
    fn local_stop_requested(&self) -> bool {
        self.worker.stop.load(Ordering::Acquire)
            || self
                .sink
                .as_ref()
                .is_some_and(BackendSink::has_accepted_end)
    }
    fn awaiting_reject(&self) -> bool {
        self.phase() == IncomingPhase::Acknowledged
            && self.worker.authorized()
            && self.worker.authorized()
            && !self.worker.stop.load(Ordering::Acquire)
            && !self.mailbox.cancellation_pending()
            && self
                .core
                .call_service(&self.worker.lease.authority().account_id)
                .is_ok_and(|service| {
                    service.pending_incoming_reject(
                        self.worker.lease.authority(),
                        &self.worker.identity,
                    )
                })
    }
    fn register_member(&self) -> Result<()> {
        let room = self
            .session
            .as_ref()
            .context("Missing incoming session")?
            .room();
        // Publish the already authenticated member before exposing it through
        // the broker. Delivery cannot keep an old snapshot past this point.
        self.mailbox.publish_member(room.own_member)?;
        self.core
            .call_service(&self.worker.lease.authority().account_id)?
            .set_incoming_member(
                self.worker.lease.authority(),
                &self.worker.identity,
                room.room,
                room.room_key,
                room.own_member,
            )?;
        Ok(())
    }
    fn poll(&mut self) -> Result<bool> {
        let ignored = self.ignored_cancellations;
        let result = self
            .session
            .as_mut()
            .context("Missing incoming session")?
            .poll(
                &self.transport,
                &mut self.state,
                self.authority,
                self.tasks.next()?,
            );
        if result
            .as_ref()
            .is_err_and(|error| error.is::<CallRpcNotSubmitted>())
        {
            if self.drain_commands()? {
                return Ok(true);
            }
            if self.awaiting_reject() {
                return Ok(false);
            }
            if self.ignored_cancellations != ignored
                && self.worker.authorized()
                && self.transport.admits(false, false)
            {
                // A queued Cancel briefly fenced this unsent poll, then the
                // source phase proved it inapplicable (for example after Join).
                // Its task remains spent; the next normal poll gets a new task.
                return Ok(false);
            }
        }
        let progress = result?;
        if matches!(progress, SyncProgress::MediaReady) {
            if let (Some(sink), Some(muted)) = (
                self.sink.as_ref(),
                self.session
                    .as_ref()
                    .and_then(|session| session.peer_microphone_muted()),
            ) {
                let _ = self.transport.runtime.block_on(sink.peer_microphone(muted));
            }
            if let (Some(sink), Some(camera)) = (
                self.sink.as_ref(),
                self.session
                    .as_ref()
                    .and_then(|session| session.peer_camera_enabled()),
            ) {
                let _ = self.transport.runtime.block_on(sink.peer_camera(camera));
            }
        }
        Ok(matches!(progress, SyncProgress::Terminal(_)))
    }
    fn drain_commands(&mut self) -> Result<bool> {
        self.wait_for_commands(Duration::ZERO)
    }
    fn wait_for_commands(&mut self, wait: Duration) -> Result<bool> {
        for index in 0..8 {
            let command =
                match self
                    .commands
                    .recv_timeout(if index == 0 { wait } else { Duration::ZERO })
                {
                    Ok(command) => self.mailbox.next(command),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => bail!("Incoming actor mailbox closed"),
                };
            match command {
                Command::Cancel { note, result, .. } => {
                    let verdict = self.cancel_note(&note);
                    let ended = matches!(verdict, Ok(IncomingCancellation::Ended));
                    if matches!(verdict, Ok(IncomingCancellation::Ignored)) {
                        self.ignored_cancellations = self
                            .ignored_cancellations
                            .checked_add(1)
                            .context("Incoming cancellation counter exhausted")?;
                    }
                    let uncertain =
                        verdict.is_err() || matches!(verdict, Ok(IncomingCancellation::Uncertain));
                    let _ = result
                        .send(verdict.map_err(|_| service_error(CallErrorCode::OutcomeUnknown)));
                    if uncertain {
                        bail!("Incoming cancellation state uncertain");
                    }
                    if ended {
                        return Ok(true);
                    }
                }
                Command::Action {
                    request,
                    sink,
                    result,
                } => {
                    let operation = request.ticket.operation();
                    let verdict = self.perform_action(&request, sink);
                    let failed = verdict.is_err();
                    let ended = matches!(verdict, Ok(BackendOutcome::Ended));
                    let _ = result
                        .send(verdict.map_err(|_| service_error(CallErrorCode::OutcomeUnknown)));
                    if failed {
                        bail!("Incoming owner action unresolved");
                    }
                    if ended {
                        return Ok(true);
                    }
                    ensure!(
                        operation == Operation::Answer,
                        "Unexpected incoming owner action"
                    );
                }
            }
        }
        Ok(self.phase() == IncomingPhase::Finished)
    }
    fn cancel_note(&mut self, note: &IncomingNotification) -> Result<IncomingCancellation> {
        ensure!(
            note.lease.current()
                && self.worker.authorized()
                && note.lease.authority() == self.worker.lease.authority()
                && note.own_username == self.request.notification.own_username
                && note.header_from == self.request.notification.header_from
                && note
                    .core
                    .upgrade()
                    .is_some_and(|core| Arc::ptr_eq(&core, &self.core)),
            "Incoming cancellation authority mismatch"
        );
        ensure!(
            note.control.conference.unknown_fields.is_empty()
                && note
                    .control
                    .carrier
                    .fields()
                    .iter()
                    .all(|field| (1..=2).contains(&field.number)),
            "Unknown incoming cancellation carrier fields"
        );
        let NotificationPayload::Cancel(cancel) = &note.control.payload else {
            bail!("Incoming cancellation payload mismatch")
        };
        let ended = if let Some(session) = self.session.as_mut() {
            session.retire_cancel(&self.state, self.authority, cancel)?
        } else {
            let NotificationPayload::Invite(invite) = &self.request.notification.control.payload
            else {
                bail!("Missing original invitation")
            };
            ensure!(
                cancel.unknown_fields.is_empty(),
                "Unknown cancellation fields"
            );
            cancel.room_id == invite.room_id
                && cancel.room_key == invite.room_key
                && cancel.member_id == invite.member_id
        };
        if ended {
            self.abort_preconnect();
            self.worker.media_stop.store(true, Ordering::Release);
            Ok(IncomingCancellation::Ended)
        } else {
            Ok(IncomingCancellation::Ignored)
        }
    }
    fn perform_action(
        &mut self,
        request: &BackendRequest,
        sink: BackendSink,
    ) -> Result<BackendOutcome> {
        ensure!(
            request.ticket.identity() == self.worker.identity
                && request.ticket.authority() == self.worker.lease.authority()
                && request.peer_id == self.request.notification.caller_username
                && request.call_type == self.worker.call_type
                && self.worker.authorized(),
            "Incoming winner identity mismatch"
        );
        ensure!(
            self.phase() == IncomingPhase::Acknowledged,
            "Incoming owner operation in wrong source phase"
        );
        match request.ticket.operation() {
            Operation::Answer => {
                ensure!(
                    sink.is_current()
                        && self.worker.authorized()
                        && !self.worker.stop.load(Ordering::Acquire),
                    "Incoming Answer admission revoked"
                );
                self.worker.muted.store(request.muted, Ordering::Release);
                self.worker
                    .camera_enabled
                    .store(request.camera_enabled, Ordering::Release);
                let join_camera = if self.worker.call_type == CallType::Video {
                    if !request.camera_enabled {
                        // Direct video rooms must join with the video status bit set so the
                        // relay forwards initial keyframe requests and the caller enters
                        // normal video recovery; SwitchAV clears the camera bit once media
                        // starts flowing or after the startup grace window.
                        self.worker.schedule_initial_camera_off();
                    }
                    true
                } else {
                    request.camera_enabled
                };
                self.transport.admission = ControlAdmission::Owner(sink.clone());
                self.sink = Some(sink);
                // Answer retires the pre-answer offer guard while Join is in flight;
                // keep the pre-connected relay socket alive and draining seq=0 packets
                // until start_media claims it or Join fails.
                if let Some(pre) = self.preconnect.as_ref() {
                    pre.answering.store(true, Ordering::Release);
                }
                let result = self
                    .session
                    .as_mut()
                    .expect("incoming session")
                    .accept_with_camera(
                        &self.transport,
                        &mut self.state,
                        self.authority,
                        join_camera,
                        self.tasks.next()?,
                    );
                if let Err(error) = result {
                    self.abort_preconnect();
                    if error.is::<UnsupportedCallMedia>() && self.phase() == IncomingPhase::Joined {
                        self.exit()?;
                        return Ok(BackendOutcome::Ended);
                    }
                    if error.is::<CallRpcNotSubmitted>() && self.drain_commands()? {
                        return Ok(BackendOutcome::Ended);
                    }
                    return Err(error);
                }
                if let Err(error) = self.register_member() {
                    self.abort_preconnect();
                    let stale_binding = error
                        .downcast_ref::<CallServiceError>()
                        .is_some_and(|error| error.0 == CallErrorCode::Stale);
                    if stale_binding
                        && self.phase() == IncomingPhase::Joined
                        && self.worker.authorized()
                        && self
                            .sink
                            .as_ref()
                            .is_some_and(BackendSink::lost_incoming_answer_owner)
                    {
                        // Join is already accepted, but its original controller
                        // disappeared before any media was admitted. Clean up
                        // once instead of reviving it through the normal owner
                        // grace period. Established calls keep their own grace.
                        self.worker.media_stop.store(true, Ordering::Release);
                        self.exit()?;
                        return Ok(BackendOutcome::Ended);
                    }
                    return Err(error);
                }
                self.joined_deadline = Some(Instant::now() + self.inner.config.max_duration);
                Ok(BackendOutcome::Admitted)
            }
            Operation::Reject => {
                self.abort_preconnect();
                ensure!(
                    sink.is_terminal_current(),
                    "Incoming Reject admission revoked"
                );
                self.transport.admission = ControlAdmission::Terminal(sink);
                let result = self.session.as_mut().expect("incoming session").reject(
                    &self.transport,
                    &mut self.state,
                    self.authority,
                    self.tasks.next()?,
                );
                if let Err(error) = result {
                    if error.is::<CallRpcNotSubmitted>() && self.drain_commands()? {
                        return Ok(BackendOutcome::Ended);
                    }
                    return Err(error);
                }
                Ok(BackendOutcome::Ended)
            }
            _ => bail!("Unsupported incoming owner operation"),
        }
    }
    fn start_preconnect(&mut self) {
        if self.preconnect.is_some() || self.phase() != IncomingPhase::Acknowledged {
            return;
        }
        let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let candidates = match session.ringing_relay_candidates() {
            Ok(candidates) => candidates,
            Err(error) => {
                if video_trace {
                    eprintln!(
                        "[wechat-call-video] incoming relay preconnect candidates err: {error:#}"
                    );
                }
                return;
            }
        };
        if candidates.is_empty() {
            if video_trace {
                eprintln!(
                    "[wechat-call-video] incoming relay preconnect skipped: 0 ack candidates"
                );
            }
            return;
        }
        let (connection_index, udp_index) = super::media::select_relay_indices(&candidates);
        let target = match session.ringing_relay_target(
            &self.state,
            self.authority,
            connection_index,
            udp_index,
        ) {
            Ok(Some(target)) => target,
            Ok(None) => {
                if video_trace {
                    eprintln!(
                        "[wechat-call-video] incoming relay preconnect skipped: target unavailable (candidates={})",
                        candidates.len()
                    );
                }
                return;
            }
            Err(error) => {
                if video_trace {
                    eprintln!(
                        "[wechat-call-video] incoming relay preconnect target err: {error:#}"
                    );
                }
                return;
            }
        };
        let worker = self.worker.clone();
        let guard = self.request.guard.clone();
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
        let Ok(handle) = thread::Builder::new()
            .name("wechat-call-preconnect".into())
            .spawn(move || -> Option<PreconnectedRelay> {
                let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
                let started = Instant::now();
                let can_continue = || {
                    !thread_abort.load(Ordering::Acquire)
                        && worker.current()
                        && !worker.stop.load(Ordering::Acquire)
                        && (thread_answering.load(Ordering::Acquire)
                            || thread_handoff.load(Ordering::Acquire)
                            || guard.is_current())
                };
                let mut ready = match preconnect_relay(
                    &thread_target,
                    relay_epoch,
                    started,
                    || can_continue() && started.elapsed() < PRECONNECT_HANDSHAKE_BUDGET,
                    |socket, endpoint, packet| {
                        ensure!(can_continue(), "Incoming relay pre-connect stopped");
                        ensure!(
                            send_datagram(socket, endpoint, packet, &worker, &[])?,
                            "Incoming relay pre-connect handoff stopped"
                        );
                        Ok(())
                    },
                ) {
                    Ok(ready) => ready,
                    Err(error) => {
                        if video_trace {
                            eprintln!(
                                "[wechat-call-video] incoming relay preconnect skipped/failed endpoint={}: {error:#}",
                                thread_target.endpoint
                            );
                        }
                        return None;
                    }
                };
                thread_ready.store(true, Ordering::Release);
                if video_trace {
                    eprintln!(
                        "[wechat-call-video] incoming relay preconnected endpoint={} conn={} elapsed_ms={} early_pkts={}",
                        ready.endpoint(),
                        ready.connection_id(),
                        started.elapsed().as_millis(),
                        ready.early_packet_count()
                    );
                }
                while !thread_handoff.load(Ordering::Acquire) {
                    if !can_continue() {
                        return None;
                    }
                    ready.drain_early_packets();
                    thread::sleep(Duration::from_millis(5));
                }
                if thread_abort.load(Ordering::Acquire)
                    || !worker.current()
                    || worker.stop.load(Ordering::Acquire)
                {
                    return None;
                }
                ready.drain_early_packets();
                Some(ready)
            })
        else {
            return;
        };
        self.preconnect = Some(PreconnectHandle {
            target,
            ready: ready_flag,
            answering,
            handoff,
            abort,
            handle,
        });
    }

    fn take_matching_preconnected_relay(
        &mut self,
        candidates: &[(usize, usize, std::net::SocketAddr)],
        own_member: u16,
    ) -> Option<PreconnectedRelay> {
        let pre = self.preconnect.take()?;
        let endpoint_matches = candidates
            .iter()
            .any(|&(_, _, ep)| ep == pre.target.endpoint);
        if pre.target.own_member != own_member || !endpoint_matches {
            pre.abort.store(true, Ordering::Release);
            let _ = pre.handle.join();
            return None;
        }
        pre.answering.store(true, Ordering::Release);
        pre.handoff.store(true, Ordering::Release);
        let wait_started = Instant::now();
        while !pre.ready.load(Ordering::Acquire) && !pre.handle.is_finished() {
            if wait_started.elapsed() >= PRECONNECT_HANDOFF_WAIT {
                pre.abort.store(true, Ordering::Release);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        pre.handle.join().ok().flatten()
    }

    fn abort_preconnect(&mut self) {
        if let Some(pre) = self.preconnect.take() {
            pre.abort.store(true, Ordering::Release);
            let _ = pre.handle.join();
        }
    }

    fn start_media(&mut self) -> Result<()> {
        if self.media.is_some() {
            return Ok(());
        }
        let session = self.session.as_mut().context("Missing incoming session")?;
        if !session.media_ready(&self.state, self.authority)? {
            return Ok(());
        }
        let sink = self.sink.clone().context("Incoming owner missing")?;
        ensure!(
            sink.is_current() && self.worker.current(),
            "Incoming media admission revoked"
        );
        let (candidates, own_member) = {
            let session = self.session.as_ref().context("Missing incoming session")?;
            (
                session.relay_candidates()?,
                session.room().own_member as u16,
            )
        };
        let preconnected = self.take_matching_preconnected_relay(&candidates, own_member);
        let (connection_index, udp_index) = preconnected
            .as_ref()
            .and_then(|ready| {
                candidates
                    .iter()
                    .find(|&&(_, _, ep)| ep == ready.endpoint())
                    .map(|&(c, u, _)| (c, u))
            })
            .unwrap_or_else(|| super::media::select_relay_indices(&candidates));
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            let listed: Vec<String> = candidates
                .iter()
                .map(|(c, u, ep)| format!("{c}/{u}={ep}"))
                .collect();
            eprintln!(
                "[wechat-call-video] join relay candidates={} selected={connection_index}/{udp_index} preconnected={} list=[{}]",
                candidates.len(),
                preconnected.is_some(),
                listed.join(",")
            );
        }
        let session = self.session.as_mut().context("Missing incoming session")?;
        let media = session.take_media(&self.state, self.authority, connection_index, udp_index)?;
        if let Some(muted) = session.peer_microphone_muted() {
            let _ = self.transport.runtime.block_on(sink.peer_microphone(muted));
        }
        if let Some(camera) = session.peer_camera_enabled() {
            let _ = self.transport.runtime.block_on(sink.peer_camera(camera));
        }
        let audio = self
            .audio
            .take()
            .context("Incoming audio already consumed")?;
        let video = self.video.take();
        let keyframe_rx = self
            .keyframe_rx
            .take()
            .context("Incoming keyframe receiver already consumed")?;
        let is_video = self.worker.call_type == CallType::Video;
        if is_video {
            self.worker.arm_initial_camera_off(
                self.inner.config.direct_camera_off_grace,
                self.inner.config.direct_camera_off_recovery_grace,
            );
        }
        let config = self.inner.config.clone();
        let worker = self.worker.clone();
        let runtime = self.transport.runtime.clone();
        self.media = Some(
            thread::Builder::new()
                .name("wechat-call-media".into())
                .spawn(move || {
                    let result = super::media::run_with_preconnected_relay(
                        &config,
                        &worker,
                        media,
                        audio,
                        is_video.then_some(video).flatten(),
                        keyframe_rx,
                        preconnected,
                        &sink,
                        &runtime,
                    );
                    worker.media_stop.store(true, Ordering::Release);
                    result
                })?,
        );
        Ok(())
    }
    fn stop_media(&mut self) {
        self.abort_preconnect();
        if let Some(sink) = &self.sink {
            let _ = self.transport.runtime.block_on(sink.ending());
        }
        self.worker.media_stop.store(true, Ordering::Release);
        if let Some(media) = self.media.take() {
            match media.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = record_media_failure(
                        &self.core,
                        &self.worker.lease.authority().account_id,
                        self.worker.identity.call_id(),
                        &self.worker,
                        &error,
                    );
                }
                Err(_) => {
                    let _ = self.core.store.put_runtime(
                        &self.worker.lease.authority().account_id,
                        &format!("call-media-error:{}", self.worker.identity.call_id()),
                        b"media_failed",
                    );
                }
            }
        }
    }
    fn finish_remote(&mut self) -> Result<()> {
        self.stop_media();
        match self.phase() {
            IncomingPhase::Finished => Ok(()),
            IncomingPhase::TerminalPending => self
                .session
                .as_mut()
                .expect("incoming session")
                .finish_remote(
                    &self.transport,
                    &mut self.state,
                    self.authority,
                    self.tasks.next()?,
                    false,
                ),
            _ => bail!("Incoming terminal context unavailable"),
        }
    }
    fn exit(&mut self) -> Result<()> {
        self.stop_media();
        self.session
            .as_mut()
            .context("Missing incoming session")?
            .exit(
                &self.transport,
                &mut self.state,
                self.authority,
                self.tasks.next()?,
                false,
            )
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.stop_media();
    }
}

pub(super) enum Command {
    Action {
        request: Box<BackendRequest>,
        sink: BackendSink,
        result: oneshot::Sender<std::result::Result<BackendOutcome, CallServiceError>>,
    },
    Cancel {
        note: Arc<IncomingNotification>,
        fenced: bool,
        result: oneshot::Sender<std::result::Result<IncomingCancellation, CallServiceError>>,
    },
}
pub(super) struct Mailbox {
    send: SyncSender<Command>,
    pub(super) answer_queued: Notify,
    pending_cancellations: AtomicU32,
    room: u64,
    room_key: u64,
    member: AtomicU32,
    delivery: Mutex<()>,
}
impl Mailbox {
    fn new(room: u64, room_key: u64, member: u32) -> (Arc<Self>, Receiver<Command>) {
        let (send, receive) = mpsc::sync_channel(8);
        (
            Arc::new(Self {
                send,
                answer_queued: Notify::new(),
                pending_cancellations: AtomicU32::new(0),
                room,
                room_key,
                member: AtomicU32::new(member),
                delivery: Mutex::new(()),
            }),
            receive,
        )
    }
    pub(super) fn cancellation_pending(&self) -> bool {
        self.pending_cancellations.load(Ordering::Acquire) != 0
    }
    fn publish_member(&self, member: u32) -> Result<()> {
        let _delivery = self
            .delivery
            .lock()
            .map_err(|_| anyhow::anyhow!("Incoming mailbox closed"))?;
        self.member.store(member, Ordering::Release);
        Ok(())
    }
    fn next(&self, command: Command) -> Command {
        if matches!(&command, Command::Cancel { fenced: true, .. }) {
            self.pending_cancellations.fetch_sub(1, Ordering::AcqRel);
        }
        command
    }
    fn fences_submission(&self, note: &IncomingNotification) -> bool {
        let NotificationPayload::Cancel(cancel) = &note.control.payload else {
            return true;
        };
        if !cancel.unknown_fields.is_empty()
            || !note.control.conference.unknown_fields.is_empty()
            || note
                .control
                .carrier
                .fields()
                .iter()
                .any(|field| !(1..=2).contains(&field.number))
        {
            return true;
        }
        // A future/foreign member is delivered for classification after an
        // already returned ACK/Join, but cannot interrupt a still-unsent action.
        cancel.room_id == Some(self.room)
            && cancel.room_key == Some(self.room_key)
            && cancel.member_id == Some(self.member.load(Ordering::Acquire))
    }
}

pub(super) async fn action(
    inner: Arc<Inner>,
    request: BackendRequest,
    sink: BackendSink,
) -> std::result::Result<BackendOutcome, CallServiceError> {
    let worker = active(&inner, &request.ticket.identity())?;
    if worker.lease.authority() != request.ticket.authority() || worker.peer_id != request.peer_id {
        return Err(service_error(CallErrorCode::Stale));
    }
    let mailbox = worker
        .incoming
        .as_ref()
        .ok_or_else(|| service_error(CallErrorCode::Unsupported))?;
    let (send, receive) = oneshot::channel();
    let answering = request.ticket.operation() == Operation::Answer;
    mailbox
        .send
        .try_send(Command::Action {
            request: Box::new(request),
            sink,
            result: send,
        })
        .map_err(|_| service_error(CallErrorCode::Capacity))?;
    if answering {
        mailbox.answer_queued.notify_one();
    }
    receive
        .await
        .unwrap_or(Err(service_error(CallErrorCode::OutcomeUnknown)))
}
pub(super) async fn cancel(
    inner: Arc<Inner>,
    identity: CallIdentity,
    note: Arc<IncomingNotification>,
) -> std::result::Result<IncomingCancellation, CallServiceError> {
    let worker = active(&inner, &identity)?;
    if worker.lease.authority() != note.lease.authority()
        || !worker.authorized()
        || !note.lease.current()
    {
        return Ok(IncomingCancellation::Uncertain);
    }
    let Some(mailbox) = worker.incoming.as_ref() else {
        return Ok(IncomingCancellation::Ignored);
    };
    let (send, receive) = oneshot::channel();
    {
        // Complete a member snapshot and nonblocking delivery together. A
        // snapshot taken before ACK/Join publication must reach its drain;
        // it cannot enqueue afterward with a stale non-fencing decision.
        let _delivery = mailbox
            .delivery
            .lock()
            .map_err(|_| service_error(CallErrorCode::Closed))?;
        let fenced = mailbox.fences_submission(&note);
        if fenced {
            mailbox.pending_cancellations.fetch_add(1, Ordering::AcqRel);
        }
        if mailbox
            .send
            .try_send(Command::Cancel {
                note,
                fenced,
                result: send,
            })
            .is_err()
        {
            if fenced {
                mailbox.pending_cancellations.fetch_sub(1, Ordering::AcqRel);
            }
            return Err(service_error(CallErrorCode::Capacity));
        }
    }
    match receive.await {
        Ok(outcome) => outcome,
        // A successful terminal action may finish before draining this note.
        // Mailbox closure alone proves nothing; wait for this exact worker's
        // final result before treating a leftover control as a no-op.
        Err(_) => Ok(match worker.wait().await {
            Ok(BackendOutcome::Ended) => IncomingCancellation::Ignored,
            _ => IncomingCancellation::Uncertain,
        }),
    }
}

#[cfg(test)]
mod tests;
