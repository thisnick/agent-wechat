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
struct Backend {
    core: Weak<Core>,
    attempts: mpsc::UnboundedSender<Attempt>,
    prepared: AtomicUsize,
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
        let (reply, result) = oneshot::channel();
        self.attempts
            .send(Attempt { request, reply })
            .unwrap_or_else(|_| panic!("fixture receiver"));
        Box::pin(async move { Ok(result.await.unwrap_or(IncomingPreparation::Uncertain)) })
    }
    fn submit(&self, _: BackendRequest, _: BackendSink) -> FutureResult<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Connected) })
    }
    fn send_audio(&self, _: BackendAudioRequest) -> FutureResult<()> {
        Box::pin(async { Ok(()) })
    }
    fn stop(&self, _: CallIdentity) -> FutureResult<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Uncertain) })
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<Backend>,
    attempts: mpsc::UnboundedReceiver<Attempt>,
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
        let backend = Arc::new(Backend {
            core: Arc::downgrade(&core),
            attempts,
            prepared: AtomicUsize::new(0),
        });
        Core::configure_calls(&core, backend.clone()).unwrap();
        Self {
            _temp: temp,
            core,
            backend,
            attempts: receiver,
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
    let now = chrono::Utc::now().timestamp() as u64;
    let inner = if cancel {
        p::message(&[
            p::uint(1, room),
            p::uint(2, room + 1000),
            p::uint(3, 0),
            p::uint(4, 2),
        ])
        .unwrap()
    } else {
        p::message(&[
            p::uint(1, room),
            p::uint(2, room + 1000),
            p::uint(3, 0),
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
