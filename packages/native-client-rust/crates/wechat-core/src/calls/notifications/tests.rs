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
        _note: Arc<IncomingNotification>,
    ) -> FutureResult<IncomingCancellation> {
        assert!(
            self.actors.lock().unwrap().contains(&identity),
            "cancellation preceded mailbox installation"
        );
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        let (reply, received) = oneshot::channel();
        self.cancellations
            .send(Cancellation { identity, reply })
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

    let old = f.core.call_authority("one").unwrap();
    let mut state = f.core.account_state("one").unwrap();
    state.session_key = vec![8; 16];
    f.core.save_state("one", &state).unwrap();
    assert!(!old.current());
    let catchup = batch(41, false, 2, "synthetic_self");
    let catchup_id = f.stage(catchup.clone());
    f.live();
    f.stage(catchup);
    assert_eq!(
        f.core
            .store
            .runtime("one", &format!("apply-origin:{catchup_id}"))
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
    let _observer = f.client(false).await;
    f.stage(batch(39, false, 1, "synthetic_self"));
    f.stage(batch(40, false, 2, "other-self"));
    assert!(f
        .core
        .call_service()
        .current("one", "fixture")
        .await
        .unwrap()
        .current_pending_offer
        .is_none());
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
    assert_eq!(f.core.store.pending_sync_count("one").unwrap(), 5);
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

    f.core.disconnect("one").unwrap();
    f.stage(batch(44, false, 2, "synthetic_self"));
    assert_eq!(f.backend.prepared.load(Ordering::SeqCst), 1);
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
