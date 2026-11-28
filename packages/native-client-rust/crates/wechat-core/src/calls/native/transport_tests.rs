//! Writer/CAS tests use real Core admission, staged controls and encrypted
//! journals. The only host is synthetic and has no native-network fallback.
use super::*;
use crate::{
    call_types::*,
    calls::{
        notifications::*,
        service::{CallService, ControlConnection},
    },
    Store,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::Condvar;
use wechat_protocol::{
    calls::app_session::PendingAppAuth,
    wire::{self, SyncBatch, SyncCommand},
    AuthSnapshot, DeviceProfile,
};

const OWN: &str = "synthetic_self";
const PEER: &str = "synthetic_peer";

fn synthetic_account() -> (tempfile::TempDir, Arc<Core>) {
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
    (temp, core)
}

fn app_authority(core: &Core, lease: &CallAuthorityLease) -> AppAuthority {
    AppAuthority {
        account_generation: nonzero64(lease.authority().generation.as_bytes()),
        logout_epoch: 0,
        credential_digest: authorization(&core.account_state("one").unwrap()).unwrap(),
    }
}

#[test]
fn app_session_cache_is_fenced_by_errors_and_account_authority() {
    let (_temp, core) = synthetic_account();
    let backend = NativeCallBackend::new(Arc::downgrade(&core));
    let inner = &backend.inner;
    let lease = core.call_authority("one").unwrap();
    let auth = app_authority(&core, &lease);
    let app = PendingAppAuth::new(auth, [9; 32])
        .unwrap()
        .complete(&reply(CallHostRoute::AppAuth), auth)
        .unwrap();

    assert!(inner.cached_app_session("one", auth).is_none());
    inner.store_app_session(&lease, app.clone());
    inner
        .invalidate_app_session_on_error("one", Ok(()))
        .unwrap();
    assert_eq!(inner.cached_app_session("one", auth).unwrap().app_uin(), 17);
    assert!(inner.cached_app_session("other", auth).is_none());
    let error = inner.invalidate_app_session_on_error::<()>(
        "one",
        Err(anyhow::anyhow!("synthetic app failure")),
    );
    assert_eq!(error.unwrap_err().to_string(), "synthetic app failure");
    assert!(inner.cached_app_session("one", auth).is_none());

    inner.store_app_session(&lease, app.clone());
    let changed = AppAuthority {
        logout_epoch: 1,
        ..auth
    };
    assert!(inner.cached_app_session("one", changed).is_none());
    assert!(inner.cached_app_session("one", auth).is_none());

    inner.store_app_session(&lease, app);
    core.fence_call_authority("one");
    assert!(!lease.current());
    assert!(inner.cached_app_session("one", auth).is_none());
    assert!(inner.app_sessions.lock().unwrap().is_empty());
}

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
    fail_live_sync: AtomicBool,
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
            fail_live_sync: AtomicBool::new(false),
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
        if route == CallHostRoute::LiveSync && self.fail_live_sync.load(Ordering::Acquire) {
            return Err(wechat_protocol::calls::host_transport::CallHostRpcError {
                stage: wechat_protocol::calls::host_transport::CallHostStage::Exchange,
                code: "timeout",
                value: None,
                crypto_step: None,
            }
            .into());
        }
        Ok(reply(route))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unconfirmed_read_only_sync_retains_receipt_without_blocking_known_room_cleanup() {
    let f = Fixture::new(Hold::AfterHandoff).await;
    f.host.fail_live_sync.store(true, Ordering::Release);
    f.host.release();
    let (_, result) = f.start(CallHostRoute::LiveSync, 1).await.unwrap();
    assert!(result.is_err());
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "needs_review");
    assert!(records[0].encrypted_request.is_some());
    assert!(records[0].encrypted_reply.is_none());
    assert!(!f
        .core
        .store
        .begin_call_action(
            "one",
            &f.transport.call_id,
            "1",
            9262,
            &records[0].request_digest,
            records[0].encrypted_request.as_deref().unwrap()
        )
        .unwrap());
    // Only the journal admission is exercised here. Actual cleanup still
    // requires the existing account, room, owner and terminal-request fences.
    let sealed = f
        .core
        .vault
        .seal("one", "synthetic-cleanup", &[8, 0])
        .unwrap();
    assert!(f
        .core
        .store
        .begin_call_action(
            "one",
            &f.transport.call_id,
            "cleanup",
            11421,
            &"b".repeat(64),
            &sealed
        )
        .unwrap());
    assert!(!f
        .core
        .store
        .begin_call("one", "unrelated", "generation")
        .unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conference_poll_wait_stops_when_media_or_owner_stops() {
    for media_finished in [true, false] {
        let f = Fixture::new(Hold::BeforeHandoff).await;
        let transport = f.transport.clone();
        let waiting = tokio::task::spawn_blocking(move || {
            transport.wait_for_conference_sync::<Vec<u8>>(std::future::pending())
        });
        if media_finished {
            f.transport.worker.media_stop.store(true, Ordering::Release);
        } else {
            f.transport.worker.stop.store(true, Ordering::Release);
        }
        let result = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().is::<CallLiveSyncInterrupted>());
        assert!(f.records().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_poll_wait_stops_when_direct_command_queued() {
    let f = Fixture::new(Hold::BeforeHandoff).await;
    let transport = f.transport.clone();
    let waiting = tokio::task::spawn_blocking(move || {
        transport.wait_for_direct_sync::<Vec<u8>>(std::future::pending())
    });
    let (reply, _rx) = oneshot::channel();
    f.transport
        .worker
        .direct_commands
        .lock()
        .unwrap()
        .push_back(DirectCommand {
            sets_camera: true,
            reply,
        });
    // Before joined_commands_ready is enabled (e.g. while ringing), queued commands must not
    // interrupt LiveSync into a tight poll spin.
    f.transport.worker.notify.notify_waiters();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!waiting.is_finished());
    // Once connected and draining commands, wait_for_direct_sync wakes immediately.
    f.transport
        .worker
        .joined_commands_ready
        .store(true, Ordering::Release);
    f.transport.worker.notify.notify_waiters();
    let result = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(result.unwrap_err().is::<CallLiveSyncInterrupted>());
    assert!(f.records().is_empty());
}
fn reply(route: CallHostRoute) -> Vec<u8> {
    let body = match route {
        CallHostRoute::AppAuth => {
            let auth = p::message(&[p::bytes(9, b"synthetic"), p::uint(11, 1)]).unwrap();
            let unify = p::message(&[p::bytes(2, &auth), p::bytes(3, &p::uint(1, 17))]).unwrap();
            p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &unify)]).unwrap()
        }
        CallHostRoute::LiveSync => p::bytes(2, b"synthetic-cursor"),
        CallHostRoute::Cancel => p::bytes(1, &p::message(&[p::uint(1, 7), p::uint(2, 9)]).unwrap()),
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
        let (temp, core) = synthetic_account();
        let store = &core.store;
        let (captured, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let backend = Arc::new(Backend {
            core: Arc::downgrade(&core),
            captured,
            cancelled: AtomicU32::new(0),
        });
        Core::configure_calls(&core, backend.clone()).unwrap();
        store.put_runtime("one", "history-caught-up", b"1").unwrap();
        let mut client = core
            .call_service("one")
            .unwrap()
            .connect("one", "transport-fixture", true)
            .await
            .unwrap();
        assert!(matches!(
            client.events.recv().await,
            Some(CallEvent::Hello { .. })
        ));
        core.call_service("one")
            .unwrap()
            .command(
                &client.client_id,
                CallCommand::Ready {
                    request_id: "ready".into(),
                    ready: true,
                    format: CallAudioFormat::default(),
                    video: None,
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
        let (video, _) = mpsc::sync_channel(1);
        let (video_keyframe_requests, _) = mpsc::sync_channel(1);
        let worker = Arc::new(Worker {
            identity: request.identity.clone(),
            peer_id: request.notification.caller_username.clone(),
            call_type: CallType::Voice,
            lease: lease.clone(),
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
            submission: SendAdmission::default(),
            metrics: MediaMetrics::default(),

            mutations_submitted: AtomicBool::new(false),
            joined_commands_ready: AtomicBool::new(false),
            incoming: None,
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

            worker,
            admission: ControlAdmission::Incoming(request.guard),
            runtime: tokio::runtime::Handle::current(),
            timeout: CALL_RPC_TIMEOUT,
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
                CallHostRoute::Invite | CallHostRoute::Cancel => p::bytes(1, b"forbidden"),
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

#[tokio::test]
async fn outgoing_app_auth_cleanup_admits_only_app_auth_without_an_owner_sink() {
    let mut f = Fixture::new(Hold::AfterHandoff).await;
    Arc::get_mut(&mut f.transport).unwrap().admission = ControlAdmission::OutgoingAppAuthCleanup;
    f.host.release();
    for route in [CallHostRoute::Invite, CallHostRoute::Cancel] {
        let (_, result) = f.start(route, 1).await.unwrap();
        assert!(result.unwrap_err().is::<CallRpcNotSubmitted>());
    }
    let (_, result) = f.start(CallHostRoute::AppAuth, 2).await.unwrap();
    assert_eq!(result.unwrap(), reply(CallHostRoute::AppAuth));
    assert_eq!(f.host.submitted.load(Ordering::Acquire), 1);
    let records = f.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].route, CallHostRoute::AppAuth.command());
}

fn dial_invite_reply(peer_status: u64) -> Vec<u8> {
    let member = |name: &str, id, status| {
        p::bytes(
            2,
            &p::message(&[
                p::bytes(1, name.as_bytes()),
                p::uint(2, id),
                p::uint(3, status),
                p::bytes(7, name.as_bytes()),
            ])
            .unwrap(),
        )
    };
    let roster = p::message(&[
        p::uint(1, 1),
        member(OWN, 0, 1),
        member(PEER, 1, peer_status),
    ])
    .unwrap();
    let engine = p::message(&[p::uint(5, 17), p::uint(6, 0x206), p::uint(7, 0x800000)]).unwrap();
    let channel = p::message(&[p::uint(1, 2), p::uint(2, 3)]).unwrap();
    let cap = p::message(&[
        p::uint(1, 1),
        p::bytes(
            2,
            &p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap(),
        ),
    ])
    .unwrap();
    // Deliberately omit relay endpoints: the ringing actor cannot open a socket.
    let strategy = p::message(&[p::bytes(13, &cap), p::uint(14, 0)]).unwrap();
    let av = p::message(&[
        p::uint(3, 1),
        p::bytes(4, &p::message(&[p::uint(1, 0), p::uint(2, 1)]).unwrap()),
        p::bytes(4, &p::message(&[p::uint(1, 1), p::uint(2, 0)]).unwrap()),
    ])
    .unwrap();
    let client = p::message(&[
        p::bytes(1, &strategy),
        p::bytes(2, &roster),
        p::bytes(3, &av),
        p::uint(5, 99),
        p::bytes(6, b"initial"),
    ])
    .unwrap();
    let body = p::message(&[
        p::uint(1, 7),
        p::uint(2, 9),
        p::uint(3, 0),
        p::bytes(4, b"group"),
        p::bytes(5, &client),
    ])
    .unwrap();
    p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &body)]).unwrap()
}

struct DialHost {
    peer_status: u64,
    fail_auth: bool,
    drop_owner: Mutex<Option<ControlConnection>>,
    submitted: Mutex<Vec<CallHostRoute>>,
}
impl TestCallIo for DialHost {
    fn call(
        &self,
        _: &mut AccountState,
        route: CallHostRoute,
        _: &[u8],
        handoff: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        handoff()?;
        self.submitted.lock().unwrap().push(route);
        match route {
            CallHostRoute::Invite => {
                drop(self.drop_owner.lock().unwrap().take());
                Ok(dial_invite_reply(self.peer_status))
            }
            CallHostRoute::AppAuth if self.fail_auth => {
                bail!("synthetic post-handoff AppAuth failure")
            }
            CallHostRoute::AppAuth | CallHostRoute::Cancel => Ok(reply(route)),
            _ => bail!("unexpected synthetic dial route {route:?}"),
        }
    }
}

struct DialFixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<NativeCallBackend>,
    service: Arc<CallService>,
    host: Arc<DialHost>,
}
impl DialFixture {
    fn new(peer_status: u64, fail_auth: bool) -> Self {
        let (temp, core) = synthetic_account();
        let host = Arc::new(DialHost {
            peer_status,
            fail_auth,
            drop_owner: Mutex::new(None),
            submitted: Mutex::new(vec![]),
        });
        let mut backend = NativeCallBackend::new(Arc::downgrade(&core));
        let inner = Arc::get_mut(&mut backend.inner).unwrap();
        inner.test_io = Some(host.clone());
        // Cancel immediately after auth instead of waiting or polling for a peer.
        inner.config.ring_timeout = Duration::ZERO;
        let backend = Arc::new(backend);
        let service = Arc::new(CallService::new(backend.clone()));
        Self {
            _temp: temp,
            core,
            backend,
            service,
            host,
        }
    }

    async fn dial(
        &self,
        request_id: &str,
        drop_owner: bool,
    ) -> (String, BackendOutcome, Vec<CallHostRoute>) {
        let mut observer = self
            .service
            .connect("one", "dial-observer", false)
            .await
            .unwrap();
        let mut owner = self
            .service
            .connect("one", "dial-owner", true)
            .await
            .unwrap();
        let owner_id = owner.client_id.clone();
        self.service
            .command(
                &owner_id,
                CallCommand::Ready {
                    request_id: "ready".into(),
                    ready: true,
                    format: CallAudioFormat::default(),
                    video: None,
                    scope: None,
                },
            )
            .await
            .unwrap();
        loop {
            if let CallEvent::CommandResult { status, .. } = owner.events.try_recv().unwrap() {
                assert_eq!(status, CallCommandStatus::Accepted);
                break;
            }
        }
        let _owner = if drop_owner {
            *self.host.drop_owner.lock().unwrap() = Some(owner);
            None
        } else {
            Some(owner)
        };
        self.service
            .command(
                &owner_id,
                CallCommand::Dial {
                    request_id: request_id.into(),
                    target: CallTarget::Direct {
                        contact_id: PEER.into(),
                    },
                    call_type: CallType::Voice,
                    camera_enabled: None,
                    muted: None,
                },
            )
            .await
            .unwrap();
        let (call_id, outcome) = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = observer.events.recv().await {
                if let CallEvent::State { call } = event {
                    if matches!(call.state, CallState::Ended | CallState::Uncertain) {
                        let worker = self.backend.inner.active.lock().unwrap().clone().unwrap();
                        assert_eq!(worker.identity.call_id(), call.call_id);
                        return (call.call_id, worker.wait().await.unwrap());
                    }
                }
            }
            panic!("synthetic dial observer closed");
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "synthetic dial did not finish after {:?}",
                self.host.submitted.lock().unwrap()
            )
        });
        let routes = std::mem::take(&mut *self.host.submitted.lock().unwrap());
        (call_id, outcome, routes)
    }
}

#[tokio::test]
async fn cold_dial_authenticates_after_invite_and_warm_dial_reuses_cached_app() {
    use CallHostRoute::{AppAuth, Cancel, Invite};
    let f = DialFixture::new(2, false);
    let (_, outcome, routes) = f.dial("cold", false).await;
    assert!(matches!(outcome, BackendOutcome::Ended));
    assert_eq!(routes, [Invite, AppAuth, Cancel]);
    let lease = f.core.call_authority("one").unwrap();
    let auth = app_authority(&f.core, &lease);
    assert_eq!(
        f.backend
            .inner
            .cached_app_session("one", auth)
            .unwrap()
            .app_uin(),
        17
    );

    let (_, outcome, routes) = f.dial("warm", false).await;
    assert!(matches!(outcome, BackendOutcome::Ended));
    assert_eq!(routes, [Invite, Cancel]);
}

#[tokio::test]
async fn post_handoff_app_auth_failure_allows_ringing_cancel() {
    use CallHostRoute::{AppAuth, Cancel, Invite};
    let f = DialFixture::new(2, true);
    let (call_id, outcome, routes) = f.dial("ringing-failure", false).await;
    assert!(matches!(outcome, BackendOutcome::Ended));
    assert_eq!(routes, [Invite, AppAuth, Cancel]);
    let records = f.core.store.call_action_records("one", &call_id).unwrap();
    assert_eq!(
        records
            .iter()
            .map(|r| (r.route, r.outcome.as_str()))
            .collect::<Vec<_>>(),
        [
            (Invite.command(), "received"),
            (AppAuth.command(), "needs_review"),
            (Cancel.command(), "received"),
        ]
    );
    // Cancel's journal claim requires an active session, and its confirmed
    // reply closes the session without changing the unconfirmed AppAuth receipt.
    let status = f
        .core
        .store
        .call_session_status("one", &call_id)
        .unwrap()
        .unwrap();
    assert!(status.local_closed && status.remote_terminal_confirmed);
    assert!(!f.core.store.has_unresolved_call("one").unwrap());
}

#[tokio::test]
async fn terminal_pending_app_auth_can_submit_after_owner_revocation_but_failure_is_uncertain() {
    use CallHostRoute::{AppAuth, Invite};
    // A busy peer classifies the invite reply as TerminalPending, not Ringing.
    let f = DialFixture::new(6, true);
    let (call_id, outcome, routes) = f.dial("terminal-auth-failure", true).await;
    assert!(matches!(outcome, BackendOutcome::Uncertain));
    assert_eq!(routes, [Invite, AppAuth]);
    let records = f.core.store.call_action_records("one", &call_id).unwrap();
    assert_eq!(records[1].route, AppAuth.command());
    assert_eq!(records[1].outcome, "needs_review");
    let status = f
        .core
        .store
        .call_session_status("one", &call_id)
        .unwrap()
        .unwrap();
    assert!(!status.remote_terminal_confirmed);
    if !status.local_closed {
        let request = records[0].encrypted_request.as_deref().unwrap();
        assert_eq!(
            f.core
                .store
                .begin_call_action(
                    "one",
                    &call_id,
                    "cleanup",
                    Invite.command(),
                    &records[0].request_digest,
                    request
                )
                .unwrap_err()
                .to_string(),
            "call_needs_review"
        );
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
