//! Account-free actor fixtures. Host responses follow the selected schemas;
//! Control fixtures keep AV unready. The separate generated-media fixture uses
//! only a synthetic host and a localhost UDP peer, without external transport.
use super::*;
use crate::{call_types::*, calls::service::ControlConnection, Store};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{collections::BTreeMap, sync::Condvar};
use wechat_protocol::{
    calls::app_frame::TaskFrame,
    wire::{SyncBatch, SyncCommand},
    AuthSnapshot, DeviceProfile,
};

mod media_loopback;

#[derive(Clone, Copy)]
struct ReadyMedia {
    endpoint: SocketAddrV4,
    key: [u8; 32],
    frames_per_packet: u8,
}

const OWN: &str = "synthetic_self";
const PEER: &str = "synthetic_peer";
const ROOM: u64 = 7;
const ROOM_KEY: u64 = 9;
const PEER_MEMBER: u16 = 7;

#[derive(Clone, Copy)]
enum Case {
    Normal,
    OtherDevice,
    HoldAuth,
    HoldJoinedSync,
    ChangedMember,
    LostReject,
    HoldJoin,
    HoldReject,
    HoldChangedAck,
    HoldPreAuth,
    HoldJoinReply,
    LostOwnerExit,
}
struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
    entered: Notify,
}
impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn wait(&self) -> Result<()> {
        self.entered.notify_one();
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(2),
                |released| !*released,
            )
            .unwrap();
        ensure!(*released, "Synthetic Auth gate timed out");
        Ok(())
    }
}
struct Host {
    case: Case,
    ready_media: Option<ReadyMedia>,
    routes: Mutex<Vec<(CallHostRoute, Option<u32>)>>,
    gate: Gate,
    sync_held: AtomicBool,
    exit_members: Mutex<Vec<u32>>,
}
impl Host {
    fn new(case: Case, ready_media: Option<ReadyMedia>) -> Arc<Self> {
        Arc::new(Self {
            case,
            ready_media,
            routes: Mutex::new(vec![]),
            sync_held: AtomicBool::new(false),
            exit_members: Mutex::new(vec![]),
            gate: Gate {
                released: Mutex::new(false),
                changed: Condvar::new(),
                entered: Notify::new(),
            },
        })
    }
    fn own_member(&self) -> u16 {
        if matches!(self.case, Case::ChangedMember | Case::HoldChangedAck) {
            4
        } else {
            0
        }
    }
    fn seen(&self) -> Vec<(CallHostRoute, Option<u32>)> {
        self.routes.lock().unwrap().clone()
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
            common.base_request.is_some() && common.unknown_fields.is_empty(),
            "Synthetic common shape"
        );
        let body = common.body.context("Synthetic common body")?;
        let fields = p::decode(&body)?;
        let command = if route == CallHostRoute::Transfer {
            Some(u32::try_from(p::scalar(&fields, 2)?)?)
        } else {
            None
        };
        let joined = self
            .routes
            .lock()
            .unwrap()
            .iter()
            .any(|(_, command)| *command == Some(3361));
        if route == CallHostRoute::LiveSync
            && joined
            && matches!(self.case, Case::HoldJoinedSync)
            && !self.sync_held.swap(true, Ordering::AcqRel)
        {
            self.gate.wait()?;
        }
        if route == CallHostRoute::Transfer
            && ((command == Some(3361) && matches!(self.case, Case::HoldJoin))
                || (command == Some(3095) && matches!(self.case, Case::HoldReject)))
        {
            self.gate.wait()?;
        }
        if route == CallHostRoute::RingingAck && matches!(self.case, Case::HoldChangedAck) {
            self.gate.wait()?;
        }
        if route == CallHostRoute::AppAuth && matches!(self.case, Case::HoldPreAuth) {
            self.gate.wait()?;
        }
        handoff()?;
        self.routes.lock().unwrap().push((route, command));
        if route == CallHostRoute::Transfer
            && command == Some(3361)
            && matches!(self.case, Case::HoldJoinReply | Case::LostOwnerExit)
        {
            self.gate.wait()?;
        }
        if route == CallHostRoute::Transfer && command == Some(3380) {
            let frame = TaskFrame::decode(p::data(&fields, 5)?, 3380)?;
            let clear = wechat_protocol::calls::app_frame::lz4_decode(&frame.payload, 64 * 1024)?;
            let cloud = p::decode(&clear)?;
            let exit = p::decode(p::data(&cloud, 2)?)?;
            self.exit_members
                .lock()
                .unwrap()
                .push(u32::try_from(p::scalar(&exit, 5)?)?);
            if matches!(self.case, Case::LostOwnerExit) {
                bail!("Synthetic missing Exit receipt");
            }
        }
        state.cookie = vec![route.command() as u8];
        match route {
            CallHostRoute::AppAuth => {
                if matches!(self.case, Case::HoldAuth) {
                    self.gate.wait()?;
                }
                Ok(auth_reply())
            }
            CallHostRoute::RingingAck => Ok(ack_reply(
                self.own_member(),
                if matches!(self.case, Case::OtherDevice) {
                    1
                } else {
                    3
                },
            )),
            CallHostRoute::LiveSync => Ok(host_reply(&p::bytes(2, b"cursor"))),
            CallHostRoute::Transfer => match command {
                Some(3361) => Ok(join_reply_with_media(3, self.ready_media)),
                Some(3380) => Ok(wrapped_reply(
                    3380,
                    &p::message(&[p::uint(1, ROOM), p::uint(2, ROOM_KEY), p::uint(3, 3)])?,
                )),
                Some(3095) if matches!(self.case, Case::LostReject) => {
                    bail!("Synthetic missing Reject receipt")
                }
                Some(3095) => Ok(wrapped_reply(
                    3095,
                    &p::message(&[p::uint(1, ROOM), p::uint(2, ROOM_KEY), p::bytes(3, &[])])?,
                )),
                _ => bail!("Unexpected synthetic transfer"),
            },
            _ => bail!("Unexpected synthetic host route"),
        }
    }
}
fn host_reply(body: &[u8]) -> Vec<u8> {
    p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, body)]).unwrap()
}
fn auth_reply() -> Vec<u8> {
    let auth = p::message(&[p::bytes(9, b"synthetic-server"), p::uint(11, 1)]).unwrap();
    let unify = p::message(&[p::bytes(2, &auth), p::bytes(3, &p::uint(1, 17))]).unwrap();
    host_reply(&p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &unify)]).unwrap())
}
fn member(name: &str, openid: &[u8], id: u16, status: u32) -> Vec<u8> {
    p::message(&[
        p::bytes(1, openid),
        p::uint(2, u64::from(id)),
        p::uint(3, u64::from(status)),
        p::bytes(7, name.as_bytes()),
    ])
    .unwrap()
}
fn members(sequence: u32, own_member: u16, own_status: u32) -> Vec<u8> {
    p::message(&[
        p::uint(1, u64::from(sequence)),
        p::bytes(2, &member(OWN, b"self-open", own_member, own_status)),
        p::bytes(2, &member(PEER, b"peer-open", PEER_MEMBER, 1)),
    ])
    .unwrap()
}
fn ack_reply(own_member: u16, own_status: u32) -> Vec<u8> {
    // AckRsp6 is MemList(seq1/member2), unlike LiveSync's item10 payload.
    host_reply(&p::bytes(
        1,
        &p::message(&[
            p::uint(1, ROOM),
            p::uint(2, ROOM_KEY),
            p::uint(3, u64::from(own_member)),
            p::uint(4, 99),
            p::bytes(5, b"ack-cursor"),
            p::bytes(
                6,
                &if own_status == 3 {
                    p::message(&[p::uint(1, 0), p::bytes(3, b"")]).unwrap()
                } else {
                    members(1, own_member, own_status)
                },
            ),
            p::bytes(7, &p::uint(14, 0)),
        ])
        .unwrap(),
    ))
}
fn wrapped_reply(command: u32, body: &[u8]) -> Vec<u8> {
    let frame = TaskFrame {
        command,
        integers: BTreeMap::from([
            (4, 0),
            (7, 0),
            (8, 0),
            (21, 0),
            (22, 17),
            (23, 0),
            (25, 0),
            (26, 0),
        ]),
        strings: BTreeMap::new(),
        payload: p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 17), p::bytes(3, body)]).unwrap(),
    }
    .encode()
    .unwrap();
    host_reply(&p::message(&[p::uint(1, 0), p::bytes(2, &frame)]).unwrap())
}
fn join_reply_with_media(own_member: u16, media: Option<ReadyMedia>) -> Vec<u8> {
    let format = (u64::from(media.map_or(2, |media| media.frames_per_packet)) << 8) | 6;
    let engine = p::message(&[p::uint(5, 17), p::uint(6, format), p::uint(7, 0x800000)]).unwrap();
    let channel = p::message(&[p::uint(1, 2), p::uint(2, 3)]).unwrap();
    let capabilities = p::message(&[
        p::uint(1, 1),
        p::bytes(
            2,
            &p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap(),
        ),
    ])
    .unwrap();
    let endpoint = media.map_or_else(
        || SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 6000),
        |media| media.endpoint,
    );
    let address = p::message(&[
        p::bytes(1, endpoint.ip().to_string().as_bytes()),
        p::uint(2, u64::from(endpoint.port())),
    ])
    .unwrap();
    let strategy = p::message(&[
        p::bytes(10, &media.map_or([7; 32], |media| media.key)),
        p::bytes(13, &capabilities),
        p::uint(14, 0),
        p::bytes(
            15,
            &p::message(&[p::uint(1, 1), p::bytes(4, &address)]).unwrap(),
        ),
    ])
    .unwrap();
    // Only the dedicated localhost fixture supplies a ready media endpoint.
    // Other actor fixtures still cannot start a media socket.
    let av_status = u64::from(media.is_some());
    let av = p::message(&[
        p::uint(3, 1),
        p::bytes(
            4,
            &p::message(&[p::uint(1, u64::from(own_member)), p::uint(2, av_status)]).unwrap(),
        ),
        p::bytes(
            4,
            &p::message(&[p::uint(1, u64::from(PEER_MEMBER)), p::uint(2, av_status)]).unwrap(),
        ),
    ])
    .unwrap();
    wrapped_reply(
        3361,
        &p::message(&[
            p::uint(1, ROOM),
            p::uint(2, ROOM_KEY),
            p::uint(3, u64::from(own_member)),
            p::bytes(4, &strategy),
            p::uint(8, 2),
            p::bytes(9, &av),
            p::uint(11, 99),
            p::bytes(12, b"join-cursor"),
            p::bytes(13, &members(2, own_member, 1)),
        ])
        .unwrap(),
    )
}

struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<NativeCallBackend>,
    host: Arc<Host>,
}
impl Fixture {
    fn new(case: Case) -> Self {
        Self::with_media(case, None)
    }
    fn with_media(case: Case, ready_media: Option<ReadyMedia>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        let mut snapshot = AuthSnapshot::default();
        snapshot.response = p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 0)]).unwrap();
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
            auth_snapshot: snapshot,
            long_host: "synthetic-unused.weixin.qq.com".into(),
            short_host: "synthetic-unused.weixin.qq.com".into(),
        };
        core.save_state("one", &state).unwrap();
        assert!(core.call_authority("one").is_ok());
        let mut config =
            NativeCallConfig::receive_only("one".into(), independent_voice_parameters().unwrap());
        config.transmit_scope = NativeTransmitScope::IsolatedTest {
            peer_id: PEER.into(),
            intent_id: if ready_media.is_some() {
                crypto::random::<32>().unwrap()
            } else {
                [9; 32]
            },
            expires_at_unix_ms: unix_ms().unwrap() + 60_000,
            allow_known_audio_overlap: ready_media.is_some(),
        };
        config.max_duration = Duration::from_secs(if ready_media.is_some() { 6 } else { 2 });
        if ready_media.is_some() {
            config.peer_idle_timeout = Duration::from_millis(500);
        }
        let mut backend = NativeCallBackend::new(Arc::downgrade(&core), config).unwrap();
        let host = Host::new(case, ready_media);
        Arc::get_mut(&mut backend.inner).unwrap().test_io = Some(host.clone());
        let backend = Arc::new(backend);
        Core::configure_calls(&core, backend.clone()).unwrap();
        store.put_runtime("one", "history-caught-up", b"1").unwrap();
        Self {
            _temp: temp,
            core,
            backend,
            host,
        }
    }
    fn batch(&self, cancel: bool, member: u32) -> SyncBatch {
        let now = chrono::Utc::now().timestamp() as u64;
        let inner = if cancel {
            p::message(&[
                p::uint(1, ROOM),
                p::uint(2, ROOM_KEY),
                p::uint(3, u64::from(member)),
                p::uint(4, 2),
            ])
            .unwrap()
        } else {
            p::message(&[
                p::uint(1, ROOM),
                p::uint(2, ROOM_KEY),
                p::uint(3, 0),
                p::uint(4, 2),
                p::bytes(5, b"peer-open"),
                p::bytes(6, b"self-open"),
                p::bytes(7, PEER.as_bytes()),
                p::bytes(8, OWN.as_bytes()),
                p::bytes(10, b"wechat"),
                p::bytes(11, b"voip-2p"),
            ])
            .unwrap()
        };
        let conference =
            p::message(&[p::uint(1, if cancel { 3 } else { 2 }), p::bytes(2, &inner)]).unwrap();
        let mut carrier = vec![p::bytes(if cancel { 2 } else { 6 }, &conference)];
        if !cancel {
            // An invitation can leave caller membership for the ACK and
            // retain additional carrier data outside the conference envelope.
            carrier.extend([p::bytes(8, b"opaque"), p::uint(9, 1), p::uint(10, 0)]);
        }
        let carrier = p::message(&carrier).unwrap();
        let child = if cancel { "cancel" } else { "invite" };
        let xml = format!(
            "<sysmsg type='ilinkvoip'><voipmt><{child}>{}</{child}></voipmt></sysmsg>",
            STANDARD.encode(carrier)
        );
        let payload = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::bytes(1, b"synthetic-system")),
            p::bytes(3, &p::bytes(1, OWN.as_bytes())),
            p::uint(4, 10002),
            p::bytes(5, &p::bytes(1, xml.as_bytes())),
            p::uint(6, 0),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, now),
            p::uint(12, 100 + u64::from(cancel) * 10 + u64::from(member)),
        ])
        .unwrap();
        SyncBatch {
            code: 0,
            flags: 0,
            cursor: vec![u8::from(cancel), member as u8],
            commands: vec![SyncCommand { id: 5, payload }],
            continuation: false,
        }
    }
    fn stage(&self, cancel: bool, member: u32) {
        self.core
            .commit_sync_batch("one", self.batch(cancel, member), "synthetic", "synthetic")
            .unwrap();
    }
    fn cancellation(&self, member: u32) -> Arc<IncomingNotification> {
        let batch = self.batch(true, member);
        let raw = batch.commands[0].payload.clone();
        let message = wechat_protocol::wire::decode_add_message(&raw, OWN).unwrap();
        let control = crate::calls::incoming::decode_system_control(&message)
            .unwrap()
            .unwrap();
        Arc::new(IncomingNotification {
            lease: self.core.cached_call_authority("one").unwrap(),
            receipt_id: format!("synthetic-cancel-{member}"),
            source_created_at: u64::from(message.created_at),
            received_at_unix_ms: unix_ms().unwrap(),
            own_username: OWN.into(),
            caller_username: PEER.into(),
            original_message: raw.into(),
            control,
            core: Arc::downgrade(&self.core),
            header_from: message.from,
        })
    }
    async fn client(&self) -> ControlConnection {
        let mut client = self
            .core
            .call_service()
            .connect("one", "fixture", true)
            .await
            .unwrap();
        assert!(matches!(
            client.events.recv().await,
            Some(CallEvent::Hello { .. })
        ));
        self.core
            .call_service()
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
        assert_eq!(
            result(&mut client, "ready").await,
            CallCommandStatus::Accepted
        );
        client
    }
    async fn snapshot(&self) -> CallSnapshot {
        self.core
            .call_service()
            .current("one", "fixture")
            .await
            .unwrap()
    }
    async fn offer(&self) -> CallOffer {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(offer) = self.snapshot().await.current_offer {
                    return offer;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("native offer deadline")
    }
    async fn finished(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let done = self
                    .backend
                    .inner
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|worker| *worker.done.lock().unwrap());
                let snapshot = self.snapshot().await;
                let terminal = match done {
                    Some(BackendOutcome::Ended) => !snapshot.availability.busy,
                    Some(BackendOutcome::Uncertain) => snapshot.availability.busy,
                    _ => false,
                };
                if terminal {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("native worker finish deadline");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.gate.release();
        if let Some(worker) = self.backend.inner.active.lock().unwrap().clone() {
            worker.stop.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while worker.done.lock().unwrap().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
async fn result(client: &mut ControlConnection, request: &str) -> CallCommandStatus {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match client.events.recv().await {
                Some(CallEvent::CommandResult {
                    request_id, status, ..
                }) if request_id == request => return status,
                Some(_) => {}
                None => panic!("control closed before native result"),
            }
        }
    })
    .await
    .expect("native command result deadline")
}

async fn owned_state(
    client: &mut ControlConnection,
    expected: CallState,
    revision: u32,
) -> CallView {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match client.events.recv().await {
                Some(CallEvent::State { call })
                    if call.state == expected && call.revision >= revision =>
                {
                    return call
                }
                Some(_) => {}
                None => panic!("control closed before native state"),
            }
        }
    })
    .await
    .expect("native owner state deadline")
}

#[tokio::test]
async fn consumed_incoming_scope_can_answer_once_then_exit_without_media() {
    let f = Fixture::new(Case::HoldJoinedSync);
    let mut client = f.client().await;
    f.stage(false, 0);
    let offer = f.offer().await;
    assert!(f.backend.inner.test_spent.load(Ordering::Acquire));
    assert!(f.backend.capabilities("one").direct_audio);
    let initial = f.host.seen();
    assert_eq!(
        initial[0..3],
        [
            (CallHostRoute::AppAuth, None),
            (CallHostRoute::RingingAck, None),
            (CallHostRoute::LiveSync, None)
        ]
    );
    assert!(!initial
        .iter()
        .any(|(route, _)| *route == CallHostRoute::Transfer));
    f.core
        .call_service()
        .command(
            &client.client_id,
            CallCommand::Answer {
                request_id: "answer".into(),
                call_id: offer.call_id.clone(),
                expected_revision: offer.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut client, "answer").await,
        CallCommandStatus::Accepted
    );
    let call = owned_state(&mut client, CallState::Connecting, offer.revision + 2).await;
    // A matching Cancel after Join must not retire or fence this owned call.
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    f.stage(true, 3);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
            if worker.incoming.as_ref().unwrap().cancellation_pending() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.host.gate.release();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(f.snapshot().await.availability.busy);
    assert!(f
        .backend
        .inner
        .active
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .done
        .lock()
        .unwrap()
        .is_none());
    f.core
        .call_service()
        .command(
            &client.client_id,
            CallCommand::Hangup {
                request_id: "end".into(),
                call_id: call.call_id,
                expected_revision: call.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut client, "end").await,
        CallCommandStatus::Accepted
    );
    f.finished().await;
    assert!(!f.snapshot().await.availability.busy);
    assert!(!f.backend.capabilities("one").direct_audio);
    let seen = f.host.seen();
    assert_eq!(
        seen.iter()
            .filter(|(_, command)| *command == Some(3361))
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|(_, command)| *command == Some(3380))
            .count(),
        1
    );
    assert!(!seen
        .iter()
        .any(|(route, _)| matches!(route, CallHostRoute::Invite | CallHostRoute::Cancel)));
    assert!(!f.core.store.has_unresolved_call("one").unwrap());
    let actions = f
        .core
        .store
        .call_action_records("one", &offer.call_id)
        .unwrap();
    let unsent = actions
        .iter()
        .filter(|action| action.outcome == "not_submitted")
        .collect::<Vec<_>>();
    assert_eq!(unsent.len(), 1);
    assert_eq!(unsent[0].route, 9262);
    assert!(unsent[0].encrypted_request.is_some() && unsent[0].encrypted_reply.is_none());
}

#[tokio::test]
async fn cancellation_queued_during_auth_prevents_ack() {
    let f = Fixture::new(Case::HoldAuth);
    let _client = f.client().await;
    f.stage(false, 0);
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    f.stage(true, 0);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
            if worker.incoming.as_ref().unwrap().cancellation_pending() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.host.gate.release();
    f.finished().await;
    assert_eq!(f.host.seen(), [(CallHostRoute::AppAuth, None)]);
    assert!(!f.snapshot().await.availability.busy);
}

#[tokio::test]
async fn ack_other_device_terminal_never_publishes_offer_or_sends_exit() {
    let f = Fixture::new(Case::OtherDevice);
    let _client = f.client().await;
    f.stage(false, 0);
    f.finished().await;
    assert_eq!(
        f.host.seen(),
        [
            (CallHostRoute::AppAuth, None),
            (CallHostRoute::RingingAck, None)
        ]
    );
    assert!(!f.snapshot().await.availability.busy);
    assert!(f.snapshot().await.current_offer.is_none());
}

#[tokio::test]
async fn cancellation_uses_current_ack_member_and_preserves_original_notification() {
    let f = Fixture::new(Case::ChangedMember);
    let _client = f.client().await;
    f.stage(false, 0);
    let _offer = f.offer().await;
    f.stage(true, 0);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(f.snapshot().await.current_offer.is_some());
    f.stage(true, 4);
    f.finished().await;
    assert!(!f.snapshot().await.availability.busy);
    assert!(!f
        .host
        .seen()
        .iter()
        .any(|(route, _)| *route == CallHostRoute::Transfer));
}

#[tokio::test]
async fn explicit_reject_requires_receipt_and_lost_reply_holds_slot() {
    for (case, expected) in [
        (Case::Normal, CallCommandStatus::Accepted),
        (Case::LostReject, CallCommandStatus::Accepted),
    ] {
        let f = Fixture::new(case);
        let mut client = f.client().await;
        f.stage(false, 0);
        let offer = f.offer().await;
        f.core
            .call_service()
            .command(
                &client.client_id,
                CallCommand::Reject {
                    request_id: "reject".into(),
                    call_id: offer.call_id,
                    expected_revision: offer.revision,
                },
            )
            .await
            .unwrap();
        assert_eq!(result(&mut client, "reject").await, expected);
        f.finished().await;
        let snapshot = f.snapshot().await;
        assert_eq!(snapshot.availability.busy, matches!(case, Case::LostReject));
        if matches!(case, Case::LostReject) {
            owned_state(&mut client, CallState::Uncertain, 0).await;
        }
        assert_eq!(
            f.host
                .seen()
                .iter()
                .filter(|(_, command)| *command == Some(3095))
                .count(),
            1
        );
    }
}

async fn held_owner_action_with_cancel(case: Case, member: u32) {
    let f = Fixture::new(case);
    let mut client = f.client().await;
    f.stage(false, 0);
    let offer = f.offer().await;
    let rejecting = matches!(case, Case::HoldReject);
    let command = if rejecting {
        CallCommand::Reject {
            request_id: "owner".into(),
            call_id: offer.call_id.clone(),
            expected_revision: offer.revision,
        }
    } else {
        CallCommand::Answer {
            request_id: "owner".into(),
            call_id: offer.call_id.clone(),
            expected_revision: offer.revision,
        }
    };
    f.core
        .call_service()
        .command(&client.client_id, command)
        .await
        .unwrap();
    assert_eq!(
        result(&mut client, "owner").await,
        CallCommandStatus::Accepted
    );
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
    // Exercise a raw control already admitted during member cutover. The
    // broker separately rejects newly unmatched notes once it enters Ending.
    let mut cancelling = f
        .backend
        .cancel_incoming(worker.identity.clone(), f.cancellation(member));
    assert!(cancelling
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        .is_pending());
    assert_eq!(
        worker.incoming.as_ref().unwrap().cancellation_pending(),
        member == 0
    );
    f.host.gate.release();
    let disposition = tokio::time::timeout(Duration::from_secs(2), cancelling)
        .await
        .unwrap()
        .unwrap();
    if member == 0 {
        assert_eq!(disposition, IncomingCancellation::Ended);
    } else {
        assert_eq!(disposition, IncomingCancellation::Ignored);
    }
    if member != 0 && !rejecting {
        let call = owned_state(&mut client, CallState::Connecting, offer.revision + 2).await;
        assert!(f.snapshot().await.availability.busy);
        assert!(worker.done.lock().unwrap().is_none());
        f.core
            .call_service()
            .command(
                &client.client_id,
                CallCommand::Hangup {
                    request_id: "end".into(),
                    call_id: call.call_id,
                    expected_revision: call.revision,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result(&mut client, "end").await,
            CallCommandStatus::Accepted
        );
    }
    f.finished().await;
    assert!(!f.snapshot().await.availability.busy);
    let operation = if rejecting { 3095 } else { 3361 };
    assert_eq!(
        f.host
            .seen()
            .iter()
            .filter(|(_, command)| *command == Some(operation))
            .count(),
        usize::from(member != 0)
    );
    assert!(!f.core.store.has_unresolved_call("one").unwrap());
}

#[tokio::test]
async fn foreign_member_cancel_does_not_interrupt_unsent_join() {
    held_owner_action_with_cancel(Case::HoldJoin, 9).await;
}

#[tokio::test]
async fn foreign_member_cancel_does_not_interrupt_unsent_reject() {
    held_owner_action_with_cancel(Case::HoldReject, 9).await;
}

#[tokio::test]
async fn current_member_cancel_prevents_join_submission() {
    held_owner_action_with_cancel(Case::HoldJoin, 0).await;
}

#[tokio::test]
async fn current_member_cancel_prevents_reject_submission() {
    held_owner_action_with_cancel(Case::HoldReject, 0).await;
}

#[tokio::test]
async fn future_member_cancel_waits_for_authenticated_ack_binding() {
    let f = Fixture::new(Case::HoldChangedAck);
    let _client = f.client().await;
    f.stage(false, 0);
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
    f.stage(true, 4);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!worker.incoming.as_ref().unwrap().cancellation_pending());
    assert!(worker.done.lock().unwrap().is_none());
    assert!(f.snapshot().await.current_offer.is_none());
    f.host.gate.release();
    f.finished().await;
    assert_eq!(
        f.host.seen(),
        [
            (CallHostRoute::AppAuth, None),
            (CallHostRoute::RingingAck, None)
        ]
    );
    assert!(!f.snapshot().await.availability.busy);
}

#[tokio::test]
async fn foreign_member_cancel_does_not_spend_pre_auth_preparation() {
    let f = Fixture::new(Case::HoldPreAuth);
    let _client = f.client().await;
    f.stage(false, 0);
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
    let mut cancelling = f
        .backend
        .cancel_incoming(worker.identity.clone(), f.cancellation(9));
    assert!(cancelling
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        .is_pending());
    assert!(!worker.incoming.as_ref().unwrap().cancellation_pending());
    f.host.gate.release();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), cancelling)
            .await
            .unwrap()
            .unwrap(),
        IncomingCancellation::Ignored
    );
    let _offer = f.offer().await;
    f.stage(true, 0);
    f.finished().await;
    assert!(!f.snapshot().await.availability.busy);
    let routes = f.host.seen();
    for route in [CallHostRoute::AppAuth, CallHostRoute::RingingAck] {
        assert_eq!(routes.iter().filter(|(seen, _)| *seen == route).count(), 1);
    }
    assert!(!routes
        .iter()
        .any(|(route, _)| *route == CallHostRoute::Transfer));
}

#[tokio::test]
async fn unknown_cancel_fields_fence_even_a_foreign_member() {
    let f = Fixture::new(Case::Normal);
    let (mailbox, _) = Mailbox::new(ROOM, ROOM_KEY, 0);
    let mut note = f.cancellation(9);
    assert!(!mailbox.fences_submission(&note));
    Arc::get_mut(&mut note)
        .unwrap()
        .control
        .conference
        .unknown_fields = p::decode(&p::uint(99, 1)).unwrap();
    assert!(mailbox.fences_submission(&note));
}

#[derive(Clone, Copy)]
enum JoinReplyChange {
    Disconnect,
    DisconnectAndFence,
    Uncertain,
    UncertainThenDisconnect,
}

async fn held_join_reply_change(case: Case, change: JoinReplyChange) {
    let f = Fixture::new(case);
    let mut client = f.client().await;
    let mut observer = f
        .core
        .call_service()
        .connect("one", "observer", false)
        .await
        .unwrap();
    f.stage(false, 0);
    let offer = f.offer().await;
    f.core
        .call_service()
        .command(
            &client.client_id,
            CallCommand::Answer {
                request_id: "answer".into(),
                call_id: offer.call_id.clone(),
                expected_revision: offer.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut client, "answer").await,
        CallCommandStatus::Accepted
    );
    tokio::time::timeout(Duration::from_secs(2), f.host.gate.entered.notified())
        .await
        .unwrap();
    let worker = f.backend.inner.active.lock().unwrap().clone().unwrap();
    assert!(f
        .host
        .seen()
        .iter()
        .any(|(_, command)| *command == Some(3361)));
    if matches!(
        change,
        JoinReplyChange::Uncertain | JoinReplyChange::UncertainThenDisconnect
    ) {
        f.core
            .call_service()
            .incoming_uncertain(worker.lease.authority(), &worker.identity)
            .unwrap();
    }
    if !matches!(change, JoinReplyChange::Uncertain) {
        f.core.call_service().disconnect(&client.client_id).await;
    }
    if matches!(change, JoinReplyChange::DisconnectAndFence) {
        f.core.fence_call_authority("one");
    }
    f.host.gate.release();
    f.finished().await;
    let cleanup = matches!(change, JoinReplyChange::Disconnect);
    let confirmed = cleanup && !matches!(case, Case::LostOwnerExit);
    let outcome = *worker.done.lock().unwrap();
    assert!(if confirmed {
        matches!(outcome, Some(BackendOutcome::Ended))
    } else {
        matches!(outcome, Some(BackendOutcome::Uncertain))
    });
    assert_eq!(f.snapshot().await.availability.busy, !confirmed);
    let seen = f.host.seen();
    assert_eq!(
        seen.iter()
            .filter(|(_, operation)| *operation == Some(3361))
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|(_, operation)| *operation == Some(3380))
            .count(),
        usize::from(cleanup)
    );
    assert_eq!(
        f.host.exit_members.lock().unwrap().as_slice(),
        if cleanup { &[3][..] } else { &[][..] }
    );
    assert_eq!(
        worker.metrics.rx_delivered_frames.load(Ordering::Acquire),
        0
    );
    assert_eq!(
        worker
            .metrics
            .tx_submitted_datagrams
            .load(Ordering::Acquire),
        0
    );
    assert_eq!(worker.metrics.tx_enqueued_frames.load(Ordering::Acquire), 0);
    while let Ok(event) = observer.events.try_recv() {
        assert!(!matches!(&event, CallEvent::State { call } if call.state == CallState::Connected));
        assert!(!matches!(event, CallEvent::TrackAdded { .. }));
    }
    let records = f
        .core
        .store
        .call_action_records("one", &offer.call_id)
        .unwrap();
    assert!(records
        .iter()
        .any(|record| record.route == 11421 && record.encrypted_reply.is_some()));
    assert_eq!(f.core.store.has_unresolved_call("one").unwrap(), !confirmed);
}

#[tokio::test]
async fn owner_disconnect_after_join_handoff_exits_once_with_returned_member() {
    // This pre-media in-flight Answer exception does not change established
    // calls' normal disconnect grace or create a controller that can resume it.
    held_join_reply_change(Case::HoldJoinReply, JoinReplyChange::Disconnect).await;
}

#[tokio::test]
async fn lost_owner_cleanup_failure_keeps_unknown_without_retry() {
    held_join_reply_change(Case::LostOwnerExit, JoinReplyChange::Disconnect).await;
}

#[tokio::test]
async fn owner_disconnect_and_revoked_authority_never_submits_cleanup() {
    held_join_reply_change(Case::HoldJoinReply, JoinReplyChange::DisconnectAndFence).await;
}

#[tokio::test]
async fn same_authority_uncertainty_without_owner_loss_never_submits_cleanup() {
    held_join_reply_change(Case::HoldJoinReply, JoinReplyChange::Uncertain).await;
}

#[tokio::test]
async fn later_owner_loss_does_not_clear_preexisting_uncertainty() {
    held_join_reply_change(
        Case::HoldJoinReply,
        JoinReplyChange::UncertainThenDisconnect,
    )
    .await;
}
