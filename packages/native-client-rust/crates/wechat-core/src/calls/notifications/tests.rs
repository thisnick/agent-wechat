use super::*;
use crate::{
    call_types::*,
    calls::service::{
        BackendAudioRequest, BackendOutcome, BackendRequest, BackendSink, CallBackend,
        CallServiceError, ControlConnection,
    },
    Core, Store,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::Digest;
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{mpsc, oneshot};
use wechat_protocol::{protobuf as p, wire::SyncCommand, AccountState, DeviceProfile};

type FutureResult<T> = Pin<Box<dyn Future<Output = Result<T, CallServiceError>> + Send>>;
struct Attempt {
    request: IncomingRequest,
    reply: oneshot::Sender<IncomingPreparation>,
}
struct Cancellation {
    identity: CallIdentity,
    note: Arc<IncomingNotification>,
    reply: oneshot::Sender<Result<IncomingCancellation, CallServiceError>>,
}
struct Backend {
    core: Weak<Core>,
    attempts: mpsc::UnboundedSender<Attempt>,
    prepared: AtomicUsize,
    cancellations: mpsc::UnboundedSender<Cancellation>,
    cancelled: AtomicUsize,
    stopped: AtomicUsize,
    audio: AtomicUsize,
    registration_mode: AtomicUsize,
    actors: std::sync::Mutex<Vec<CallIdentity>>,
    last_registration: std::sync::Mutex<Option<IncomingRegistration>>,
}
impl Backend {
    fn install(&self, request: &IncomingRequest) -> bool {
        self.actors.lock().unwrap().push(request.identity.clone());
        request.registration.mark_registered()
    }
}
impl CallBackend for Backend {
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        Some(
            self.core
                .upgrade()?
                .cached_call_authority(account)?
                .authority()
                .clone(),
        )
    }
    fn capabilities(&self, _: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: true,
            ..Default::default()
        }
    }
    fn incoming_supported(&self, _: &AccountAuthority, caller: &str) -> bool {
        caller == "synthetic_peer"
    }
    fn prepare_incoming(&self, request: IncomingRequest) -> FutureResult<IncomingPreparation> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        *self.last_registration.lock().unwrap() = Some(request.registration.clone());
        match self.registration_mode.load(Ordering::SeqCst) {
            1 => return Box::pin(async { Ok(IncomingPreparation::Ignored) }),
            2 => return Box::pin(async { Ok(IncomingPreparation::Ended) }),
            3 => {} // A constructed future is deliberately not installation.
            _ => {
                if !self.install(&request) {
                    return Box::pin(async { Ok(IncomingPreparation::Ignored) });
                }
            }
        }
        let (reply, result) = oneshot::channel();
        self.attempts
            .send(Attempt { request, reply })
            .unwrap_or_else(|_| panic!("fixture receiver"));
        Box::pin(async move { Ok(result.await.unwrap_or(IncomingPreparation::Uncertain)) })
    }
    fn cancel_incoming(
        &self,
        identity: CallIdentity,
        note: Arc<IncomingNotification>,
    ) -> FutureResult<IncomingCancellation> {
        assert!(
            self.actors.lock().unwrap().contains(&identity),
            "cancellation preceded mailbox installation"
        );
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        let (reply, received) = oneshot::channel();
        self.cancellations
            .send(Cancellation {
                identity,
                note,
                reply,
            })
            .unwrap_or_else(|_| panic!("fixture cancellation receiver"));
        Box::pin(async move {
            received
                .await
                .unwrap_or(Ok(IncomingCancellation::Uncertain))
        })
    }
    fn submit(&self, _: BackendRequest, _: BackendSink) -> FutureResult<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Connected) })
    }
    fn send_audio(&self, request: BackendAudioRequest) -> FutureResult<()> {
        if !request.is_current() {
            return Box::pin(async { Err(CallServiceError(CallErrorCode::Stale)) });
        }
        self.audio.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn stop(&self, _: CallIdentity) -> FutureResult<BackendOutcome> {
        self.stopped.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(BackendOutcome::Uncertain) })
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<Backend>,
    attempts: mpsc::UnboundedReceiver<Attempt>,
    cancellations: mpsc::UnboundedReceiver<Cancellation>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        let state = AccountState {
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
        };
        core.save_state("one", &state).unwrap();
        assert!(core.call_authority("one").is_ok());
        let (attempts, receiver) = mpsc::unbounded_channel();
        let (cancellations, cancelled) = mpsc::unbounded_channel();
        let backend = Arc::new(Backend {
            core: Arc::downgrade(&core),
            attempts,
            prepared: AtomicUsize::new(0),
            cancellations,
            cancelled: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            audio: AtomicUsize::new(0),
            registration_mode: AtomicUsize::new(0),
            actors: std::sync::Mutex::new(Vec::new()),
            last_registration: std::sync::Mutex::new(None),
        });
        Core::configure_calls(&core, backend.clone()).unwrap();
        Self {
            _temp: temp,
            core,
            backend,
            attempts: receiver,
            cancellations: cancelled,
        }
    }
    fn live(&self) {
        self.core
            .store
            .put_runtime("one", "history-caught-up", b"1")
            .unwrap();
    }
    fn stage(&self, batch: SyncBatch) -> String {
        let id = format!(
            "{:x}",
            sha2::Sha256::digest(serde_json::to_vec(&batch).unwrap())
        );
        self.core
            .commit_sync_batch("one", batch, "fixture", "fixture")
            .unwrap();
        id
    }
    async fn client(&self, control: bool) -> ControlConnection {
        let mut client = self
            .core
            .call_service()
            .connect("one", "fixture", control)
            .await
            .unwrap();
        assert!(matches!(
            client.events.recv().await,
            Some(CallEvent::Hello { .. })
        ));
        client
    }
}
fn batch(room: u64, cancel: bool, room_type: u64, callee: &str) -> SyncBatch {
    batch_member(room, cancel, room_type, callee, 0)
}
fn batch_member(room: u64, cancel: bool, room_type: u64, callee: &str, member: u32) -> SyncBatch {
    let now = chrono::Utc::now().timestamp() as u64;
    let inner = if cancel {
        p::message(&[
            p::uint(1, room),
            p::uint(2, room + 1000),
            p::uint(3, u64::from(member)),
            p::uint(4, 2),
        ])
        .unwrap()
    } else {
        p::message(&[
            p::uint(1, room),
            p::uint(2, room + 1000),
            p::uint(3, u64::from(member)),
            p::uint(4, room_type),
            p::bytes(5, b"caller-open"),
            p::bytes(6, b"callee-open"),
            p::bytes(7, b"synthetic_peer"),
            p::bytes(8, callee.as_bytes()),
            p::bytes(10, b"wechat"),
            p::bytes(11, b"voip-2p"),
            p::uint(12, 7),
        ])
        .unwrap()
    };
    let conference =
        p::message(&[p::uint(1, if cancel { 3 } else { 2 }), p::bytes(2, &inner)]).unwrap();
    let carrier = p::message(&[p::bytes(if cancel { 2 } else { 6 }, &conference)]).unwrap();
    let child = if cancel { "cancel" } else { "invite" };
    let xml = format!(
        "<sysmsg type='ilinkvoip'><voipmt><{child}>{}</{child}></voipmt></sysmsg>",
        STANDARD.encode(carrier)
    );
    // Existing AddMessage field layout; outer sender is an opaque system carrier,
    // while the separate inner caller/recipient are independently validated.
    let payload = p::message(&[
        p::uint(1, 1),
        p::bytes(2, &p::message(&[p::bytes(1, b"synthetic-system")]).unwrap()),
        p::bytes(3, &p::message(&[p::bytes(1, b"synthetic_self")]).unwrap()),
        p::uint(4, 10002),
        p::bytes(5, &p::message(&[p::bytes(1, xml.as_bytes())]).unwrap()),
        p::uint(6, 0),
        p::uint(7, 0),
        p::bytes(8, &p::buffer(&[])),
        p::uint(9, now),
        p::uint(12, room * 2 + u64::from(cancel)),
    ])
    .unwrap();
    SyncBatch {
        code: 0,
        flags: 0,
        cursor: vec![room as u8, u8::from(cancel)],
        commands: vec![SyncCommand { id: 5, payload }],
        continuation: false,
    }
}
async fn ready(
    f: &Fixture,
    client: &mut ControlConnection,
    scope: CallReadyScope,
) -> CallCommandStatus {
    f.core
        .call_service()
        .command(
            &client.client_id,
            CallCommand::Ready {
                request_id: uuid::Uuid::new_v4().to_string(),
                ready: true,
                format: CallAudioFormat::default(),
                scope: Some(scope),
            },
        )
        .await
        .unwrap();
    loop {
        if let Some(CallEvent::CommandResult { status, .. }) = client.events.recv().await {
            return status;
        }
    }
}
async fn offered(f: &Fixture) -> CallOffer {
    for _ in 0..100 {
        if let Some(offer) = f
            .core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
            .current_offer
        {
            return offer;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("offer deadline");
}

#[tokio::test]
async fn fresh_staged_invite_waits_for_ready_and_ack_without_releasing_projection() {
    let mut f = Fixture::new();
    f.live();
    let _observer = f.client(false).await;
    let raw = batch(42, false, 2, "synthetic_self");
    let id = f.stage(raw.clone());
    let snapshot = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap();
    let pending = snapshot.current_pending_offer.unwrap();
    assert_eq!(pending.phase, CallPendingPhase::WaitingForReady);
    assert!(snapshot.current_offer.is_none());
    assert!(!snapshot.availability.busy);
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 0);
    let mut client = f.client(true).await;
    let guard = f.core.lock_account("one").await.unwrap();
    assert_eq!(
        ready(
            &f,
            &mut client,
            CallReadyScope::Offer {
                call_id: pending.call_id.clone(),
                expected_revision: pending.revision
            }
        )
        .await,
        CallCommandStatus::Accepted
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), f.attempts.recv())
            .await
            .is_err()
    );
    drop(guard);
    let attempt = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(attempt.request.guard.is_current());
    let snapshot = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap();
    assert!(snapshot.availability.busy);
    assert!(snapshot.current_offer.is_none());
    assert_eq!(
        snapshot.current_pending_offer.unwrap().phase,
        CallPendingPhase::Preparing
    );
    attempt.reply.send(IncomingPreparation::Acked).unwrap();
    let offer = offered(&f).await;
    assert_eq!(offer.call_id, pending.call_id);
    assert_eq!(offer.revision, 3);
    assert_eq!(f.core.store.pending_sync_count("one").unwrap(), 1);
    assert_eq!(
        f.core
            .store
            .runtime("one", &format!("apply-origin:{id}"))
            .unwrap()
            .as_deref(),
        Some(b"live".as_slice())
    );
    f.stage(raw);
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn catchup_duplicate_never_becomes_a_live_candidate() {
    let f = Fixture::new();
    let raw = batch(42, false, 2, "synthetic_self");
    let id = f.stage(raw.clone());
    f.stage(raw);
    assert_eq!(
        f.core
            .store
            .runtime("one", &format!("apply-origin:{id}"))
            .unwrap()
            .as_deref(),
        Some(b"catchup".as_slice())
    );
    assert!(f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_pending_offer
        .is_none());
    assert_eq!(f.core.store.pending_sync_count("one").unwrap(), 1);
}

#[tokio::test]
async fn cancelled_candidate_scope_cannot_ack_its_replacement_or_outgoing_readiness() {
    let f = Fixture::new();
    f.live();
    let mut client = f.client(true).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let first = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_pending_offer
        .unwrap();
    f.stage(batch(42, true, 2, "synthetic_self"));
    f.stage(batch(43, false, 2, "synthetic_self"));
    assert_eq!(
        ready(
            &f,
            &mut client,
            CallReadyScope::Offer {
                call_id: first.call_id,
                expected_revision: first.revision
            }
        )
        .await,
        CallCommandStatus::Error
    );
    assert_eq!(
        ready(&f, &mut client, CallReadyScope::Outgoing).await,
        CallCommandStatus::Accepted
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 0);
    let snapshot = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap();
    assert!(!snapshot.availability.busy);
    assert_eq!(
        snapshot.current_pending_offer.unwrap().caller_id,
        "synthetic_peer"
    );
}

#[tokio::test]
async fn unsupported_media_wrong_recipient_and_revoked_authority_never_prepare() {
    let f = Fixture::new();
    f.live();
    let mut client = f.client(true).await;
    ready(&f, &mut client, CallReadyScope::All).await;
    f.stage(batch(42, false, 1, "synthetic_self"));
    f.stage(batch(43, false, 2, "other-self"));
    assert!(f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_pending_offer
        .is_none());
    f.core.disconnect("one").unwrap();
    f.stage(batch(44, false, 2, "synthetic_self"));
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verified_terminal_after_ack_frees_the_slot_but_uncertainty_does_not() {
    for outcome in [IncomingPreparation::Ended, IncomingPreparation::Uncertain] {
        let mut f = Fixture::new();
        f.live();
        let mut client = f.client(true).await;
        ready(&f, &mut client, CallReadyScope::All).await;
        let raw = batch(42, false, 2, "synthetic_self");
        f.stage(raw.clone());
        let attempt = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
            .await
            .unwrap()
            .unwrap();
        let guard = attempt.request.guard.clone();
        attempt.reply.send(outcome).unwrap();
        let mut snapshot = f
            .core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap();
        for _ in 0..100 {
            if snapshot.current_pending_offer.is_none() {
                break;
            }
            tokio::task::yield_now().await;
            snapshot = f
                .core
                .call_service()
                .current("one", "fixture")
                .await
                .unwrap();
        }
        assert!(snapshot.current_offer.is_none());
        assert!(snapshot.current_pending_offer.is_none());
        assert_eq!(
            snapshot.availability.busy,
            outcome == IncomingPreparation::Uncertain
        );
        assert!(!guard.is_current());
        assert_eq!(f.core.store.pending_sync_count("one").unwrap(), 1);
        f.stage(raw);
        assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn another_ready_client_cannot_keep_an_ending_or_disconnected_owner_current() {
    for disconnect in [false, true] {
        let mut f = Fixture::new();
        f.live();
        let mut owner = f.client(true).await;
        let mut other = f.client(true).await;
        ready(&f, &mut owner, CallReadyScope::All).await;
        ready(&f, &mut other, CallReadyScope::All).await;
        f.stage(batch(42, false, 2, "synthetic_self"));
        let attempt = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
            .await
            .unwrap()
            .unwrap();
        let guard = attempt.request.guard.clone();
        attempt.reply.send(IncomingPreparation::Acked).unwrap();
        let offer = offered(&f).await;
        f.core
            .call_service()
            .command(
                &owner.client_id,
                CallCommand::Answer {
                    request_id: "answer".into(),
                    call_id: offer.call_id.clone(),
                    expected_revision: offer.revision,
                    media: CallMedia::default(),
                },
            )
            .await
            .unwrap();
        let view = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(CallEvent::State { call }) = owner.events.recv().await {
                    if call.state == CallState::Connected {
                        break call;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(guard.is_current());
        if disconnect {
            f.core.call_service().disconnect(&owner.client_id).await;
        } else {
            f.core
                .call_service()
                .command(
                    &owner.client_id,
                    CallCommand::Hangup {
                        request_id: "hangup".into(),
                        call_id: view.call_id,
                        expected_revision: view.revision,
                    },
                )
                .await
                .unwrap();
        }
        assert!(!guard.is_current());
    }
}

async fn next_cancellation(f: &mut Fixture) -> Cancellation {
    tokio::time::timeout(Duration::from_secs(1), f.cancellations.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}
fn cancellation_note(
    f: &Fixture,
    raw: SyncBatch,
    lease: CallAuthorityLease,
) -> IncomingNotification {
    let message =
        wechat_protocol::wire::decode_add_message(&raw.commands[0].payload, "synthetic_self")
            .unwrap();
    IncomingNotification {
        lease,
        receipt_id: uuid::Uuid::new_v4().to_string(),
        source_created_at: u64::from(message.created_at),
        received_at_unix_ms: chrono::Utc::now().timestamp_millis() as u64,
        own_username: "synthetic_self".into(),
        caller_username: String::new(),
        original_message: Arc::from(raw.commands[0].payload.clone()),
        control: crate::calls::incoming::decode_system_control(&message)
            .unwrap()
            .unwrap(),
        core: Arc::downgrade(&f.core),
        header_from: message.from,
    }
}

#[tokio::test]
async fn cancellation_reaches_held_auth_without_account_prelock_and_late_ack_cannot_revive() {
    let mut f = Fixture::new();
    f.live();
    let mut client = f.client(true).await;
    ready(&f, &mut client, CallReadyScope::All).await;
    let invite = batch(42, false, 2, "synthetic_self");
    f.stage(invite.clone());
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    let first_id = preparation.request.identity.clone();
    let guard = preparation.request.guard.clone();
    // Auth may own this lock, but already-owned cancellation bytes must still
    // reach local actor classification. Live Sync's RPC lock limit is separate.
    let account = f.core.lock_account("one").await.unwrap();
    f.stage(batch(42, true, 2, "synthetic_self"));
    let cancellation = next_cancellation(&mut f).await;
    assert_eq!(cancellation.identity, first_id);
    assert!(cancellation.note.core.upgrade().is_some());
    assert!(guard.is_current());
    assert!(
        f.core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
            .availability
            .busy
    );
    cancellation
        .reply
        .send(Ok(IncomingCancellation::Ended))
        .unwrap();
    settle().await;
    assert!(!guard.is_current());
    assert!(
        !f.core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
            .availability
            .busy
    );
    f.stage(invite);
    f.stage(batch(43, false, 2, "synthetic_self"));
    assert!(f.attempts.try_recv().is_err());
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    drop(account);
    let next = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.request.identity, first_id);
    next.reply.send(IncomingPreparation::Acked).unwrap();
    let second = offered(&f).await;
    assert_ne!(second.call_id, first_id.call_id());
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 2);
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ignored_joined_cancellation_preserves_guard_owner_and_audio() {
    use wechat_calls::{
        audio::AudioFormat,
        frame::{self, MediaHeader},
    };
    let mut f = Fixture::new();
    f.live();
    let mut owner = f.client(true).await;
    ready(&f, &mut owner, CallReadyScope::All).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    let guard = preparation.request.guard.clone();
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    let offer = offered(&f).await;
    f.core
        .call_service()
        .command(
            &owner.client_id,
            CallCommand::Answer {
                request_id: "join".into(),
                call_id: offer.call_id.clone(),
                expected_revision: offer.revision,
                media: CallMedia::default(),
            },
        )
        .await
        .unwrap();
    let mut token = None;
    let mut connected = false;
    while token.is_none() || !connected {
        match owner.events.recv().await.unwrap() {
            CallEvent::CommandResult {
                request_id,
                owner_token,
                ..
            } if request_id == "join" => token = owner_token,
            CallEvent::State { call } if call.state == CallState::Connected => connected = true,
            _ => {}
        }
    }
    let media = f
        .core
        .call_service()
        .attach_media(
            "one",
            "fixture",
            &offer.call_id,
            &owner.client_id,
            &token.unwrap(),
        )
        .await
        .unwrap();
    let cancel = batch(42, true, 2, "synthetic_self");
    f.stage(cancel.clone());
    let classification = next_cancellation(&mut f).await;
    assert!(guard.is_current());
    f.core
        .call_service()
        .validate_media(&media.id)
        .await
        .unwrap();
    classification
        .reply
        .send(Ok(IncomingCancellation::Ignored))
        .unwrap();
    settle().await;
    assert!(guard.is_current());
    f.core
        .call_service()
        .validate_control(&owner.client_id)
        .await
        .unwrap();
    assert!(f
        .core
        .call_service()
        .media_delivery_allowed(&media.id)
        .await
        .unwrap());
    assert!(owner.events.try_recv().is_err());
    let bytes = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        AudioFormat::new(16000, 1, 20).unwrap(),
        &[321; 320],
    )
    .unwrap();
    f.core
        .call_service()
        .media_frame(&media.id, &bytes)
        .await
        .unwrap();
    assert_eq!(f.backend.audio.load(Ordering::SeqCst), 1);
    f.stage(cancel);
    settle().await;
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn foreign_cancel_member_header_account_and_generation_do_not_change_candidate() {
    let mut f = Fixture::new();
    f.live();
    let old = f.core.call_authority("one").unwrap();
    let mut state = f.core.account_state("one").unwrap();
    state.session_key = vec![8; 16];
    f.core.save_state("one", &state).unwrap();
    assert!(!old.current());
    let mut owner = f.client(true).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let pending = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_pending_offer
        .unwrap();
    f.stage(batch_member(42, true, 2, "synthetic_self", 99));
    let lease = f.core.call_authority("one").unwrap();
    let mut wrong_header =
        cancellation_note(&f, batch(42, true, 2, "synthetic_self"), lease.clone());
    wrong_header.header_from = "different-system".into();
    f.core
        .call_service()
        .ingest_incoming(wrong_header, Duration::from_secs(30));
    let mut wrong_recipient = cancellation_note(&f, batch(42, true, 2, "synthetic_self"), lease);
    wrong_recipient.own_username = "different-self".into();
    f.core
        .call_service()
        .ingest_incoming(wrong_recipient, Duration::from_secs(30));
    let stale = cancellation_note(&f, batch(42, true, 2, "synthetic_self"), old);
    f.core
        .call_service()
        .ingest_incoming(stale, Duration::from_secs(30));
    settle().await;
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 0);
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
            .current_pending_offer
            .unwrap()
            .call_id,
        pending.call_id
    );
    ready(&f, &mut owner, CallReadyScope::All).await;
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(preparation.request.guard.is_current());
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    offered(&f).await;
}

#[tokio::test]
async fn cancellation_error_or_timeout_holds_slot_and_late_ignored_prepare_cannot_clear_it() {
    for timeout in [false, true] {
        let mut f = Fixture::new();
        f.live();
        let mut owner = f.client(true).await;
        ready(&f, &mut owner, CallReadyScope::All).await;
        f.stage(batch(42, false, 2, "synthetic_self"));
        let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
            .await
            .unwrap()
            .unwrap();
        let guard = preparation.request.guard.clone();
        let identity = preparation.request.identity.clone();
        let authority = preparation.request.authority.clone();
        f.stage(batch(42, true, 2, "synthetic_self"));
        let classification = next_cancellation(&mut f).await;
        if timeout {
            tokio::time::sleep(Duration::from_millis(5100)).await;
            assert!(classification
                .reply
                .send(Ok(IncomingCancellation::Ended))
                .is_err());
        } else {
            classification
                .reply
                .send(Err(CallServiceError(CallErrorCode::InvalidRequest)))
                .unwrap();
        }
        settle().await;
        assert!(!guard.is_current());
        preparation
            .reply
            .send(IncomingPreparation::Ignored)
            .unwrap();
        settle().await;
        let snapshot = f
            .core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap();
        assert!(snapshot.availability.busy);
        assert!(snapshot.current_offer.is_none());
        assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
        f.core
            .call_service()
            .verified_incoming_terminal(&authority, &identity)
            .unwrap();
        assert!(
            !f.core
                .call_service()
                .current("one", "fixture")
                .await
                .unwrap()
                .availability
                .busy
        );
    }
}

#[tokio::test]
async fn admitted_member_replaces_invite_binding_and_stale_callbacks_cannot_retire_newer_member() {
    let mut f = Fixture::new();
    f.live();
    let mut owner = f.client(true).await;
    ready(&f, &mut owner, CallReadyScope::All).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    let identity = preparation.request.identity.clone();
    let authority = preparation.request.authority.clone();
    let original = preparation.request.notification.clone();
    let guard = preparation.request.guard.clone();
    let service = f.core.call_service();
    assert!(service
        .set_incoming_member(&authority, &identity, 43, 1042, 9)
        .is_err());
    service
        .set_incoming_member(&authority, &identity, 42, 1042, 9)
        .unwrap();
    let NotificationPayload::Invite(invite) = &original.control.payload else {
        panic!("invite fixture");
    };
    assert_eq!(invite.member_id, Some(0));
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    let offer = offered(&f).await;
    f.stage(batch_member(42, true, 2, "synthetic_self", 0));
    f.stage(batch_member(42, true, 2, "synthetic_self", 11));
    for _ in 0..2 {
        let foreign = next_cancellation(&mut f).await;
        foreign
            .reply
            .send(Ok(IncomingCancellation::Ignored))
            .unwrap();
    }
    settle().await;
    assert!(guard.is_current());
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 2);
    f.stage(batch_member(42, true, 2, "synthetic_self", 9));
    let old = next_cancellation(&mut f).await;
    service
        .command(
            &owner.client_id,
            CallCommand::Answer {
                request_id: "join-member".into(),
                call_id: offer.call_id,
                expected_revision: offer.revision,
                media: CallMedia::default(),
            },
        )
        .await
        .unwrap();
    loop {
        if let Some(CallEvent::State { call }) = owner.events.recv().await {
            if call.state == CallState::Connected {
                break;
            }
        }
    }
    // A verified Join may admit member zero. A verdict for the former member
    // must not clear that newer binding when its callback completes late.
    service
        .set_incoming_member(&authority, &identity, 42, 1042, 0)
        .unwrap();
    old.reply.send(Ok(IncomingCancellation::Ended)).unwrap();
    settle().await;
    assert!(guard.is_current());
    service.validate_control(&owner.client_id).await.unwrap();
    assert!(
        service
            .current("one", "fixture")
            .await
            .unwrap()
            .availability
            .busy
    );
    let mut zero = cancellation_note(
        &f,
        batch_member(42, true, 2, "synthetic_self", 0),
        f.core.call_authority("one").unwrap(),
    );
    zero.receipt_id = "new-member-zero".into();
    service.ingest_incoming(zero, Duration::from_secs(30));
    let zero = next_cancellation(&mut f).await;
    zero.reply.send(Ok(IncomingCancellation::Ignored)).unwrap();
    settle().await;
    service
        .verified_incoming_terminal(&authority, &identity)
        .unwrap();
    assert!(service
        .set_incoming_member(&authority, &identity, 42, 1042, 99)
        .is_err());
    assert!(
        !service
            .current("one", "fixture")
            .await
            .unwrap()
            .availability
            .busy
    );
}

#[tokio::test]
async fn same_staging_turn_cancellation_waits_for_explicit_actor_registration() {
    let mut f = Fixture::new();
    f.live();
    let mut owner = f.client(true).await;
    ready(&f, &mut owner, CallReadyScope::All).await;
    let staging = f.core.lock_account("one").await.unwrap();
    f.stage(batch(42, false, 2, "synthetic_self"));
    f.stage(batch(42, true, 2, "synthetic_self"));
    settle().await;
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 0);
    drop(staging);
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(preparation.request.registration.is_registered());
    let cancellation = next_cancellation(&mut f).await;
    assert_eq!(cancellation.identity, preparation.request.identity);
    cancellation
        .reply
        .send(Ok(IncomingCancellation::Ended))
        .unwrap();
    settle().await;
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    settle().await;
    let snapshot = f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap();
    assert!(!snapshot.availability.busy);
    assert!(snapshot.current_offer.is_none());
    assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn constructing_a_preparation_future_does_not_register_an_actor() {
    let mut f = Fixture::new();
    f.live();
    f.backend.registration_mode.store(3, Ordering::SeqCst);
    let mut owner = f.client(true).await;
    ready(&f, &mut owner, CallReadyScope::All).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    f.stage(batch(42, true, 2, "synthetic_self"));
    settle().await;
    assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 0);
    assert!(!preparation.request.registration.is_registered());
    assert!(f.backend.install(&preparation.request));
    let cancellation = next_cancellation(&mut f).await;
    cancellation
        .reply
        .send(Ok(IncomingCancellation::Ended))
        .unwrap();
    settle().await;
    assert!(!preparation.request.registration.mark_registered());
    preparation
        .reply
        .send(IncomingPreparation::Ignored)
        .unwrap();
}

#[tokio::test]
async fn unregistered_synchronous_preparation_ignores_or_ends_without_false_cancel_uncertainty() {
    for mode in [1, 2] {
        let f = Fixture::new();
        f.live();
        f.backend.registration_mode.store(mode, Ordering::SeqCst);
        let mut owner = f.client(true).await;
        ready(&f, &mut owner, CallReadyScope::All).await;
        let staging = f.core.lock_account("one").await.unwrap();
        f.stage(batch(42, false, 2, "synthetic_self"));
        f.stage(batch(42, true, 2, "synthetic_self"));
        settle().await;
        assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 0);
        drop(staging);
        settle().await;
        let snapshot = f
            .core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap();
        assert!(!snapshot.availability.busy);
        assert!(snapshot.current_offer.is_none());
        assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 1);
        assert_eq!(f.backend.cancelled.load(Ordering::SeqCst), 0);
        assert!(!f
            .backend
            .last_registration
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .mark_registered());
        assert_eq!(f.backend.stopped.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn changed_member_cancel_is_handed_off_during_ack_cutover_and_applied_only_after_proof() {
    let mut f = Fixture::new();
    f.live();
    let mut owner = f.client(true).await;
    ready(&f, &mut owner, CallReadyScope::All).await;
    f.stage(batch(42, false, 2, "synthetic_self"));
    let preparation = tokio::time::timeout(Duration::from_secs(1), f.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    let guard = preparation.request.guard.clone();
    let identity = preparation.request.identity.clone();
    let authority = preparation.request.authority.clone();
    // The ACK's bytes may already have returned before the actor registers its
    // new own member. The broker delivers this raw control, never a verdict.
    f.stage(batch_member(42, true, 2, "synthetic_self", 9));
    let cancellation = next_cancellation(&mut f).await;
    assert!(guard.is_current());
    assert!(f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_offer
        .is_none());
    f.core
        .call_service()
        .set_incoming_member(&authority, &identity, 42, 1042, 9)
        .unwrap();
    cancellation
        .reply
        .send(Ok(IncomingCancellation::Ended))
        .unwrap();
    settle().await;
    preparation.reply.send(IncomingPreparation::Acked).unwrap();
    settle().await;
    assert!(!guard.is_current());
    assert!(
        !f.core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
            .availability
            .busy
    );
}
