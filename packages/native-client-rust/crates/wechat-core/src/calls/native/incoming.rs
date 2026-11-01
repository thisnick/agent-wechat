//! One source-bound callee actor. The broker chooses the Answer/Reject winner;
//! this actor serializes that intent with authenticated cancellation and sync.
use super::*;
use crate::calls::notifications::{
    IncomingCancellation, IncomingNotification, IncomingPreparation, IncomingRequest,
};
use wechat_protocol::calls::{
    client::{IncomingPhase, NativeIncomingSession},
    incoming::NotificationPayload,
};

type PreparationReply =
    Arc<Mutex<Option<oneshot::Sender<std::result::Result<IncomingPreparation, CallServiceError>>>>>;

pub(super) fn supported(inner: &Inner, authority: &AccountAuthority, caller: &str) -> bool {
    authority.account_id == inner.config.account_id
        && inner.config.tx_ready()
        && inner.config.allows_peer(caller)
        && !inner.test_spent.load(Ordering::Acquire)
        && inner.core.upgrade().is_some_and(|core| {
            core.cached_call_authority(&authority.account_id)
                .is_some_and(|lease| lease.current() && lease.authority() == authority)
        })
}

fn known_invitation(note: &IncomingNotification) -> bool {
    matches!(&note.control.payload, NotificationPayload::Invite(invite) if invite.unknown_fields.is_empty())
        && note.control.conference.unknown_fields.is_empty()
        && note
            .control
            .carrier
            .fields()
            .iter()
            .all(|field| (1..=7).contains(&field.number))
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
    let NativeTransmitScope::IsolatedTest {
        expires_at_unix_ms, ..
    } = inner.config.transmit_scope
    else {
        return Ok(IncomingPreparation::Ignored);
    };
    let now = unix_ms().map_err(|_| service_error(CallErrorCode::Stale))?;
    let Some(remaining) = expires_at_unix_ms.checked_sub(now) else {
        return Ok(IncomingPreparation::Ignored);
    };
    let NotificationPayload::Invite(invite) = &request.notification.control.payload else {
        return Ok(IncomingPreparation::Ignored);
    };
    let (Some(room), Some(room_key), Some(member)) =
        (invite.room_id, invite.room_key, invite.member_id)
    else {
        return Ok(IncomingPreparation::Ignored);
    };
    let (mailbox, commands) = Mailbox::new(room, room_key, member);
    let (audio, receive_audio) = mpsc::sync_channel(32);
    let worker = Arc::new(Worker {
        identity: request.identity.clone(),
        lease: request.notification.lease.clone(),
        stop: AtomicBool::new(false),
        media_stop: AtomicBool::new(false),
        generation: AtomicU32::new(1),
        submission: SendAdmission::default(),
        metrics: MediaMetrics::default(),
        scope_deadline: Instant::now() + Duration::from_millis(remaining.min(300_000)),
        scope_expires_unix_ms: expires_at_unix_ms,
        mutations_submitted: AtomicBool::new(false),
        incoming: Some(mailbox.clone()),
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
                old.done.lock().ok().and_then(|done| *done),
                Some(BackendOutcome::Ended)
            )
        }) {
            return Ok(IncomingPreparation::Ignored);
        }
        if !consume_test_intent(&inner.test_spent) {
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
                if core
                    .store
                    .finish_call(
                        &actor_inner.config.account_id,
                        actor_worker.identity.call_id(),
                        confirmed || no_mutation,
                    )
                    .is_err()
                {
                    confirmed = false;
                }
            }
            let terminal = confirmed || no_mutation;
            actor_worker.finish(if terminal {
                BackendOutcome::Ended
            } else {
                BackendOutcome::Uncertain
            });
            if let Some(core) = core {
                if confirmed {
                    let _ = core.call_service().verified_incoming_terminal(
                        actor_worker.lease.authority(),
                        &actor_worker.identity,
                    );
                } else if !terminal {
                    let _ = core
                        .call_service()
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

struct Actor {
    inner: Arc<Inner>,
    worker: Arc<Worker>,
    core: Arc<Core>,
    request: IncomingRequest,
    mailbox: Arc<Mailbox>,
    commands: Receiver<Command>,
    audio: Option<Receiver<QueuedAudio>>,
    prepared: PreparationReply,
    transport: FencedTransport,
    state: AccountState,
    authority: AppAuthority,
    tasks: Tasks,
    session: Option<NativeIncomingSession>,
    sink: Option<BackendSink>,
    media: Option<thread::JoinHandle<Result<()>>>,
    joined_deadline: Option<Instant>,
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
        prepared: PreparationReply,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self> {
        let core = inner.core.upgrade().context("Call core closed")?;
        let admitted = core.call_authority(&inner.config.account_id)?;
        ensure!(
            admitted.current()
                && admitted.authority() == worker.lease.authority()
                && request.guard.is_current(),
            "Incoming account admission revoked"
        );
        begin_operational_call(&inner, &worker, &core, &runtime)?;
        let state = core.account_state(&inner.config.account_id)?;
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
            account: inner.config.account_id.clone(),
            lease: worker.lease.clone(),
            call_id: worker.identity.call_id().into(),
            ordinal: AtomicU64::new(1),
            scope_deadline: worker.scope_deadline,
            scope_expires_unix_ms: worker.scope_expires_unix_ms,
            worker: worker.clone(),
            admission: ControlAdmission::Incoming(request.guard.clone()),
            runtime,
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
            prepared,
            transport,
            state,
            authority,
            tasks: Tasks::new()?,
            session: None,
            sink: None,
            media: None,
            joined_deadline: None,
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
        let offer = VoiceOffer::from_owned_random(
            self.state.device.client_version,
            crypto::random::<32>()?,
        )?;
        let mut parameters = self.inner.config.parameters.clone();
        offer.apply_to(&mut parameters);
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
        // Mode1 starts LiveSync in ACKed status3. Inspect the first whole batch
        // before publishing; ACKed polling continues while no winner exists.
        if self.poll()? {
            return self.finish_remote();
        }
        ensure!(
            reply_prepared(&self.prepared, IncomingPreparation::Acked),
            "Incoming preparation receiver closed"
        );
        loop {
            if self.drain_commands()? {
                return Ok(());
            }
            ensure!(
                self.worker.lease.current(),
                "Incoming account authority revoked"
            );
            if self.phase() == IncomingPhase::Joined {
                if self.worker.stop.load(Ordering::Acquire)
                    || !self.worker.scope_current()
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
            } else {
                // No controller owns an unanswered offer. Neither local expiry
                // nor disappearing listeners grants a user Reject operation.
                ensure!(
                    self.worker.scope_current()
                        && self.request.guard.is_current()
                        && !self.worker.stop.load(Ordering::Acquire),
                    "Unowned incoming admission expired"
                );
            }
            match self.poll() {
                Ok(true) => return self.finish_remote(),
                Ok(false) => {}
                Err(error) if error.is::<CallRpcNotSubmitted>() => {
                    if self.drain_commands()? {
                        return Ok(());
                    }
                    if self.phase() == IncomingPhase::Joined
                        && (self.worker.stop.load(Ordering::Acquire)
                            || !self.worker.scope_current())
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
    fn register_member(&self) -> Result<()> {
        let room = self
            .session
            .as_ref()
            .context("Missing incoming session")?
            .room();
        // Publish the already authenticated member before exposing it through
        // the broker. Delivery cannot keep an old snapshot past this point.
        self.mailbox.publish_member(room.own_member)?;
        self.core.call_service().set_incoming_member(
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
            if self.ignored_cancellations != ignored
                && self.worker.lease.current()
                && self.transport.admits(false, false)
            {
                // A queued Cancel briefly fenced this unsent poll, then the
                // source phase proved it inapplicable (for example after Join).
                // Its task remains spent; the next normal poll gets a new task.
                return Ok(false);
            }
        }
        Ok(matches!(result?, SyncProgress::Terminal(_)))
    }
    fn drain_commands(&mut self) -> Result<bool> {
        for _ in 0..8 {
            let command = match self.commands.try_recv() {
                Ok(command) => self.mailbox.next(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => bail!("Incoming actor mailbox closed"),
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
                && self.worker.lease.current()
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
                && request.media.audio
                && !request.media.video
                && self.worker.lease.current(),
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
                        && self.worker.scope_current()
                        && !self.worker.stop.load(Ordering::Acquire),
                    "Incoming Answer admission revoked"
                );
                self.transport.admission = ControlAdmission::Owner(sink.clone());
                self.sink = Some(sink);
                let result = self.session.as_mut().expect("incoming session").accept(
                    &self.transport,
                    &mut self.state,
                    self.authority,
                    self.tasks.next()?,
                );
                if let Err(error) = result {
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
                    let stale_binding = error
                        .downcast_ref::<CallServiceError>()
                        .is_some_and(|error| error.0 == CallErrorCode::Stale);
                    if stale_binding
                        && self.phase() == IncomingPhase::Joined
                        && self.worker.lease.current()
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
        let media = session.take_media(&self.state, self.authority, 0, 0)?;
        let audio = self
            .audio
            .take()
            .context("Incoming audio already consumed")?;
        let config = self.inner.config.clone();
        let worker = self.worker.clone();
        let runtime = self.transport.runtime.clone();
        self.media = Some(
            thread::Builder::new()
                .name("wechat-call-media".into())
                .spawn(move || {
                    let result = run_media(&config, &worker, media, audio, &sink, &runtime);
                    worker.media_stop.store(true, Ordering::Release);
                    result
                })?,
        );
        Ok(())
    }
    fn stop_media(&mut self) {
        self.worker.media_stop.store(true, Ordering::Release);
        if let Some(media) = self.media.take() {
            let failed = media.join().map_or(true, |result| result.is_err());
            if failed {
                let _ = self.core.store.put_runtime(
                    &self.inner.config.account_id,
                    &format!("call-media-error:{}", self.worker.identity.call_id()),
                    b"media_failed",
                );
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
    if worker.lease.authority() != request.ticket.authority()
        || !inner.config.allows_peer(&request.peer_id)
    {
        return Err(service_error(CallErrorCode::Stale));
    }
    let mailbox = worker
        .incoming
        .as_ref()
        .ok_or_else(|| service_error(CallErrorCode::Unsupported))?;
    let (send, receive) = oneshot::channel();
    mailbox
        .send
        .try_send(Command::Action {
            request: Box::new(request),
            sink,
            result: send,
        })
        .map_err(|_| service_error(CallErrorCode::Capacity))?;
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
        || !worker.lease.current()
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
