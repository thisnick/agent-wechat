//! Writer/CAS tests use real Core admission, staged controls and encrypted
//! journals. The only host is synthetic and has no native-network fallback.
use super::*;
use crate::{
    call_types::*,
    calls::{notifications::*, service::ControlConnection},
    Store,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::Condvar;
use wechat_protocol::{
    wire::{self, SyncBatch, SyncCommand},
    AuthSnapshot, DeviceProfile,
};

const OWN: &str = "synthetic_self";
const PEER: &str = "synthetic_peer";
struct Captured {
    request: IncomingRequest,
    complete: oneshot::Sender<IncomingPreparation>,
}
struct Backend {
    core: Weak<Core>,
    captured: tokio::sync::mpsc::UnboundedSender<Captured>,
    cancelled: AtomicU32,
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
    fn incoming_supported(&self, _: &AccountAuthority, peer: &str) -> bool {
        peer == PEER
    }
    fn prepare_incoming(&self, request: IncomingRequest) -> BackendFuture<IncomingPreparation> {
        let registration = request.registration.clone();
        let (complete, result) = oneshot::channel();
        self.captured
            .send(Captured { request, complete })
            .unwrap_or_else(|_| panic!("synthetic receiver"));
        assert!(registration.mark_registered());
        Box::pin(async move { Ok(result.await.unwrap_or(IncomingPreparation::Ignored)) })
    }
    fn cancel_incoming(
        &self,
        _: CallIdentity,
        _: Arc<IncomingNotification>,
    ) -> BackendFuture<IncomingCancellation> {
        self.cancelled.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(IncomingCancellation::Ignored) })
    }
    fn submit(&self, _: BackendRequest, _: BackendSink) -> BackendFuture<BackendOutcome> {
        Box::pin(async { Err(service_error(CallErrorCode::Unsupported)) })
    }
    fn send_audio(&self, _: BackendAudioRequest) -> BackendFuture<()> {
        Box::pin(async { Err(service_error(CallErrorCode::Unsupported)) })
    }
    fn stop(&self, _: CallIdentity) -> BackendFuture<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Uncertain) })
    }
}

#[derive(Clone, Copy)]
enum Hold {
    BeforeHandoff,
    AfterHandoff,
}
struct Host {
    hold: Hold,
    entered: Notify,
    released: Mutex<bool>,
    changed: Condvar,
    prepared: AtomicU32,
    submitted: AtomicU32,
    cookies: Mutex<Vec<Vec<u8>>>,
    invalid_unsent_marker: AtomicBool,
}
impl Host {
    fn new(hold: Hold) -> Arc<Self> {
        Arc::new(Self {
            hold,
            entered: Notify::new(),
            released: Mutex::new(false),
            changed: Condvar::new(),
            prepared: AtomicU32::new(0),
            submitted: AtomicU32::new(0),
            cookies: Mutex::new(vec![]),
            invalid_unsent_marker: AtomicBool::new(false),
        })
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn pause(&self) -> Result<()> {
        self.entered.notify_one();
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(3),
                |done| !*done,
            )
            .unwrap();
        ensure!(*released, "synthetic exchange gate timed out");
        Ok(())
    }
}
impl TestCallIo for Host {
    fn call(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
        handoff: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        let common = wechat_protocol::calls::signaling::CommonRequest::decode(request)?;
        ensure!(
            common.base_request.is_some() && common.body.is_some(),
            "synthetic request wrapper"
        );
        let first = self.prepared.fetch_add(1, Ordering::AcqRel) == 0;
        self.cookies.lock().unwrap().push(state.cookie.clone());
        if first && matches!(self.hold, Hold::BeforeHandoff) {
            self.pause()?;
        }
        handoff()?;
        let submitted = self.submitted.fetch_add(1, Ordering::AcqRel) + 1;
        if first && matches!(self.hold, Hold::AfterHandoff) {
            self.pause()?;
        }
        state.cookie = vec![submitted as u8];
        if self.invalid_unsent_marker.load(Ordering::Acquire) {
            return Err(CallRpcNotSubmitted.into());
        }
        Ok(reply(route))
    }
}
fn reply(route: CallHostRoute) -> Vec<u8> {
    let body = match route {
        CallHostRoute::AppAuth => {
            let auth = p::message(&[p::bytes(9, b"synthetic"), p::uint(11, 1)]).unwrap();
            let unify = p::message(&[p::bytes(2, &auth), p::bytes(3, &p::uint(1, 17))]).unwrap();
            p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &unify)]).unwrap()
        }
        CallHostRoute::LiveSync => p::bytes(2, b"synthetic-cursor"),
        _ => panic!("unexpected synthetic host route"),
    };
    p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &body)]).unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<Backend>,
    _client: ControlConnection,
    complete: Option<oneshot::Sender<IncomingPreparation>>,
    transport: Arc<FencedTransport>,
    host: Arc<Host>,
}
impl Fixture {
    async fn new(hold: Hold) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        let mut auth_snapshot = AuthSnapshot::default();
        auth_snapshot.response = p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 0)]).unwrap();
        let state = AccountState {
            schema: 1,
            device: DeviceProfile::generate("Synthetic Linux".into()).unwrap(),
            uin: 1,
            own_username: OWN.into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot,
            long_host: "synthetic-unused.weixin.qq.com".into(),
            short_host: "synthetic-unused.weixin.qq.com".into(),
        };
        core.save_state("one", &state).unwrap();
        let (captured, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let backend = Arc::new(Backend {
            core: Arc::downgrade(&core),
            captured,
            cancelled: AtomicU32::new(0),
        });
        Core::configure_calls(&core, backend.clone()).unwrap();
        store.put_runtime("one", "history-caught-up", b"1").unwrap();
        let mut client = core
            .call_service()
            .connect("one", "transport-fixture", true)
            .await
            .unwrap();
        assert!(matches!(
            client.events.recv().await,
            Some(CallEvent::Hello { .. })
        ));
        core.call_service()
            .command(
                &client.client_id,
                CallCommand::Ready {
                    request_id: "ready".into(),
                    ready: true,
                    format: CallAudioFormat::default(),
                    scope: Some(CallReadyScope::All),
                },
            )
            .await
            .unwrap();
        while !matches!(
            client.events.recv().await,
            Some(CallEvent::CommandResult {
                status: CallCommandStatus::Accepted,
                ..
            })
        ) {}
        {
            let _writer = core.lock_account("one").await.unwrap();
            stage(&core, false);
        }
        let Captured { request, complete } =
            tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .unwrap();
        let lease = request.notification.lease.clone();
        let (audio, _) = mpsc::sync_channel(1);
        let worker = Arc::new(Worker {
            identity: request.identity.clone(),
            lease: lease.clone(),
            stop: AtomicBool::new(false),
            media_stop: AtomicBool::new(false),
            generation: AtomicU32::new(1),
            submission: SendAdmission::default(),
            metrics: MediaMetrics::default(),
            scope_deadline: Instant::now() + Duration::from_secs(60),
            scope_expires_unix_ms: unix_ms().unwrap() + 60_000,
            mutations_submitted: AtomicBool::new(false),
            incoming: None,
            audio,
            done: Mutex::new(None),
            notify: Notify::new(),
        });
        {
            let _writer = core.lock_account("one").await.unwrap();
            assert!(store
                .begin_call(
                    "one",
                    request.identity.call_id(),
                    &lease.authority().generation
                )
                .unwrap());
        }
        let host = Host::new(hold);
        let transport = Arc::new(FencedTransport {
            core: Arc::downgrade(&core),
            account: "one".into(),
            lease,
            call_id: request.identity.call_id().into(),
            ordinal: AtomicU64::new(1),
            scope_deadline: worker.scope_deadline,
            scope_expires_unix_ms: worker.scope_expires_unix_ms,
            worker,
            admission: ControlAdmission::Incoming(request.guard),
            runtime: tokio::runtime::Handle::current(),
            test_io: Some(host.clone()),
        });
        Self {
            _temp: temp,
            core,
            backend,
            _client: client,
            complete: Some(complete),
            transport,
            host,
        }
    }
    fn start(
        &self,
        route: CallHostRoute,
        task: u32,
    ) -> tokio::task::JoinHandle<(AccountState, Result<Vec<u8>>)> {
        self.start_from(self.core.account_state("one").unwrap(), route, task)
    }
    fn start_from(
        &self,
        mut state: AccountState,
        route: CallHostRoute,
        task: u32,
    ) -> tokio::task::JoinHandle<(AccountState, Result<Vec<u8>>)> {
        let transport = self.transport.clone();
        let request = wechat_protocol::calls::signaling::CommonRequest {
            base_request: Some(wire::base_request(&state.device, state.uin, task).unwrap()),
            body: Some(match route {
                CallHostRoute::AppAuth => p::bytes(1, &p::bytes(1, &[9; 32])),
                CallHostRoute::LiveSync => {
                    wechat_protocol::calls::personal::live_sync(99, b"cursor", 6, task)
                        .unwrap()
                        .body()
                        .to_vec()
                }
                _ => panic!("unsupported synthetic request"),
            }),
            unknown_fields: vec![],
        }
        .encode()
        .unwrap();
        tokio::task::spawn_blocking(move || {
            let result = transport.call_rpc(&mut state, route, &request);
            (state, result)
        })
    }
    fn records(&self) -> Vec<wechat_store::CallActionRecord> {
        self.core
            .store
            .call_action_records("one", &self.transport.call_id)
            .unwrap()
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.host.entered.notified())
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.release();
        if let Some(done) = self.complete.take() {
            let _ = done.send(IncomingPreparation::Ignored);
        }
    }
}
fn stage(core: &Arc<Core>, cancel: bool) {
    let inner = if cancel {
        p::message(&[p::uint(1, 7), p::uint(2, 9), p::uint(3, 0), p::uint(4, 2)]).unwrap()
    } else {
        p::message(&[
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 0),
            p::uint(4, 2),
            p::bytes(5, b"peer-open"),
            p::bytes(6, b"self-open"),
            p::bytes(7, PEER.as_bytes()),
            p::bytes(8, OWN.as_bytes()),
            p::bytes(10, b"wechat"),
            p::bytes(11, b"voip-2p"),
            p::uint(12, 7),
        ])
        .unwrap()
    };
    let conf = p::message(&[p::uint(1, if cancel { 3 } else { 2 }), p::bytes(2, &inner)]).unwrap();
    let outer = p::bytes(if cancel { 2 } else { 6 }, &conf);
    let child = if cancel { "cancel" } else { "invite" };
    let xml = format!(
        "<sysmsg type='ilinkvoip'><voipmt><{child}>{}</{child}></voipmt></sysmsg>",
        STANDARD.encode(outer)
    );
    let raw = p::message(&[
        p::uint(1, 1),
        p::bytes(2, &p::bytes(1, b"synthetic-system")),
        p::bytes(3, &p::bytes(1, OWN.as_bytes())),
        p::uint(4, 10002),
        p::bytes(5, &p::bytes(1, xml.as_bytes())),
        p::uint(6, 0),
        p::uint(7, 0),
        p::bytes(8, &p::buffer(&[])),
        p::uint(9, chrono::Utc::now().timestamp() as u64),
        p::uint(12, 100 + u64::from(cancel)),
    ])
    .unwrap();
    core.commit_sync_batch(
        "one",
        SyncBatch {
            code: 0,
            flags: 0,
            cursor: vec![u8::from(cancel)],
            commands: vec![SyncCommand {
                id: 5,
                payload: raw,
            }],
            continuation: false,
        },
        "synthetic",
        "synthetic",
    )
    .unwrap();
}

#[tokio::test]
async fn writer_is_free_for_staged_control_and_newer_cookie_feeds_next_rpc() {
    let f = Fixture::new(Hold::AfterHandoff).await;
    let first = f.start(CallHostRoute::AppAuth, 1);
    f.entered().await;
    let newer_cipher = {
        let _writer = tokio::time::timeout(Duration::from_millis(500), f.core.lock_account("one"))
            .await
            .expect("RPC held account writer")
            .unwrap();
        stage(&f.core, true);
        let mut newer = f.core.account_state("one").unwrap();
        newer.cookie = vec![77];
        f.core.save_state("one", &newer).unwrap();
        f.core
            .store
            .runtime("one", "account-state")
            .unwrap()
            .unwrap()
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while f.backend.cancelled.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.host.release();
    let (state, result) = first.await.unwrap();
    assert_eq!(result.unwrap(), reply(CallHostRoute::AppAuth));
    assert_eq!(state.cookie, [77]);
    assert_eq!(
        f.core
            .store
            .runtime("one", "account-state")
            .unwrap()
            .unwrap(),
        newer_cipher
    );
    let (_, result) = f
        .start_from(state, CallHostRoute::LiveSync, 2)
        .await
        .unwrap();
    assert!(result.is_ok());
    assert_eq!(*f.host.cookies.lock().unwrap(), [vec![], vec![77]]);
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 2);
    let records = f.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].action_id, "1");
    assert_eq!(records[1].action_id, "2");
    assert!(records.iter().all(|record| record.outcome == "received"
        && record.encrypted_request.is_some()
        && record.encrypted_reply.is_some()));
}

#[tokio::test]
async fn policy_revocation_before_handoff_records_unsent_and_never_submits() {
    let f = Fixture::new(Hold::BeforeHandoff).await;
    let task = f.start(CallHostRoute::AppAuth, 1);
    f.entered().await;
    let accepted = {
        let _writer = f.core.lock_account("one").await.unwrap();
        let mut state = f.core.account_state("one").unwrap();
        state.auth_snapshot.ax_auth = vec![8, 1];
        f.core.save_state("one", &state).unwrap();
        f.core
            .cached_call_authority("one")
            .unwrap()
            .authority()
            .clone()
    };
    f.host.release();
    let (state, result) = task.await.unwrap();
    assert!(result.unwrap_err().is::<CallRpcNotSubmitted>());
    assert!(state.cookie.is_empty());
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 0);
    assert_eq!(f.host.prepared.load(Ordering::Acquire), 1);
    assert_eq!(
        f.core.cached_call_authority("one").unwrap().authority(),
        &accepted
    );
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "not_submitted");
    assert!(records[0].encrypted_reply.is_none());
}

#[tokio::test]
async fn post_handoff_policy_change_retains_reply_without_adopting_old_state() {
    let f = Fixture::new(Hold::AfterHandoff).await;
    let task = f.start(CallHostRoute::AppAuth, 1);
    f.entered().await;
    let (accepted, cipher) = {
        let _writer = f.core.lock_account("one").await.unwrap();
        let mut state = f.core.account_state("one").unwrap();
        state.auth_snapshot.ax_auth = vec![8, 1];
        state.cookie = vec![77];
        f.core.save_state("one", &state).unwrap();
        (
            f.core
                .cached_call_authority("one")
                .unwrap()
                .authority()
                .clone(),
            f.core
                .store
                .runtime("one", "account-state")
                .unwrap()
                .unwrap(),
        )
    };
    f.host.release();
    let (state, result) = task.await.unwrap();
    assert!(result.is_err());
    assert!(state.cookie.is_empty());
    assert_eq!(
        f.core
            .store
            .runtime("one", "account-state")
            .unwrap()
            .unwrap(),
        cipher
    );
    assert_eq!(
        f.core.cached_call_authority("one").unwrap().authority(),
        &accepted
    );
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "received");
    let clear = f
        .core
        .vault
        .open_record(
            "one",
            &format!("call-reply:{}:1", f.transport.call_id),
            records[0].encrypted_reply.as_deref().unwrap(),
        )
        .unwrap();
    assert_eq!(clear.as_slice(), reply(CallHostRoute::AppAuth));
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn writer_reacquisition_failure_cannot_discard_authenticated_reply() {
    let f = Fixture::new(Hold::AfterHandoff).await;
    let task = f.start(CallHostRoute::AppAuth, 1);
    f.entered().await;
    let core = f.core.clone();
    let poisoned = thread::spawn(move || {
        let _guard = core.gates.lock().unwrap();
        panic!("synthetic poisoned writer registry");
    })
    .join();
    assert!(poisoned.is_err());
    f.host.release();
    let (state, result) = task.await.unwrap();
    assert!(result.is_err());
    assert!(state.cookie.is_empty());
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "received");
    assert!(records[0].encrypted_reply.is_some());
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn an_unsent_marker_after_successful_handoff_is_uncertain_and_never_retried() {
    let f = Fixture::new(Hold::AfterHandoff).await;
    f.host.invalid_unsent_marker.store(true, Ordering::Release);
    let task = f.start(CallHostRoute::AppAuth, 1);
    f.entered().await;
    f.host.release();
    let (state, result) = task.await.unwrap();
    let error = result.unwrap_err();
    assert!(!error.is::<CallRpcNotSubmitted>());
    assert!(state.cookie.is_empty());
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "needs_review");
    assert!(records[0].encrypted_request.is_some() && records[0].encrypted_reply.is_none());
    assert_eq!(f.host.prepared.load(Ordering::Acquire), 1);
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 1);
}
