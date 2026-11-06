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
use wechat_core::call_types::CallCapabilities;
use wechat_core::{
    calls::{
        ownership::{AccountAuthority, CallIdentity, Operation},
        service::{BackendAudioRequest, BackendOutcome, BackendRequest, BackendSink, CallBackend},
    },
    Core,
};
use wechat_store::{AccessToken, Store};

type BackendFuture<T> = Pin<Box<dyn Future<Output = Result<T, CallServiceError>> + Send>>;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const OWNER: &str = "synthetic-owner-api-token-for-call-tests-only";

#[derive(Default)]
struct EchoBackend {
    submits: AtomicUsize,
    received: AtomicUsize,
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
    fn stop(&self, _: CallIdentity) -> BackendFuture<BackendOutcome> {
        Box::pin(async { Ok(BackendOutcome::Ended) })
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    core: Arc<Core>,
    backend: Arc<EchoBackend>,
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
        let server = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        Self {
            _temp: temp,
            core,
            backend,
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
                json!({"type":"call.hello","supportedProtocolVersions":[2]}),
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
    ClientMessage::Text(json!({"type":"audio.chunk","trackId":1,"generation":generation,"pcmBase64":BASE64.encode(bytes)}).to_string().into())
}
fn pcm_chunk(message: ClientMessage) -> Option<Vec<u8>> {
    let ClientMessage::Text(text) = message else {
        return None;
    };
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["type"], "audio.chunk");
    assert_eq!(value["trackId"], 2);
    Some(BASE64.decode(value["pcmBase64"].as_str().unwrap()).unwrap())
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
            json!({"type":"call.hello","supportedProtocolVersions":[1,3]}),
            "unsupported_protocol_version",
            json!([1, 3]),
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
            json!({"type":"call.hello","supportedProtocolVersions":[2,2]}),
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
        assert_eq!(error["serverVersions"], json!([2]));
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
        json!({"type":"call.hello","supportedProtocolVersions":[3,2]}),
    )
    .await;
    assert_eq!(json_event(&mut socket).await["protocolVersion"], 2);
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
        json!({"type":"call.hello","supportedProtocolVersions":[2]}),
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
        json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":client_one,"ownerToken":"0".repeat(64)}),
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
            json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":client_one,"ownerToken":owner_token}),
        )
        .await;
        assert_eq!(json_event(&mut foreign).await["code"], "forbidden");
    }
    let mut malformed = f
        .socket("/v1/accounts/one/calls/incoming-fixture/media", calls_token)
        .await;
    command(&mut malformed, json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":client_one,"ownerToken":owner_token,"unexpected":true})).await;
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
        json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":client_one,"ownerToken":owner_token}),
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
        json!({"type":"audio.start","trackId":1,"generation":1}),
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
    command(&mut media,json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
    assert_eq!(json_event(&mut media).await["type"], "media.ready");
    let frame = || {
        [432i16; 320]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>()
    };
    command(
        &mut media,
        json!({"type":"audio.start","trackId":1,"generation":1}),
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
        json!({"type":"audio.start","trackId":1,"generation":2}),
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
async fn json_pcm_limits_tracks_and_generations_are_checked_before_audio_admission() {
    let oversized = BASE64.encode(vec![0; MEDIA_LIMIT + 2]);
    for (start, chunk, accepted) in [
        (true, ClientMessage::Binary(vec![0, 0].into()), false),
        (
            true,
            ClientMessage::Text(
                json!({"type":"audio.chunk","trackId":1,"generation":1,"pcmBase64":"!!!!"})
                    .to_string()
                    .into(),
            ),
            false,
        ),
        (
            true,
            ClientMessage::Text(
                json!({"type":"audio.chunk","trackId":1,"generation":1,"pcmBase64":oversized})
                    .to_string()
                    .into(),
            ),
            false,
        ),
        (
            true,
            ClientMessage::Text(
                json!({"type":"audio.chunk","trackId":2,"generation":1,"pcmBase64":"AAA="})
                    .to_string()
                    .into(),
            ),
            false,
        ),
        (
            true,
            ClientMessage::Text(
                json!({"type":"audio.chunk","trackId":1,"generation":2,"pcmBase64":"AAA="})
                    .to_string()
                    .into(),
            ),
            false,
        ),
        (false, audio_chunk(&[0, 0], 1), false),
        (true, audio_chunk(&vec![0; MEDIA_LIMIT], 1), true),
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
        command(&mut media, json!({"type":"media.attach","supportedProtocolVersions":[2],"clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
        assert_eq!(json_event(&mut media).await["type"], "media.ready");
        if start {
            command(
                &mut media,
                json!({"type":"audio.start","trackId":1,"generation":1}),
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
