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
    async fn socket(&self, path: &str, token: &str) -> Socket {
        let mut request = format!("ws://{}{path}", self.address)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        connect_async(request).await.unwrap().0
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
        .call_service()
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
        .call_service()
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
        json!({"type":"media.attach","clientId":client_one,"ownerToken":"0".repeat(64)}),
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
            json!({"type":"media.attach","clientId":client_one,"ownerToken":owner_token}),
        )
        .await;
        assert_eq!(json_event(&mut foreign).await["code"], "forbidden");
    }
    let mut malformed = f
        .socket("/v1/accounts/one/calls/incoming-fixture/media", calls_token)
        .await;
    command(&mut malformed, json!({"type":"media.attach","clientId":client_one,"ownerToken":owner_token,"unexpected":true})).await;
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
        json!({"type":"media.attach","clientId":client_one,"ownerToken":owner_token}),
    )
    .await;
    let ack = json_event(&mut media).await;
    assert_eq!(ack["type"], "media.ready");
    use wechat_calls::{
        audio::AudioFormat,
        frame::{encode_pcm, MediaFrame, MediaHeader},
    };
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let samples: Vec<i16> = (0..320)
        .map(|n| if n % 2 == 0 { 1000 } else { -1000 })
        .collect();
    let bytes = encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 1,
            timestamp_micros: 20_000,
        },
        format,
        &samples,
    )
    .unwrap();
    media
        .send(ClientMessage::Binary(bytes.into()))
        .await
        .unwrap();
    loop {
        let response = timeout(Duration::from_secs(3), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let ClientMessage::Binary(bytes) = response {
            let frame = MediaFrame::decode(&bytes, format).unwrap();
            assert_eq!(frame.header.track_id, 2);
            assert_eq!(frame.samples().collect::<Vec<_>>(), samples);
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
    use wechat_calls::{
        audio::AudioFormat,
        frame::{encode_pcm, MediaFrame, MediaHeader},
    };
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
    command(&mut media,json!({"type":"media.attach","clientId":hello["clientId"],"ownerToken":owner["ownerToken"]})).await;
    assert_eq!(json_event(&mut media).await["type"], "media.ready");
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let frame = |generation, sequence, timestamp| {
        encode_pcm(
            MediaHeader {
                track_id: 1,
                generation,
                sequence,
                timestamp_micros: timestamp,
            },
            format,
            &[432; 320],
        )
        .unwrap()
    };
    let (release_audio, audio_gate) = tokio::sync::oneshot::channel();
    *f.backend.held_audio.lock().unwrap() = Some(audio_gate);
    media
        .send(ClientMessage::Binary(frame(1, 1, 20_000).into()))
        .await
        .unwrap();
    timeout(Duration::from_secs(2), f.backend.audio_started.notified())
        .await
        .unwrap();
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
    media
        .send(ClientMessage::Binary(frame(1, 2, 40_000).into()))
        .await
        .unwrap();
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
    media
        .send(ClientMessage::Binary(frame(2, 0, 0).into()))
        .await
        .unwrap();
    loop {
        let response = timeout(Duration::from_secs(2), media.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let ClientMessage::Binary(bytes) = response {
            let decoded = MediaFrame::decode(&bytes, format).unwrap();
            assert_eq!(decoded.samples().collect::<Vec<_>>(), vec![432; 320]);
            break;
        }
    }
    assert!(f
        .core
        .call_service()
        .validate_control(hello["clientId"].as_str().unwrap())
        .await
        .is_ok());
    assert_eq!(f.backend.received.load(Ordering::SeqCst), 2);
}
