//! Authenticated control and owner-bound PCM sockets. No account keys cross these routes.
use super::{ApiError, ApiResult, App, Claims};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Extension, Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{
    future::{select, Either},
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use std::{pin::pin, time::Duration};
use tokio::{
    sync::{broadcast::error::RecvError, mpsc, watch},
    time::{interval, timeout, Instant, MissedTickBehavior},
};
use wechat_calls::audio::AudioFormat;
use wechat_core::calls::pcm_envelope::MediaFrame;
use wechat_core::{
    call_types::{
        CallClientHello, CallCommand, CallErrorCode, CallEvent, CallMediaCommand,
        CallProtocolErrorCode, CallSnapshot, CALL_PROTOCOL_VERSION,
    },
    calls::service::{CallService, CallServiceError, MediaDelivery, MediaReceiveQueue},
};

const CONTROL_LIMIT: usize = 16 * 1024;
const MEDIA_LIMIT: usize = 64 * 1024;
const MEDIA_TEXT_LIMIT: usize = MEDIA_LIMIT.div_ceil(3) * 4 + 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Token scope that every calls route requires.
const CALLS_SCOPE: &str = "calls";
const CONTROL_PATH: &str = "/v1/accounts/{a}/calls/socket";
const MEDIA_PATH: &str = "/v1/accounts/{a}/calls/{call}/media";

/// Which variants of a tagged message type a socket side sends.
#[cfg(test)]
pub(crate) enum Variants {
    /// Every variant.
    All,
    /// Only the variants with these tag values.
    Only(&'static [&'static str]),
    /// Every variant except those with these tag values.
    Except(&'static [&'static str]),
}

/// A message type that one side of a socket sends, as a serde enum in
/// `wechat_core::call_types` tagged by `type`.
#[cfg(test)]
pub(crate) struct SocketMessage {
    pub(crate) type_name: &'static str,
    pub(crate) variants: Variants,
}

/// One calls WebSocket. `interfaces/http/calls-websocket.txt` is generated
/// from this table and the message schemas, so keep it in step with the
/// handlers below.
#[cfg(test)]
pub(crate) struct CallSocket {
    pub(crate) name: &'static str,
    /// Route path exactly as registered with the router.
    pub(crate) path: &'static str,
    /// Token scope the upgrade request needs.
    pub(crate) scope: &'static str,
    pub(crate) protocol_version: u32,
    pub(crate) doc: &'static str,
    pub(crate) client: &'static [SocketMessage],
    pub(crate) server: &'static [SocketMessage],
}

/// Every calls WebSocket, in the order the listing documents them.
#[cfg(test)]
pub(crate) const SOCKETS: &[CallSocket] = &[
    CallSocket {
        name: "control",
        path: CONTROL_PATH,
        scope: CALLS_SCOPE,
        protocol_version: CALL_PROTOCOL_VERSION,
        doc: "Call commands and events as JSON text. Send call.hello within five seconds of connecting, then wait for hello.",
        client: &[
            SocketMessage {
                type_name: "CallClientHello",
                variants: Variants::All,
            },
            SocketMessage {
                type_name: "CallCommand",
                variants: Variants::All,
            },
        ],
        server: &[SocketMessage {
            type_name: "CallEvent",
            variants: Variants::Except(&["media.ready"]),
        }],
    },
    CallSocket {
        name: "media",
        path: MEDIA_PATH,
        scope: CALLS_SCOPE,
        protocol_version: CALL_PROTOCOL_VERSION,
        doc: "Audio for one call you own, as JSON text with base64 PCM. Send media.attach within five seconds of connecting, then wait for media.ready. Binary messages are rejected.",
        client: &[SocketMessage {
            type_name: "CallMediaCommand",
            variants: Variants::All,
        }],
        server: &[
            SocketMessage {
                type_name: "CallEvent",
                variants: Variants::Only(&[
                    "call.error",
                    "media.ready",
                    "protocol.error",
                    "track.added",
                    "track.removed",
                ]),
            },
            SocketMessage {
                type_name: "CallMediaCommand",
                variants: Variants::Only(&["audio.chunk"]),
            },
        ],
    },
];

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/v1/accounts/{a}/calls/current", get(current))
        .route(CONTROL_PATH, get(control))
        .route(MEDIA_PATH, get(media))
}

fn principal(claims: &Claims) -> String {
    match &claims.grant {
        Some(grant) => format!("grant:{}", grant.id),
        None => "owner".into(),
    }
}

fn service_error(error: CallServiceError) -> ApiError {
    let code = error.code();
    let status = match code {
        "invalid_request" => StatusCode::BAD_REQUEST,
        "forbidden" | "not_owner" => StatusCode::FORBIDDEN,
        "closed" | "offer_expired" | "stale" => StatusCode::GONE,
        "unsupported" => StatusCode::NOT_IMPLEMENTED,
        "capacity" => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::CONFLICT,
    };
    ApiError(status, code)
}

async fn current(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path(account): Path<String>,
) -> ApiResult<impl IntoResponse> {
    claims.require(Some(&account), CALLS_SCOPE)?;
    ensure_current(&app, &claims).await?;
    app.core.store.account(&account)?;
    let snapshot: CallSnapshot = app
        .core
        .call_service(&account)?
        .current(&account, &principal(&claims))
        .await
        .map_err(service_error)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(snapshot)))
}

/// Header authentication is required. Browser ticket issuance is a separate API;
/// persistent bearer credentials are never accepted from a socket URL.
async fn control(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path(account): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    claims.require(Some(&account), CALLS_SCOPE)?;
    ensure_current(&app, &claims).await?;
    check_origin(&headers)?;
    app.core.store.account(&account)?;
    Ok(upgrade
        .max_message_size(CONTROL_LIMIT)
        .max_frame_size(CONTROL_LIMIT)
        .on_upgrade(move |socket| control_socket(socket, app, claims, account)))
}

async fn media(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path((account, call)): Path<(String, String)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    claims.require(Some(&account), CALLS_SCOPE)?;
    ensure_current(&app, &claims).await?;
    check_origin(&headers)?;
    app.core.store.account(&account)?;
    Ok(upgrade
        .max_message_size(MEDIA_TEXT_LIMIT)
        .max_frame_size(MEDIA_TEXT_LIMIT)
        .on_upgrade(move |socket| media_socket(socket, app, claims, account, call)))
}

async fn ensure_current(app: &App, claims: &Claims) -> ApiResult<()> {
    if app.core.stopping.load(std::sync::atomic::Ordering::Acquire) {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon_stopping"));
    }
    let store = app.core.store.clone();
    let claims = claims.clone();
    let valid = tokio::task::spawn_blocking(move || claims.valid(&store))
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "operation_failed"))?;
    if !valid {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    Ok(())
}

fn check_origin(headers: &HeaderMap) -> ApiResult<()> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    if headers.get_all(header::ORIGIN).iter().count() != 1 {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    let origin: Uri = origin
        .to_str()
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or(ApiError(StatusCode::FORBIDDEN, "forbidden"))?;
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or(ApiError(StatusCode::FORBIDDEN, "forbidden"))?;
    if !matches!(origin.scheme_str(), Some("http" | "https"))
        || origin
            .authority()
            .is_none_or(|a| !a.as_str().eq_ignore_ascii_case(host))
        || !matches!(origin.path(), "" | "/")
        || origin.query().is_some()
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    Ok(())
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: Message,
) -> bool {
    matches!(timeout(WRITE_TIMEOUT, sink.send(message)).await, Ok(Ok(())))
}
async fn send_event(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    event: &CallEvent,
) -> bool {
    let Ok(value) = serde_json::to_string(event) else {
        return false;
    };
    if value.len() > CONTROL_LIMIT {
        return false;
    }
    send(sink, Message::Text(value.into())).await
}

/// Resolve only an absent declaration to legacy version 1. Explicit invalid
/// lists are never treated as an implicit request for a different protocol.
fn client_versions(value: &serde_json::Value) -> Result<Vec<u32>, ()> {
    let object = value.as_object().ok_or(())?;
    let Some(versions) = object.get("supportedProtocolVersions") else {
        return Ok(vec![1]);
    };
    let values = versions.as_array().ok_or(())?;
    if values.is_empty() || values.len() > 8 {
        return Err(());
    }
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let version = value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(())?;
        if version == 0 || result.contains(&version) {
            return Err(());
        }
        result.push(version);
    }
    Ok(result)
}

async fn negotiate(
    sink: &mut SplitSink<WebSocket, Message>,
    input: &mut SplitStream<WebSocket>,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    let first = loop {
        match tokio::time::timeout_at(deadline, input.next()).await {
            Ok(Some(Ok(Message::Text(text)))) if text.len() <= CONTROL_LIMIT => {
                break serde_json::from_str::<serde_json::Value>(&text).ok();
            }
            Ok(Some(Ok(Message::Ping(bytes)))) => {
                if !send(sink, Message::Pong(bytes)).await {
                    return None;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Err(_) => break Some(serde_json::json!({})),
            _ => return None,
        }
    };
    let versions = first.as_ref().ok_or(()).and_then(client_versions);
    let error = match versions {
        Ok(ref versions) if versions.contains(&CALL_PROTOCOL_VERSION) => return first,
        Ok(versions) => CallEvent::ProtocolError {
            code: CallProtocolErrorCode::UnsupportedProtocolVersion,
            client_versions: versions,
            server_versions: vec![CALL_PROTOCOL_VERSION],
        },
        Err(()) => CallEvent::ProtocolError {
            code: CallProtocolErrorCode::InvalidProtocolVersion,
            client_versions: vec![],
            server_versions: vec![CALL_PROTOCOL_VERSION],
        },
    };
    let _ = send_event(sink, &error).await;
    let _ = send(
        sink,
        Message::Close(Some(CloseFrame {
            code: 1002,
            reason: "unsupported or invalid call protocol version".into(),
        })),
    )
    .await;
    None
}

async fn control_socket(socket: WebSocket, app: App, claims: Claims, account: String) {
    let (mut sink, mut input) = socket.split();
    let Some(first) = negotiate(&mut sink, &mut input).await else {
        return;
    };
    if ensure_current(&app, &claims).await.is_err() {
        return;
    }
    if serde_json::from_value::<CallClientHello>(first).is_err() {
        let _ = send_event(
            &mut sink,
            &CallEvent::Error {
                code: CallErrorCode::InvalidRequest,
            },
        )
        .await;
        return;
    }
    let Ok(service) = app.core.call_service(&account) else {
        return;
    };
    let control_allowed = claims.require(Some(&account), CALLS_SCOPE).is_ok();
    let Ok(mut connection) = service
        .connect(&account, &principal(&claims), control_allowed)
        .await
    else {
        return;
    };
    let client_id = connection.client_id.clone();
    let mut valid_tick = interval(Duration::from_secs(1));
    let mut ping_tick = interval(Duration::from_secs(15));
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            _ = valid_tick.tick() => {
                if last_seen.elapsed() >= IDLE_TIMEOUT || ensure_current(&app, &claims).await.is_err() { break; }
            }
            _ = ping_tick.tick() => {
                if !send(&mut sink, Message::Ping(Default::default())).await { break; }
            }
            event = connection.events.recv() => {
                let Some(event) = event else { break; };
                if ensure_current(&app, &claims).await.is_err()
                    || service.validate_control(&client_id).await.is_err()
                    || !send_event(&mut sink, &event).await { break; }
            }
            message = input.next() => {
                let Some(Ok(message)) = message else { break; };
                last_seen = Instant::now();
                if ensure_current(&app, &claims).await.is_err() { break; }
                match message {
                    Message::Text(text) if text.len() <= CONTROL_LIMIT => {
                        let Ok(command) = serde_json::from_str::<CallCommand>(&text) else {
                            let _ = send_event(&mut sink, &CallEvent::Error { code: CallErrorCode::InvalidRequest }).await;
                            break;
                        };
                        let passive = matches!(&command, CallCommand::Ready { ready: false, .. });
                        if !passive && claims.require(Some(&account), CALLS_SCOPE).is_err() {
                            let _ = send_event(&mut sink, &CallEvent::Error { code: CallErrorCode::Forbidden }).await;
                            break;
                        }
                        if service.command(&client_id, command).await.is_err() { break; }
                    }
                    Message::Ping(bytes) => { if !send(&mut sink, Message::Pong(bytes)).await { break; } }
                    Message::Pong(_) => {}
                    _ => break,
                }
            }
        }
    }
    service.disconnect(&client_id).await;
    let _ = send(&mut sink, Message::Close(None)).await;
}

async fn media_socket(socket: WebSocket, app: App, claims: Claims, account: String, call: String) {
    let Ok(service) = app.core.call_service(&account) else {
        return;
    };
    let (mut sink, mut input) = socket.split();
    let Some(first) = negotiate(&mut sink, &mut input).await else {
        return;
    };
    if ensure_current(&app, &claims).await.is_err() {
        return;
    }
    let Ok(CallMediaCommand::Attach {
        client_id,
        owner_token,
        ..
    }) = serde_json::from_value(first)
    else {
        return;
    };
    let mut connection = match service
        .attach_media(
            &account,
            &principal(&claims),
            &call,
            &client_id,
            &owner_token,
        )
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            let _ = send_event(&mut sink, &CallEvent::Error { code: error.0 }).await;
            return;
        }
    };
    if !send_event(&mut sink, &connection.ready).await {
        service.detach_media(&connection.id).await;
        return;
    }
    let CallEvent::MediaReady { send_track, .. } = &connection.ready else {
        service.detach_media(&connection.id).await;
        return;
    };
    let Ok(format) = AudioFormat::new(
        send_track.format.sample_rate,
        send_track.format.channels,
        send_track.format.frame_ms,
    ) else {
        service.detach_media(&connection.id).await;
        return;
    };
    let scope = MediaScope {
        app: &app,
        claims: &claims,
        service: &service,
        id: &connection.id,
    };
    let trace = std::env::var("WX_CALL_AUDIO_TRACE").as_deref() == Ok("1");
    let mut receive_trace = trace.then(ReceiveTrace::default);
    let mut reply_trace = trace.then(ReplyTrace::default);
    // Only the writer touches the sink. Limit queued pongs if the peer floods
    // control frames while the socket is blocked.
    let (pongs, pending_pongs) = mpsc::channel(4);
    let (stop, shutdown) = watch::channel(());
    let reason = {
        let writer = pin!(write_media(
            &scope,
            &mut sink,
            &mut connection.frames,
            pending_pongs,
            format,
            &mut receive_trace,
            shutdown.clone(),
        ));
        let reader = pin!(read_media(
            &scope,
            &mut input,
            pongs,
            send_track.id,
            format,
            &mut reply_trace,
            shutdown,
        ));
        let (reason, finishing) = match select(writer, reader).await {
            Either::Left((reason, reader)) => (reason, Either::Left(reader)),
            Either::Right((reason, writer)) => (reason, Either::Right(writer)),
        };
        // Revoke ownership promptly, then finish any fence the other direction
        // has already taken from the service. Dropping it mid-await could lose
        // pending maintenance work.
        stop.send_replace(());
        service.detach_media(&connection.id).await;
        finishing.await;
        reason
    };
    if let Some(trace) = &mut receive_trace {
        trace.report(connection.frames.pending_frames());
    }
    if let Some(trace) = &mut reply_trace {
        trace.report();
    }
    if trace {
        eprintln!("[wechat-call-audio] daemon close reason={reason}");
    }
    let _ = send(&mut sink, Message::Close(None)).await;
}

struct MediaScope<'a> {
    app: &'a App,
    claims: &'a Claims,
    service: &'a CallService,
    id: &'a str,
}

#[derive(Default)]
struct ReceiveTrace {
    dequeued: u64,
    written: u64,
    dropped: u64,
    peak_depth: usize,
    producer_gap_min: Option<Duration>,
    producer_gap_max: Duration,
    queue_age_max: Duration,
    credential_max: Duration,
    delivery_check_max: Duration,
    socket_write_max: Duration,
}
impl ReceiveTrace {
    fn report(&mut self, pending_frames: usize) {
        if self.dequeued != 0 || self.dropped != 0 || pending_frames != 0 {
            eprintln!(
                "[wechat-call-audio] daemon receive dequeued={} written={} dropped={} pending_frames={} peak_depth={} producer_gap_min_us={} producer_gap_max_us={} queue_age_max_us={} credential_max_us={} delivery_check_max_us={} socket_write_max_us={}",
                self.dequeued,
                self.written,
                self.dropped,
                pending_frames,
                self.peak_depth.max(pending_frames),
                self.producer_gap_min.unwrap_or_default().as_micros(),
                self.producer_gap_max.as_micros(),
                self.queue_age_max.as_micros(),
                self.credential_max.as_micros(),
                self.delivery_check_max.as_micros(),
                self.socket_write_max.as_micros(),
            );
        }
        *self = Self::default();
    }
}

#[derive(Default)]
struct ReplyTrace {
    chunks: u64,
    bytes: usize,
    peak_pending_bytes: usize,
    submitted_frames: u64,
    credential_max: Duration,
    backend_max: Duration,
}
impl ReplyTrace {
    fn report(&mut self) {
        if self.chunks != 0 || self.submitted_frames != 0 || self.peak_pending_bytes != 0 {
            eprintln!(
                "[wechat-call-audio] daemon reply chunks={} bytes={} peak_pending_bytes={} submitted_frames={} credential_max_us={} backend_max_us={}",
                self.chunks,
                self.bytes,
                self.peak_pending_bytes,
                self.submitted_frames,
                self.credential_max.as_micros(),
                self.backend_max.as_micros(),
            );
        }
        *self = Self::default();
    }
}

async fn write_media(
    scope: &MediaScope<'_>,
    sink: &mut SplitSink<WebSocket, Message>,
    frames: &mut MediaReceiveQueue,
    mut pongs: mpsc::Receiver<Message>,
    format: AudioFormat,
    trace: &mut Option<ReceiveTrace>,
    mut shutdown: watch::Receiver<()>,
) -> &'static str {
    let mut ping_tick = interval(Duration::from_secs(15));
    let mut trace_tick = interval(Duration::from_secs(1));
    trace_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while !shutdown.has_changed().unwrap_or(true) {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ping_tick.tick() => {
                if !send(sink, Message::Ping(Default::default())).await { return "media-ping-send-failed"; }
            }
            pong = pongs.recv() => {
                let Some(pong) = pong else { return "media-pong-source-closed"; };
                if !send(sink, pong).await { return "media-pong-send-failed"; }
            }
            _ = trace_tick.tick(), if trace.is_some() => {
                if let Some(trace) = trace { trace.report(frames.pending_frames()); }
            }
            frame = frames.recv_delivery() => {
                let frame = match frame {
                    Ok(MediaDelivery::Audio(frame)) => frame,
                    Ok(MediaDelivery::Control(event)) => {
                        if ensure_current(scope.app, scope.claims).await.is_err()
                            || !matches!(scope.service.media_delivery_allowed(scope.id).await, Ok(true)) {
                            return "media-track-event-fenced";
                        }
                        let Ok(text) = serde_json::to_string(&event) else { return "media-track-encode-failed"; };
                        if !send(sink, Message::Text(text.into())).await { return "media-track-send-failed"; }
                        continue;
                    }
                    Err(RecvError::Lagged(dropped)) => {
                        if let Some(trace) = trace {
                            trace.dropped += dropped;
                            trace.peak_depth = trace.peak_depth.max(frames.pending_frames());
                        }
                        continue;
                    }
                    Err(RecvError::Closed) => return "media-receiver-closed",
                };
                if let Some(trace) = trace {
                    trace.dequeued += 1;
                    trace.peak_depth = trace.peak_depth.max(1 + frames.pending_frames());
                    trace.queue_age_max = trace.queue_age_max.max(frame.enqueued_at.elapsed());
                    if let Some(gap) = frame.producer_gap {
                        trace.producer_gap_min = Some(trace.producer_gap_min.map_or(gap, |min| min.min(gap)));
                        trace.producer_gap_max = trace.producer_gap_max.max(gap);
                    }
                }
                let start = Instant::now();
                let credential = ensure_current(scope.app, scope.claims).await;
                if let Some(trace) = trace {
                    trace.credential_max = trace.credential_max.max(start.elapsed());
                }
                if credential.is_err() || frame.bytes.len() > MEDIA_LIMIT { return "media-receive-fenced"; }
                let start = Instant::now();
                let delivery = scope.service.media_delivery_allowed(scope.id).await;
                if let Some(trace) = trace {
                    trace.delivery_check_max = trace.delivery_check_max.max(start.elapsed());
                }
                match delivery {
                    Ok(true) => {
                        let Ok(frame) = MediaFrame::decode(&frame.bytes, format) else { return "media-received-frame-invalid"; };
                        let start = Instant::now();
                        let chunk = CallMediaCommand::AudioChunk {
                            track_id: frame.header.track_id,
                            generation: frame.header.generation,
                            pcm_base64: BASE64.encode(frame.payload()),
                            sequence: Some(frame.header.sequence),
                            sample_offset: Some(frame.header.timestamp_micros * u64::from(format.sample_rate()) / 1_000_000),
                        };
                        let Ok(text) = serde_json::to_string(&chunk) else { return "media-encode-failed"; };
                        let sent = send(sink, Message::Text(text.into())).await;
                        if let Some(trace) = trace {
                            trace.socket_write_max = trace.socket_write_max.max(start.elapsed());
                            trace.written += u64::from(sent);
                        }
                        if !sent { return "media-receive-send-failed"; }
                    }
                    Ok(false) => {},
                    Err(_) => return "media-delivery-fenced",
                }
            }
        }
    }
    "media-other-direction-closed"
}

async fn read_media(
    scope: &MediaScope<'_>,
    input: &mut SplitStream<WebSocket>,
    pongs: mpsc::Sender<Message>,
    track: u32,
    format: AudioFormat,
    trace: &mut Option<ReplyTrace>,
    mut shutdown: watch::Receiver<()>,
) -> &'static str {
    let mut valid_tick = interval(Duration::from_secs(1));
    let mut trace_tick = interval(Duration::from_secs(1));
    trace_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_seen = Instant::now();
    while !shutdown.has_changed().unwrap_or(true) {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = valid_tick.tick() => {
                if last_seen.elapsed() >= IDLE_TIMEOUT { return "media-idle-timeout"; }
                if ensure_current(scope.app, scope.claims).await.is_err() { return "media-credential-fenced"; }
                if scope.service.validate_media(scope.id).await.is_err() {
                    return "media-validation-failed";
                }
            }
            _ = trace_tick.tick(), if trace.is_some() => {
                if let Some(trace) = trace {
                    trace.report();

                }
            }
            message = input.next() => {
                let Some(Ok(message)) = message else { return "media-client-stream-closed"; };
                last_seen = Instant::now();
                let credential = ensure_current(scope.app, scope.claims).await;
                if let Some(trace) = trace {
                    trace.credential_max = trace.credential_max.max(last_seen.elapsed());
                }
                if credential.is_err() { return "media-credential-fenced"; }
                match message {
                    Message::Text(text) if text.len() <= MEDIA_TEXT_LIMIT => {
                        let Ok(command) = serde_json::from_str::<CallMediaCommand>(&text) else { return "media-message-invalid"; };
                        let Ok(current) = scope.service.media_generation(scope.id).await else { return "media-generation-unavailable"; };
                        match command {
                            CallMediaCommand::AudioStart { track_id, generation } => {
                                if track_id != track || scope.service.start_media_audio(scope.id, track_id, generation).await.is_err() { return "media-generation-rejected"; }
                            }
                            CallMediaCommand::AudioChunk { track_id, generation, pcm_base64, sequence, sample_offset } => {
                                if sequence.is_some() || sample_offset.is_some() { return "media-client-clock-not-allowed"; }
                                if track_id != track || generation == 0 || generation > current { return "media-track-rejected"; }
                                if pcm_base64.len() > MEDIA_LIMIT.div_ceil(3) * 4 { return "media-chunk-too-large"; }
                                let Ok(bytes) = BASE64.decode(&pcm_base64) else { return "media-base64-invalid"; };
                                if bytes.is_empty() || !bytes.len().is_multiple_of(format.channels() as usize * 2) || bytes.len() > MEDIA_LIMIT { return "media-pcm-invalid"; }
                                if generation < current { continue; }
                                let start = Instant::now();
                                let Ok(pending) = scope.service.submit_pcm(scope.id, track_id, generation, &bytes).await else { return "media-pcm-rejected"; };
                                if let Some(trace) = trace {
                                    trace.chunks += 1;
                                    trace.bytes += bytes.len();
                                    trace.peak_pending_bytes = trace.peak_pending_bytes.max(pending);
                                    trace.backend_max = trace.backend_max.max(start.elapsed());
                                }
                            }
                            _ => return "media-unexpected-message",
                        }
                    }
                    Message::Ping(bytes) => {
                        if pongs.try_send(Message::Pong(bytes)).is_err() { return "media-pong-backpressure"; }
                    }
                    Message::Pong(_) => {}
                    _ => return "media-unexpected-message",
                }
            }
        }
    }
    "media-other-direction-closed"
}

#[cfg(test)]
mod socket_tests;
