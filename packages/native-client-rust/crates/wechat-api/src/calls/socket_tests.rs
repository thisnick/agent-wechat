use super::*;
use crate::router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message as ClientMessage},
    MaybeTlsStream, WebSocketStream,
};
use wechat_core::call_types::{
    CallCapabilities, CallTicketResponse, AUDIO_SEND_TRACK_ID, CALL_CONTROL_PROTOCOL_VERSION,
    CALL_CONTROL_SUPPORTED_VERSIONS, CALL_PROTOCOL_VERSION, VIDEO_RECEIVE_TRACK_BASE,
    VIDEO_SEND_TRACK_ID,
};
use wechat_core::{
    calls::{
        ownership::{AccountAuthority, CallIdentity, Operation},
        pcm_envelope::{
            self, MediaHeader, MediaPayloadKind, WireMediaFrame, FLAG_CODEC_CONFIG, FLAG_KEYFRAME,
            MAX_AUDIO_PAYLOAD_BYTES,
        },
        service::{
            BackendAudioRequest, BackendOutcome, BackendRequest, BackendSink, BackendVideoRequest,
            CallBackend,
        },
    },
    Core,
};
use wechat_store::{AccessToken, Store};

type BackendFuture<T> = Pin<Box<dyn Future<Output = Result<T, CallServiceError>> + Send>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const OWNER: &str = "synthetic-owner-api-token-for-call-tests-only";

#[derive(Default)]
struct EchoBackend {
    direct_video: std::sync::atomic::AtomicBool,
    submits: AtomicUsize,
    received: AtomicUsize,
    received_video: AtomicUsize,
    camera_changes: Mutex<Vec<bool>>,
    sink: Mutex<Option<BackendSink>>,
    held_audio: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    audio_started: Arc<tokio::sync::Notify>,
    held_clear: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    clear_started: Arc<tokio::sync::Notify>,
}
impl CallBackend for EchoBackend {
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        Some(AccountAuthority {
            account_id: account.into(),
            generation: "fixture-generation".into(),
        })
    }
    fn capabilities(&self, _: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: true,
            direct_video: self.direct_video.load(Ordering::Acquire),
            ..CallCapabilities::default()
        }
    }
    fn submit(&self, request: BackendRequest, sink: BackendSink) -> BackendFuture<BackendOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let start = matches!(
            request.ticket.operation(),
            Operation::Dial | Operation::Answer
        );
        if start {
            *self.sink.lock().unwrap() = Some(sink);
        }
        Box::pin(async move {
            Ok(if start {
                BackendOutcome::Connected
            } else {
                BackendOutcome::Ended
            })
        })
    }
    fn send_audio(&self, request: BackendAudioRequest) -> BackendFuture<()> {
        self.received.fetch_add(1, Ordering::SeqCst);
        let sink = self.sink.lock().unwrap().clone().unwrap();
        let held = self.held_audio.lock().unwrap().take();
        let started = self.audio_started.clone();
        Box::pin(async move {
            if let Some(held) = held {
                started.notify_one();
                let _ = held.await;
            }
            if !request.is_current() {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            sink.audio(&request.samples).await
        })
    }
    fn clear_audio(&self, _identity: CallIdentity, _generation: u32) -> BackendFuture<()> {
        let held = self.held_clear.lock().unwrap().take();
        let started = self.clear_started.clone();
        Box::pin(async move {
            started.notify_one();
            if let Some(held) = held {
                let _ = held.await;
            }
            Ok(())
        })
    }
    fn set_camera(&self, _identity: CallIdentity, enabled: bool) -> BackendFuture<()> {
        self.camera_changes.lock().unwrap().push(enabled);
        Box::pin(async { Ok(()) })
    }
    fn send_video(&self, request: BackendVideoRequest) -> BackendFuture<()> {
        let seq = self.received_video.fetch_add(1, Ordering::SeqCst) as u64;
        let sink = self.sink.lock().unwrap().clone().unwrap();
        Box::pin(async move {
            if !request.is_current() {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            sink.video_frame(
                VIDEO_RECEIVE_TRACK_BASE,
                1,
                request.codec,
                request.flags,
                seq,
                request.timestamp_micros,
                &request.payload,
            )
            .await
        })
    }
    fn request_video_keyframe(&self, _identity: CallIdentity, _track_id: u32) -> BackendFuture<()> {
        let sink = self.sink.lock().unwrap().clone().unwrap();
        Box::pin(async move { sink.peer_keyframe_requested().await })
    }
    fn stop(&self, _: CallIdentity) -> BackendFuture<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Ended) })
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<EchoBackend>,
    routes: axum::Router,
    address: std::net::SocketAddr,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(enabled: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let core = Core::new(store, None).unwrap();
        let backend = Arc::new(EchoBackend::default());
        if enabled {
            Core::configure_calls(&core, backend.clone()).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = router(core.clone(), OWNER);
        let served = routes.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, served).await.unwrap();
        });
        Self {
            _temp: temp,
            core,
            backend,
            routes,
            address,
            server,
        }
    }
    async fn raw_socket(&self, path: &str, token: &str) -> Socket {
        let mut request = format!("ws://{}{path}", self.address)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        connect_async(request).await.unwrap().0
    }
    async fn socket(&self, path: &str, token: &str) -> Socket {
        let mut socket = self.raw_socket(path, token).await;
        if path.ends_with("/calls/socket") {
            command(
                &mut socket,
                json!({"type":"call.hello","supportedProtocolVersions":[CALL_PROTOCOL_VERSION]}),
            )
            .await;
        }
        socket
    }
    fn grant(&self, token: &str, scopes: &[&str], accounts: &[&str]) -> AccessToken {
        use sha2::{Digest, Sha256};
        self.core
            .store
            .issue_token(
                "call-fixture",
                &format!("{:x}", Sha256::digest(token)),
                &accounts.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
                &scopes.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
                None,
            )
            .unwrap()
    }
}
async fn json_event(socket: &mut Socket) -> Value {
    loop {
        let message = timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match message {
            ClientMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
            ClientMessage::Ping(_) | ClientMessage::Pong(_) => {}
            _ => panic!("unexpected socket event"),
        }
    }
}
async fn until(socket: &mut Socket, mut predicate: impl FnMut(&Value) -> bool) -> Value {
    for _ in 0..40 {
        let event = json_event(socket).await;
        if predicate(&event) {
            return event;
        }
    }
    panic!("expected event did not arrive");
}
async fn command(socket: &mut Socket, body: Value) {
    socket
        .send(ClientMessage::Text(body.to_string().into()))
        .await
        .unwrap();
}
async fn ready(socket: &mut Socket, id: &str) {
    command(socket,json!({"type":"call.ready","requestId":id,"ready":true,"format":{"sampleRate":16000,"channels":1,"frameMs":20}})).await;
    let event = until(socket, |v| {
        v["type"] == "command.result" && v["requestId"] == id
    })
    .await;
    assert_eq!(event["status"], "accepted");
}

fn audio_chunk(bytes: &[u8], generation: u32) -> ClientMessage {
    audio_chunk_for(AUDIO_SEND_TRACK_ID, bytes, generation)
}
fn audio_chunk_for(track_id: u32, bytes: &[u8], generation: u32) -> ClientMessage {
    let encoded = pcm_envelope::encode_pcm_chunk(
        MediaHeader {
            track_id,
            generation,
            sequence: 0,
            timestamp_micros: 0,
        },
        bytes,
    )
    .unwrap();
    ClientMessage::Binary(encoded.into())
}
fn pcm_chunk(message: ClientMessage) -> Option<Vec<u8>> {
    let ClientMessage::Binary(bytes) = message else {
        return None;
    };
    let wire = WireMediaFrame::decode(&bytes).unwrap();
    assert_eq!(wire.kind, MediaPayloadKind::PcmS16le);
    assert_eq!(wire.header.track_id, 2);
    Some(wire.payload().to_vec())
}

#[tokio::test]
async fn protocol_negotiation_rejects_legacy_and_invalid_lists_before_actions() {
    let f = Fixture::new(true).await;
    for (message, code, versions) in [
        (
            json!({"type":"call.ready","requestId":"old","ready":true}),
            "unsupported_protocol_version",
            json!([1]),
        ),
        (
            json!({"type":"call.hello","supportedProtocolVersions":[1,2]}),
            "unsupported_protocol_version",
            json!([1, 2]),
        ),
        (
            json!({"type":"call.hello","supportedProtocolVersions":[]}),
            "invalid_protocol_version",
            json!([]),
        ),
        (
            json!({"type":"call.hello","supportedProtocolVersions":null}),
            "invalid_protocol_version",
            json!([]),
        ),
        (
            json!({"type":"call.hello","supportedProtocolVersions":[3,3]}),
            "invalid_protocol_version",
            json!([]),
        ),
    ] {
        let mut socket = f.raw_socket("/v1/accounts/one/calls/socket", OWNER).await;
        command(&mut socket, message).await;
        let error = json_event(&mut socket).await;
        assert_eq!(error["type"], "protocol.error");
        assert_eq!(error["code"], code);
        assert_eq!(error["clientVersions"], versions);
        assert_eq!(
            error["serverVersions"],
            json!(CALL_CONTROL_SUPPORTED_VERSIONS)
        );
        assert!(
            matches!(socket.next().await.unwrap().unwrap(), ClientMessage::Close(Some(frame)) if u16::from(frame.code) == 1002)
        );
    }
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 0);
    let mut socket = f.raw_socket("/v1/accounts/one/calls/socket", OWNER).await;
    socket
        .send(ClientMessage::Ping(vec![1].into()))
        .await
        .unwrap();
    command(
        &mut socket,
        json!({"type":"call.hello","supportedProtocolVersions":[2,3]}),
    )
    .await;
    assert_eq!(
        json_event(&mut socket).await["protocolVersion"],
        CALL_PROTOCOL_VERSION
    );
    let mut v4_socket = f.raw_socket("/v1/accounts/one/calls/socket", OWNER).await;
    command(
        &mut v4_socket,
        json!({"type":"call.hello","supportedProtocolVersions":[3,4]}),
    )
    .await;
    assert_eq!(
        json_event(&mut v4_socket).await["protocolVersion"],
        CALL_CONTROL_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn control_socket_suppresses_peer_status_for_v3_and_delivers_it_for_v4() {
    let f = Fixture::new(true).await;
    let mut v3 = f.raw_socket("/v1/accounts/one/calls/socket", OWNER).await;
    command(
        &mut v3,
        json!({"type":"call.hello","supportedProtocolVersions":[3]}),
    )
    .await;
    assert_eq!(
        json_event(&mut v3).await["protocolVersion"],
        CALL_PROTOCOL_VERSION
    );

    let mut v4 = f.raw_socket("/v1/accounts/one/calls/socket", OWNER).await;
    command(
        &mut v4,
        json!({"type":"call.hello","supportedProtocolVersions":[4,3]}),
    )
    .await;
    assert_eq!(
        json_event(&mut v4).await["protocolVersion"],
        CALL_CONTROL_PROTOCOL_VERSION
    );

    ready(&mut v4, "r1").await;
    command(
        &mut v4,
        json!({"type":"call.dial","requestId":"d1","callType":"voice","target":{"kind":"direct","contactId":"peer-1"}}),
    )
    .await;
    let connected_v4 = until(&mut v4, |ev| {
        ev["type"] == "call.state" && ev["call"]["state"] == "connected"
    })
    .await;
    let call_id = connected_v4["call"]["callId"]
        .as_str()
        .expect("v4 callId")
        .to_owned();
    let _ = until(&mut v3, |ev| {
        ev["type"] == "call.state" && ev["call"]["state"] == "connected"
    })
    .await;

    let sink = f.backend.sink.lock().unwrap().clone().expect("active sink");
    sink.peer_status(3).await.unwrap();
    sink.peer_microphone(true).await.unwrap();

    // v4 receives call.peer_status followed by call.control.
    let peer_status_v4 = until(&mut v4, |ev| ev["type"] == "call.peer_status").await;
    assert_eq!(peer_status_v4["callId"], call_id);
    assert_eq!(peer_status_v4["status"]["kind"], "acknowledged");
    let control_v4 = until(&mut v4, |ev| ev["type"] == "call.control").await;
    assert_eq!(control_v4["control"]["muted"], true);

    // Legacy v3 skips call.peer_status completely and receives call.control cleanly.
    loop {
        let ev = json_event(&mut v3).await;
        assert_ne!(
            ev["type"], "call.peer_status",
            "legacy v3 client must never receive call.peer_status"
        );
        if ev["type"] == "call.control" {
            assert_eq!(ev["control"]["muted"], true);
            break;
        }
    }
}

#[tokio::test]
async fn websocket_upgrade_checks_bearer_scope_account_and_origin() {
    let f = Fixture::new(false).await;
    f.grant("other-account", &["calls"], &["two"]);
    f.grant("non-calls", &["read"], &["one"]);
    for (token, origin, status) in [
        ("wrong", None, 401),
        ("other-account", None, 403),
        ("non-calls", None, 403),
        (OWNER, Some("null"), 403),
        (OWNER, Some("https://elsewhere.example"), 403),
        (OWNER, Some(&format!("http://{}/path", f.address)), 403),
        (
            OWNER,
            Some(&format!("http://{}/?token=value", f.address)),
            403,
        ),
    ] {
        let mut request = format!("ws://{}/v1/accounts/one/calls/socket", f.address)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        let error = connect_async(request).await.err().unwrap();
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), status)
            }
            _ => panic!("expected HTTP rejection"),
        }
    }

    let mut request = format!("ws://{}/v1/accounts/one/calls/socket", f.address)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {OWNER}").parse().unwrap(),
    );
    let origin = format!("http://{}", f.address);
    request
        .headers_mut()
        .insert(header::ORIGIN, origin.parse().unwrap());
    let (mut socket, _) = connect_async(request.clone()).await.unwrap();
    command(
        &mut socket,
        json!({"type":"call.hello","supportedProtocolVersions":[CALL_PROTOCOL_VERSION]}),
    )
    .await;
    let hello = json_event(&mut socket).await;
    assert_eq!(hello["capabilities"], json!({}));
    assert_eq!(hello["currentOffer"], Value::Null);
    socket.close(None).await.unwrap();
    request
        .headers_mut()
        .append(header::ORIGIN, origin.parse().unwrap());
    assert!(
        matches!(connect_async(request).await, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == StatusCode::FORBIDDEN)
    );
}

#[tokio::test]
async fn calls_grant_stays_passive_until_ready_and_revocation_closes_socket() {
    let f = Fixture::new(true).await;
    let grant = f.grant("calls-grant", &["calls"], &["one"]);
    f.grant("non-calls", &["read"], &["one"]);
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    for (path, token, status) in [
        (
            "/v1/accounts/one/calls/current",
            "calls-grant",
            StatusCode::OK,
        ),
        (
            "/v1/accounts/two/calls/current",
            "calls-grant",
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/accounts/one/calls/current",
            "non-calls",
            StatusCode::FORBIDDEN,
        ),
        (
            "/v1/accounts/one/calls/current",
            "wrong",
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let response = router(f.core.clone(), OWNER)
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == StatusCode::OK {
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let snapshot: CallSnapshot = serde_json::from_slice(&bytes).unwrap();
            assert!(snapshot.capabilities.direct_audio);
            assert!(!snapshot.availability.busy);
        }
    }
    let mut malformed = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
    json_event(&mut malformed).await;
    command(
        &mut malformed,
        json!({"type":"call.ready","requestId":"malformed","ready":true,"format":{"sampleRate":16000,"channels":1,"frameMs":20},"unexpected":true}),
    )
    .await;
    assert_eq!(json_event(&mut malformed).await["code"], "invalid_request");
    f.core
        .call_service("one")
        .unwrap()
        .offer(
            f.backend.authority("one").unwrap(),
            "passive-offer".into(),
            "peer-fixture".into(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let mut socket = f
        .socket("/v1/accounts/one/calls/socket", "calls-grant")
        .await;
    assert_eq!(json_event(&mut socket).await["type"], "hello");
    command(&mut socket,json!({"type":"call.answer","requestId":"not-ready","callId":"passive-offer","expectedRevision":1})).await;
    let result = until(&mut socket, |v| {
        v["type"] == "command.result" && v["requestId"] == "not-ready"
    })
    .await;
    assert_eq!(result["code"], "media_not_ready");
    assert_eq!(result["ownerToken"], Value::Null);
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 0);
    ready(&mut socket, "ready").await;
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 0);
    f.core.store.revoke_token(&grant.id).unwrap();
    let closed = timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                Some(Ok(ClientMessage::Ping(_))) | Some(Ok(ClientMessage::Pong(_))) => {}
                Some(Ok(ClientMessage::Close(_))) | Some(Err(_)) | None => break true,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(closed);
}

#[tokio::test]
async fn first_answer_owns_media_and_generated_pcm_roundtrips_without_keys() {
    let f = Fixture::new(true).await;
    let calls_token = "calls-participant";
    f.grant(calls_token, &["calls"], &["one", "two"]);
    let authority = f.backend.authority("one").unwrap();
    f.core
        .call_service("one")
        .unwrap()
        .offer(
            authority,
            "incoming-fixture".into(),
            "peer-fixture".into(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let mut one = f.socket("/v1/accounts/one/calls/socket", calls_token).await;
    let mut two = f.socket("/v1/accounts/one/calls/socket", calls_token).await;
    let hello_one = json_event(&mut one).await;
    let hello_two = json_event(&mut two).await;
    assert_eq!(hello_one["currentOffer"]["callId"], "incoming-fixture");
    ready(&mut one, "ready1").await;
    ready(&mut two, "ready2").await;
    let answer = |request: &str| json!({"type":"call.answer","requestId":request,"callId":"incoming-fixture","expectedRevision":1});
    tokio::join!(
        command(&mut one, answer("answer1")),
        command(&mut two, answer("answer2"))
    );
    let result_one = until(&mut one, |v| {
        v["type"] == "command.result" && v["requestId"] == "answer1"
    })
    .await;
    let result_two = until(&mut two, |v| {
        v["type"] == "command.result" && v["requestId"] == "answer2"
    })
    .await;
    let (winner, hello, result, loser) = if result_one["status"] == "accepted" {
        (&mut one, hello_one, result_one, result_two)
    } else {
        (&mut two, hello_two, result_two, result_one)
    };
    assert_eq!(result["status"], "accepted");
    assert_eq!(loser["status"], "error");
    assert_eq!(loser["code"], "already_claimed");
    assert_eq!(loser["ownerToken"], Value::Null);
    let client_one = hello["clientId"].as_str().unwrap().to_owned();
    let owner_token = result["ownerToken"].as_str().unwrap().to_owned();
    let incoming_state = until(winner, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "connected"
    })
    .await;
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 1);
    let mut wrong = f
        .socket("/v1/accounts/one/calls/incoming-fixture/media", calls_token)
        .await;
    command(
        &mut wrong,
        json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":client_one,"ownerToken":"0".repeat(64)}),
    )
    .await;
    assert_eq!(json_event(&mut wrong).await["code"], "not_owner");
    f.grant("other-media-principal", &["calls"], &["one"]);
    for (path, bearer) in [
        (
            "/v1/accounts/one/calls/incoming-fixture/media",
            "other-media-principal",
        ),
        ("/v1/accounts/two/calls/incoming-fixture/media", calls_token),
    ] {
        let mut foreign = f.socket(path, bearer).await;
        command(
            &mut foreign,
            json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":client_one,"ownerToken":owner_token}),
        )
        .await;
        assert_eq!(json_event(&mut foreign).await["code"], "forbidden");
    }
    let mut malformed = f
        .socket("/v1/accounts/one/calls/incoming-fixture/media", calls_token)
        .await;
    command(&mut malformed, json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":client_one,"ownerToken":owner_token,"unexpected":true})).await;
    assert!(matches!(
        timeout(Duration::from_secs(1), malformed.next())
            .await
            .unwrap(),
        None | Some(Err(_)) | Some(Ok(ClientMessage::Close(_)))
    ));
    let mut media = f
        .socket("/v1/accounts/one/calls/incoming-fixture/media", calls_token)
        .await;
    command(
        &mut media,
        json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":client_one,"ownerToken":owner_token}),
    )
    .await;
    let ack = json_event(&mut media).await;
    assert_eq!(ack["type"], "media.ready");
    let samples: Vec<i16> = (0..320)
        .map(|n| if n % 2 == 0 { 1000 } else { -1000 })
        .collect();
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    command(
        &mut media,
        json!({"type":"track.start","trackId":1,"generation":1}),
    )
    .await;
    // Application chunks need not match codec frame boundaries. The daemon
    // joins partial chunks and clocks complete frames, with JSON PCM on RX too.
    for chunk in [&bytes[..6], &bytes[6..]] {
        media.send(audio_chunk(chunk, 1)).await.unwrap();
    }
    loop {
        let response = timeout(Duration::from_secs(3), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(response) = pcm_chunk(response) {
            assert_eq!(response.as_slice(), bytes.as_slice());
            break;
        }
    }
    assert_eq!(f.backend.received.load(Ordering::SeqCst), 1);

    command(winner, json!({"type":"call.hangup","requestId":"end-incoming","callId":"incoming-fixture","expectedRevision":incoming_state["call"]["revision"]})).await;
    until(winner, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "ended"
    })
    .await;
    media.close(None).await.unwrap();
    let dial = json!({"type":"call.dial","requestId":"outgoing-idempotent","target":{"kind":"direct","contactId":"synthetic-peer"},"callType":"voice"});
    command(winner, dial.clone()).await;
    let result = until(winner, |v| {
        v["type"] == "command.result" && v["requestId"] == "outgoing-idempotent"
    })
    .await;
    assert_eq!(result["status"], "accepted");
    let outgoing_state = until(winner, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "connected"
    })
    .await;
    command(winner, dial.clone()).await;
    assert_eq!(
        until(winner, |v| v["type"] == "command.result"
            && v["requestId"] == "outgoing-idempotent")
        .await["status"],
        "replayed"
    );
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 3);
    command(winner, json!({"type":"call.hangup","requestId":"end-outgoing","callId":result["callId"],"expectedRevision":outgoing_state["call"]["revision"]})).await;
    until(winner, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "ended"
    })
    .await;
    command(winner, dial).await;
    assert_eq!(
        until(winner, |v| v["type"] == "command.result"
            && v["requestId"] == "outgoing-idempotent")
        .await["status"],
        "replayed"
    );
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn clear_ack_waits_for_backend_and_old_inflight_audio_does_not_revoke_owner() {
    let f = Fixture::new(true).await;
    let mut control = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
    let hello = json_event(&mut control).await;
    ready(&mut control, "ready-clear").await;
    command(&mut control,json!({"type":"call.dial","requestId":"dial-clear","target":{"kind":"direct","contactId":"synthetic-contact"},"callType":"voice"})).await;
    let owner = until(&mut control, |event| {
        event["type"] == "command.result" && event["requestId"] == "dial-clear"
    })
    .await;
    let call_id = owner["callId"].as_str().unwrap().to_string();
    until(&mut control, |event| {
        event["type"] == "call.state" && event["call"]["state"] == "connected"
    })
    .await;
    let mut media = f
        .socket(&format!("/v1/accounts/one/calls/{call_id}/media"), OWNER)
        .await;
    command(&mut media,json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
    assert_eq!(json_event(&mut media).await["type"], "media.ready");
    let frame = || {
        [432i16; 320]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    command(
        &mut media,
        json!({"type":"track.start","trackId":1,"generation":1}),
    )
    .await;
    let (release_audio, audio_gate) = tokio::sync::oneshot::channel();
    *f.backend.held_audio.lock().unwrap() = Some(audio_gate);
    media.send(audio_chunk(&frame(), 1)).await.unwrap();
    timeout(Duration::from_secs(2), f.backend.audio_started.notified())
        .await
        .unwrap();
    // The backend is still waiting to submit reply audio. A caller frame must
    // be delivered independently on the same socket before we release it.
    let peer = f.backend.sink.lock().unwrap().clone().unwrap();
    peer.audio(&[321; 320]).await.unwrap();
    timeout(Duration::from_secs(2), async {
        while let Some(message) = media.next().await {
            if let Some(bytes) = pcm_chunk(message.unwrap()) {
                assert_eq!(bytes.len(), 640);
                assert!(bytes
                    .chunks_exact(2)
                    .all(|sample| sample == 321i16.to_le_bytes()));
                return;
            }
        }
        panic!("caller audio socket closed");
    })
    .await
    .expect("caller audio blocked by reply submission");
    let (release_clear, clear_gate) = tokio::sync::oneshot::channel();
    *f.backend.held_clear.lock().unwrap() = Some(clear_gate);
    command(
        &mut control,
        json!({"type":"call.clear_audio","requestId":"clear-held","callId":call_id}),
    )
    .await;
    timeout(Duration::from_secs(2), f.backend.clear_started.notified())
        .await
        .unwrap();
    // Internal events can be queued, but the real control socket cannot forward
    // either acknowledgement or new track until the worker clear has completed.
    assert!(timeout(Duration::from_millis(50), json_event(&mut control))
        .await
        .is_err());
    release_audio.send(()).unwrap();
    media.send(audio_chunk(&frame(), 1)).await.unwrap();
    release_clear.send(()).unwrap();
    let mut result = None;
    let mut track = None;
    while result.is_none() || track.is_none() {
        let event = json_event(&mut control).await;
        if event["type"] == "track.updated" {
            track = Some(event);
        } else if event["type"] == "command.result" && event["requestId"] == "clear-held" {
            result = Some(event);
        }
    }
    assert_eq!(result.unwrap()["status"], "accepted");
    assert_eq!(track.unwrap()["track"]["generation"], 2);
    command(
        &mut media,
        json!({"type":"track.start","trackId":1,"generation":2}),
    )
    .await;
    media.send(audio_chunk(&frame(), 2)).await.unwrap();
    loop {
        let response = timeout(Duration::from_secs(2), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Some(bytes) = pcm_chunk(response) {
            assert_eq!(bytes.as_slice(), frame().as_slice());
            break;
        }
    }
    assert!(f
        .core
        .call_service("one")
        .unwrap()
        .validate_control(hello["clientId"].as_str().unwrap())
        .await
        .is_ok());
    assert_eq!(f.backend.received.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn active_outgoing_call_does_not_block_another_accounts_incoming_call() {
    let f = Fixture::new(true).await;
    let mut caller = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
    json_event(&mut caller).await;
    ready(&mut caller, "ready-caller").await;
    command(
        &mut caller,
        json!({"type":"call.dial","requestId":"dial-first","target":{"kind":"direct","contactId":"synthetic-peer"},"callType":"voice"}),
    )
    .await;
    let outgoing = until(&mut caller, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "connected"
    })
    .await;
    assert_eq!(outgoing["call"]["ownedByThisClient"], true);

    let mut callee = f.socket("/v1/accounts/two/calls/socket", OWNER).await;
    assert_eq!(json_event(&mut callee).await["availability"]["busy"], false);
    f.core
        .call_service("two")
        .unwrap()
        .offer(
            f.backend.authority("two").unwrap(),
            "second-account-offer".into(),
            "synthetic-caller".into(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    ready(&mut callee, "ready-callee").await;
    command(
        &mut callee,
        json!({"type":"call.answer","requestId":"answer-second","callId":"second-account-offer","expectedRevision":1}),
    )
    .await;
    let incoming = until(&mut callee, |v| {
        v["type"] == "call.state" && v["call"]["state"] == "connected"
    })
    .await;
    assert_eq!(incoming["call"]["ownedByThisClient"], true);

    let mut competitor = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
    let hello = json_event(&mut competitor).await;
    assert_eq!(hello["availability"]["busy"], true);
    assert_eq!(hello["currentCall"]["callId"], outgoing["call"]["callId"]);
    assert_eq!(hello["currentCall"]["ownedByThisClient"], false);
    ready(&mut competitor, "ready-competitor").await;
    command(
        &mut competitor,
        json!({"type":"call.dial","requestId":"dial-while-busy","target":{"kind":"direct","contactId":"other-synthetic-peer"},"callType":"voice"}),
    )
    .await;
    let denied = until(&mut competitor, |v| v["code"] == "busy").await;
    assert_eq!(denied["ownerToken"], Value::Null);
    assert_eq!(f.backend.submits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn binary_wxcm_limits_tracks_and_generations_are_checked_before_audio_admission() {
    let mut bad_version = pcm_envelope::encode_pcm_chunk(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        &[0; 640],
    )
    .unwrap();
    bad_version[4] = 2;
    let mut oversized_audio =
        Vec::with_capacity(pcm_envelope::HEADER_BYTES + MAX_AUDIO_PAYLOAD_BYTES + 2);
    oversized_audio.extend_from_slice(b"WXCM");
    oversized_audio.push(CALL_PROTOCOL_VERSION as u8);
    oversized_audio.push(MediaPayloadKind::PcmS16le as u8);
    oversized_audio.extend_from_slice(&0u16.to_be_bytes());
    oversized_audio.extend_from_slice(&1u32.to_be_bytes());
    oversized_audio.extend_from_slice(&1u32.to_be_bytes());
    oversized_audio.extend_from_slice(&0u64.to_be_bytes());
    oversized_audio.extend_from_slice(&0u64.to_be_bytes());
    oversized_audio.extend_from_slice(&((MAX_AUDIO_PAYLOAD_BYTES + 2) as u32).to_be_bytes());
    oversized_audio.resize(pcm_envelope::HEADER_BYTES + MAX_AUDIO_PAYLOAD_BYTES + 2, 0);
    for (start, chunk, accepted) in [
        (true, ClientMessage::Binary(vec![0, 0].into()), false),
        (true, ClientMessage::Binary(bad_version.into()), false),
        (true, ClientMessage::Binary(oversized_audio.into()), false),
        (
            true,
            ClientMessage::Text(
                json!({"type":"audio.chunk","trackId":1,"generation":1,"pcmBase64":"AAA="})
                    .to_string()
                    .into(),
            ),
            false,
        ),
        (true, audio_chunk_for(2, &[0, 0], 1), false),
        (true, audio_chunk(&[0, 0], 2), false),
        (false, audio_chunk(&[0, 0], 1), false),
        (
            true,
            audio_chunk(&vec![0; MAX_AUDIO_PAYLOAD_BYTES], 1),
            true,
        ),
    ] {
        let f = Fixture::new(true).await;
        let mut control = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
        let hello = json_event(&mut control).await;
        ready(&mut control, "ready").await;
        command(&mut control, json!({"type":"call.dial","requestId":"dial","target":{"kind":"direct","contactId":"synthetic-peer"},"callType":"voice"})).await;
        let owner = until(&mut control, |event| {
            event["type"] == "command.result" && event["requestId"] == "dial"
        })
        .await;
        assert_eq!(owner["status"], "accepted");
        until(&mut control, |event| {
            event["type"] == "call.state" && event["call"]["state"] == "connected"
        })
        .await;
        let path = format!(
            "/v1/accounts/one/calls/{}/media",
            owner["callId"].as_str().unwrap()
        );
        let mut media = f.socket(&path, OWNER).await;
        command(&mut media, json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
        assert_eq!(json_event(&mut media).await["type"], "media.ready");
        if start {
            command(
                &mut media,
                json!({"type":"track.start","trackId":1,"generation":1}),
            )
            .await;
        }
        media.send(chunk).await.unwrap();
        let outcome = timeout(Duration::from_secs(2), async {
            loop {
                match media.next().await {
                    Some(Ok(ClientMessage::Ping(_) | ClientMessage::Pong(_))) => continue,
                    other => break other,
                }
            }
        })
        .await
        .unwrap();
        if accepted {
            assert_eq!(pcm_chunk(outcome.unwrap().unwrap()).unwrap(), vec![0; 640]);
            assert!(f.backend.received.load(Ordering::SeqCst) > 0);
        } else {
            assert!(matches!(
                outcome,
                None | Some(Err(_)) | Some(Ok(ClientMessage::Close(_)))
            ));
            assert_eq!(f.backend.received.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn call_socket_tickets_are_single_use_scoped_and_revoked_with_bearer() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let f = Fixture::new(true).await;
    let grant = f.grant("browser-calls-token", &["calls"], &["one"]);
    f.grant("read-only-token", &["read"], &["one"]);

    let post_ticket = |token: &str, body: Value| {
        let app = f.routes.clone();
        let auth = format!("Bearer {token}");
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/accounts/one/calls/tickets")
                    .header(header::AUTHORIZATION, auth)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Non-calls token cannot mint a ticket.
    let denied = post_ticket("read-only-token", json!({"socket": "control"})).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);

    // Media socket ticket requires callId.
    let bad_req = post_ticket("browser-calls-token", json!({"socket": "media"})).await;
    assert_eq!(bad_req.status(), StatusCode::BAD_REQUEST);

    // Wrong account cannot use a ticket, and the mismatched attempt burns it.
    let probe_resp = post_ticket("browser-calls-token", json!({"socket": "control"})).await;
    let probe_ticket: CallTicketResponse =
        serde_json::from_slice(&probe_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    let wrong_account = connect_async(format!(
        "ws://{}/v1/accounts/two/calls/socket?ticket={}",
        f.address, probe_ticket.ticket
    ))
    .await;
    assert!(matches!(
        wrong_account,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));
    let reused_probe = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, probe_ticket.ticket
    ))
    .await;
    assert!(matches!(
        reused_probe,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));

    // Mint a control ticket and connect without Authorization header.
    let ticket_resp = post_ticket("browser-calls-token", json!({"socket": "control"})).await;
    assert_eq!(ticket_resp.status(), StatusCode::OK);
    assert_eq!(
        ticket_resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let bytes = ticket_resp.into_body().collect().await.unwrap().to_bytes();
    let issued: CallTicketResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(!issued.ticket.is_empty());

    // Correct account + socket upgrades without Authorization header.
    let (mut control, _) = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, issued.ticket
    ))
    .await
    .unwrap();
    command(
        &mut control,
        json!({"type":"call.hello","supportedProtocolVersions":[CALL_PROTOCOL_VERSION]}),
    )
    .await;
    let hello = json_event(&mut control).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["protocolVersion"], CALL_PROTOCOL_VERSION);

    // Replay of the exact same ticket is rejected.
    let replay = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, issued.ticket
    ))
    .await;
    assert!(matches!(
        replay,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));

    // Ticket cannot be used on ordinary REST routes like /calls/current.
    let rest_resp = post_ticket("browser-calls-token", json!({"socket": "control"})).await;
    let rest_ticket: CallTicketResponse =
        serde_json::from_slice(&rest_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let rest_denied = f
        .routes
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/accounts/one/calls/current?ticket={}",
                    rest_ticket.ticket
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rest_denied.status(), StatusCode::UNAUTHORIZED);

    // Dial a call and verify a media-scoped ticket connects to the exact call only.
    ready(&mut control, "ready-ticket").await;
    command(&mut control, json!({"type":"call.dial","requestId":"dial-ticket","target":{"kind":"direct","contactId":"synthetic-peer"},"callType":"voice"})).await;
    let owner = until(&mut control, |e| {
        e["type"] == "command.result" && e["requestId"] == "dial-ticket"
    })
    .await;
    let call_id = owner["callId"].as_str().unwrap().to_string();
    until(&mut control, |e| {
        e["type"] == "call.state" && e["call"]["state"] == "connected"
    })
    .await;

    let wrong_media_resp = post_ticket(
        "browser-calls-token",
        json!({"socket": "media", "callId": call_id}),
    )
    .await;
    let wrong_media_ticket: CallTicketResponse = serde_json::from_slice(
        &wrong_media_resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    let wrong_call = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/other-call/media?ticket={}",
        f.address, wrong_media_ticket.ticket
    ))
    .await;
    assert!(matches!(
        wrong_call,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));
    let media_resp = post_ticket(
        "browser-calls-token",
        json!({"socket": "media", "callId": call_id}),
    )
    .await;
    let media_ticket: CallTicketResponse =
        serde_json::from_slice(&media_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    let (mut media, _) = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/{call_id}/media?ticket={}",
        f.address, media_ticket.ticket
    ))
    .await
    .unwrap();
    command(&mut media, json!({"type":"media.attach","supportedProtocolVersions":[CALL_PROTOCOL_VERSION],"clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
    assert_eq!(json_event(&mut media).await["type"], "media.ready");

    // Revoking the issuing bearer invalidates unused tickets too.
    let doomed_resp = post_ticket("browser-calls-token", json!({"socket": "control"})).await;
    let doomed_ticket: CallTicketResponse =
        serde_json::from_slice(&doomed_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    f.core.store.revoke_token(&grant.id).unwrap();
    let revoked_connect = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, doomed_ticket.ticket
    ))
    .await;
    assert!(matches!(
        revoked_connect,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));
}

#[tokio::test]
async fn video_call_negotiates_passthrough_and_roundtrips_binary_video_and_keyframe_requests() {
    let f = Fixture::new(true).await;
    f.backend.direct_video.store(true, Ordering::Release);
    let mut control = f.socket("/v1/accounts/one/calls/socket", OWNER).await;
    let hello = json_event(&mut control).await;
    assert_eq!(hello["capabilities"]["directVideo"], true);
    command(
        &mut control,
        json!({
            "type": "call.ready",
            "requestId": "ready-v",
            "ready": true,
            "format": {"sampleRate": 16000, "channels": 1, "frameMs": 20},
            "video": {"sendCodecs": ["h265", "jpeg"], "receiveCodecs": ["h265", "jpeg"]}
        }),
    )
    .await;
    assert_eq!(
        until(&mut control, |e| e["requestId"] == "ready-v").await["status"],
        "accepted"
    );
    command(
        &mut control,
        json!({
            "type": "call.dial",
            "requestId": "dial-v",
            "target": {"kind": "direct", "contactId": "synthetic-peer"},
            "callType": "video",
            "cameraEnabled": true
        }),
    )
    .await;
    let owner = until(&mut control, |e| e["requestId"] == "dial-v").await;
    let call_id = owner["callId"].as_str().unwrap().to_string();
    until(&mut control, |e| {
        e["type"] == "call.state" && e["call"]["state"] == "connected"
    })
    .await;

    let mut media = f
        .socket(&format!("/v1/accounts/one/calls/{call_id}/media"), OWNER)
        .await;
    command(
        &mut media,
        json!({
            "type": "media.attach",
            "supportedProtocolVersions": [CALL_PROTOCOL_VERSION],
            "clientId": hello["clientId"],
            "ownerToken": owner["ownerToken"]
        }),
    )
    .await;
    let ready_event = json_event(&mut media).await;
    assert_eq!(ready_event["type"], "media.ready");
    assert!(ready_event["sendTracks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["media"] == "video"
            && t["id"] == VIDEO_SEND_TRACK_ID
            && t["format"]["codec"] == "h265"
            && t["format"]["mode"] == "passthrough"));
    assert!(ready_event["receiveTracks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["media"] == "video"
            && t["id"] == VIDEO_RECEIVE_TRACK_BASE
            && t["format"]["codec"] == "h265"
            && t["format"]["mode"] == "passthrough"));

    // Start video send track and send one binary H.265 keyframe.
    command(
        &mut media,
        json!({"type": "track.start", "trackId": VIDEO_SEND_TRACK_ID, "generation": 1}),
    )
    .await;
    let nal = vec![0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xde, 0xad, 0xbe, 0xef];
    let packet = pcm_envelope::encode_video(
        MediaPayloadKind::H265AnnexB,
        FLAG_KEYFRAME | FLAG_CODEC_CONFIG,
        MediaHeader {
            track_id: VIDEO_SEND_TRACK_ID,
            generation: 1,
            sequence: 0,
            timestamp_micros: 33_333,
        },
        &nal,
    )
    .unwrap();
    media
        .send(ClientMessage::Binary(packet.into()))
        .await
        .unwrap();

    // EchoBackend echoes video frames onto VIDEO_RECEIVE_TRACK_BASE_ID.
    loop {
        match timeout(Duration::from_secs(2), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            ClientMessage::Binary(bytes) => {
                let wire = WireMediaFrame::decode(&bytes).unwrap();
                assert_eq!(wire.kind, MediaPayloadKind::H265AnnexB);
                assert_eq!(wire.header.track_id, VIDEO_RECEIVE_TRACK_BASE);
                assert_eq!(wire.flags, FLAG_KEYFRAME | FLAG_CODEC_CONFIG);
                assert_eq!(wire.header.timestamp_micros, 33_333);
                assert_eq!(wire.payload(), nal.as_slice());
                break;
            }
            ClientMessage::Ping(_) | ClientMessage::Pong(_) => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(f.backend.received_video.load(Ordering::SeqCst), 1);

    // Client requests keyframe -> EchoBackend emits video.keyframe_requested.
    command(
        &mut media,
        json!({"type": "video.request_keyframe", "trackId": VIDEO_RECEIVE_TRACK_BASE}),
    )
    .await;
    let kf_event = until(&mut media, |e| e["type"] == "video.keyframe_requested").await;
    assert_eq!(kf_event["trackId"], VIDEO_SEND_TRACK_ID);

    // Client toggles camera off -> backend records it and video send track generation bumps to 2.
    command(
        &mut control,
        json!({"type": "call.camera", "requestId": "cam-toggle", "callId": call_id, "enabled": false}),
    )
    .await;
    let cam_res = until(&mut control, |e| e["requestId"] == "cam-toggle").await;
    assert_eq!(cam_res["status"], "accepted");
    assert_eq!(
        f.backend.camera_changes.lock().unwrap().as_slice(),
        &[false]
    );
    let track_upd = until(&mut media, |e| {
        e["type"] == "track.updated" && e["track"]["id"] == VIDEO_SEND_TRACK_ID
    })
    .await;
    assert_eq!(track_upd["track"]["generation"], 2);

    // Superseded track.start (generation 1 < current 2) and stale video.request_keyframe on an
    // unknown receive track ID >= VIDEO_RECEIVE_TRACK_BASE must not fail or detach the owner.
    command(
        &mut media,
        json!({"type": "track.start", "trackId": VIDEO_SEND_TRACK_ID, "generation": 1}),
    )
    .await;
    command(
        &mut media,
        json!({"type": "video.request_keyframe", "trackId": VIDEO_RECEIVE_TRACK_BASE + 9}),
    )
    .await;
    // Toggle camera back on (generation 3) and confirm video delivery still works afterward.
    command(
        &mut control,
        json!({"type": "call.camera", "requestId": "cam-toggle-on", "callId": call_id, "enabled": true}),
    )
    .await;
    assert_eq!(
        until(&mut control, |e| e["requestId"] == "cam-toggle-on").await["status"],
        "accepted"
    );
    let track_upd_on = until(&mut media, |e| {
        e["type"] == "track.updated"
            && e["track"]["id"] == VIDEO_SEND_TRACK_ID
            && e["track"]["generation"] == 3
    })
    .await;
    assert_eq!(track_upd_on["track"]["active"], true);
    command(
        &mut media,
        json!({"type": "track.start", "trackId": VIDEO_SEND_TRACK_ID, "generation": 3}),
    )
    .await;
    let packet_g3 = pcm_envelope::encode_video(
        MediaPayloadKind::H265AnnexB,
        FLAG_KEYFRAME | FLAG_CODEC_CONFIG,
        MediaHeader {
            track_id: VIDEO_SEND_TRACK_ID,
            generation: 3,
            sequence: 1,
            timestamp_micros: 66_666,
        },
        &nal,
    )
    .unwrap();
    media
        .send(ClientMessage::Binary(packet_g3.into()))
        .await
        .unwrap();
    loop {
        match timeout(Duration::from_secs(2), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            ClientMessage::Binary(bytes) => {
                let wire = WireMediaFrame::decode(&bytes).unwrap();
                assert_eq!(wire.header.timestamp_micros, 66_666);
                break;
            }
            ClientMessage::Ping(_) | ClientMessage::Pong(_) => {}
            other => panic!("unexpected frame after stale track.start: {other:?}"),
        }
    }
    assert_eq!(f.backend.received_video.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn call_tickets_enforce_ttl_socket_binding_origin_and_account_aliases() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // 1. Direct TTL expiration check on CallSocketTickets.
    let store = CallSocketTickets::default();
    let issued = store
        .issue(
            "one",
            Claims { grant: None },
            CallTicketSocket::Control,
            None,
        )
        .unwrap();
    {
        let mut entries = store.entries.lock().unwrap();
        for item in entries.values_mut() {
            item.expires_at = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
        }
    }
    assert!(store
        .consume(&issued.ticket, "one", CallTicketSocket::Control, None)
        .is_none());

    // 2. HTTP + WebSocket end-to-end checks for cross-socket misuse, alias resolution, and Origin.
    let f = Fixture::new(true).await;
    f.core.store.set_account_alias("one", "alias-one").unwrap();
    f.grant("alias-token", &["calls"], &["one"]);

    let post_ticket = |account: &str, body: Value| {
        let routes = f.routes.clone();
        let uri = format!("/v1/accounts/{account}/calls/tickets");
        async move {
            let resp = routes
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(header::AUTHORIZATION, "Bearer alias-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<CallTicketResponse>(&bytes).unwrap()
        }
    };

    // Cross-socket: control ticket cannot connect to /media and is burned on mismatch.
    let ctrl_tkt = post_ticket("one", json!({"socket": "control"})).await;
    assert!(matches!(
        connect_async(format!(
            "ws://{}/v1/accounts/one/calls/call-x/media?ticket={}",
            f.address, ctrl_tkt.ticket
        ))
        .await,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));
    assert!(matches!(
        connect_async(format!(
            "ws://{}/v1/accounts/one/calls/socket?ticket={}",
            f.address, ctrl_tkt.ticket
        ))
        .await,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));

    // Cross-socket: media ticket cannot connect to /socket and is burned on mismatch.
    let media_tkt = post_ticket("one", json!({"socket": "media", "callId": "call-x"})).await;
    assert!(matches!(
        connect_async(format!(
            "ws://{}/v1/accounts/one/calls/socket?ticket={}",
            f.address, media_tkt.ticket
        ))
        .await,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));
    assert!(matches!(
        connect_async(format!(
            "ws://{}/v1/accounts/one/calls/call-x/media?ticket={}",
            f.address, media_tkt.ticket
        ))
        .await,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::UNAUTHORIZED
    ));

    // Origin check still rejects cross-origin browser upgrades even with a valid ticket.
    let origin_tkt = post_ticket("one", json!({"socket": "control"})).await;
    let mut bad_origin_req = format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, origin_tkt.ticket
    )
    .into_client_request()
    .unwrap();
    bad_origin_req
        .headers_mut()
        .insert(header::ORIGIN, "https://elsewhere.example".parse().unwrap());
    assert!(matches!(
        connect_async(bad_origin_req).await,
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status() == StatusCode::FORBIDDEN
    ));

    // Alias resolution: issue via alias, consume via canonical ID, and vice versa.
    let from_alias = post_ticket("alias-one", json!({"socket": "control"})).await;
    let (mut sock_a, _) = connect_async(format!(
        "ws://{}/v1/accounts/one/calls/socket?ticket={}",
        f.address, from_alias.ticket
    ))
    .await
    .unwrap();
    command(
        &mut sock_a,
        json!({"type":"call.hello","supportedProtocolVersions":[CALL_PROTOCOL_VERSION]}),
    )
    .await;
    assert_eq!(json_event(&mut sock_a).await["type"], "hello");

    let from_canonical = post_ticket("one", json!({"socket": "control"})).await;
    let (mut sock_b, _) = connect_async(format!(
        "ws://{}/v1/accounts/alias-one/calls/socket?ticket={}",
        f.address, from_canonical.ticket
    ))
    .await
    .unwrap();
    command(
        &mut sock_b,
        json!({"type":"call.hello","supportedProtocolVersions":[CALL_PROTOCOL_VERSION]}),
    )
    .await;
    assert_eq!(json_event(&mut sock_b).await["type"], "hello");
}
