//! Authenticated control and owner-bound binary audio/video sockets, plus
//! single-use browser WebSocket ticket issuance. No account keys cross these routes.
use super::{ApiError, ApiResult, ApiRoute, App, Claims};
use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Extension, Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{
    future::{select, Either},
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    pin::pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{broadcast::error::RecvError, mpsc, watch},
    time::{interval, timeout, Instant, MissedTickBehavior},
};
use wechat_calls::audio::AudioFormat;
use wechat_core::calls::pcm_envelope::{
    MediaPayloadKind, WireMediaFrame, MAX_AUDIO_PAYLOAD_BYTES, MAX_FRAME_BYTES,
};
use wechat_core::{
    call_types::{
        CallClientHello, CallCommand, CallErrorCode, CallEvent, CallMediaCommand,
        CallProtocolErrorCode, CallSnapshot, CallTicketRequest, CallTicketResponse,
        CallTicketSocket, CallTrack, AUDIO_SEND_TRACK_ID, CALL_CONTROL_PROTOCOL_VERSION,
        CALL_CONTROL_SUPPORTED_VERSIONS, CALL_PROTOCOL_VERSION,
    },
    calls::service::{CallService, CallServiceError, MediaDelivery, MediaReceiveQueue},
};

const CONTROL_LIMIT: usize = 16 * 1024;
const MEDIA_BINARY_LIMIT: usize = MAX_FRAME_BYTES;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TICKET_TTL_SECONDS: u32 = 20;
const TICKET_TTL: Duration = Duration::from_secs(TICKET_TTL_SECONDS as u64);
const MAX_PENDING_TICKETS: usize = 256;

/// Token scope that every calls route requires.
const CALLS_SCOPE: &str = "calls";
const CONTROL_PATH: &str = "/v1/accounts/{a}/calls/socket";
const MEDIA_PATH: &str = "/v1/accounts/{a}/calls/{call}/media";
const TICKETS_PATH: &str = "/v1/accounts/{a}/calls/tickets";

struct IssuedTicket {
    account: String,
    claims: Claims,
    socket: CallTicketSocket,
    call_id: Option<String>,
    expires_at: Instant,
}

/// Short-lived, single-use opaque tickets authorizing one browser WebSocket upgrade.
#[derive(Clone, Default)]
pub(crate) struct CallSocketTickets {
    entries: Arc<Mutex<BTreeMap<[u8; 32], IssuedTicket>>>,
}

impl CallSocketTickets {
    fn issue(
        &self,
        account: &str,
        claims: Claims,
        socket: CallTicketSocket,
        call_id: Option<String>,
    ) -> ApiResult<CallTicketResponse> {
        match (socket, &call_id) {
            (CallTicketSocket::Control, Some(_)) => {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
            }
            (CallTicketSocket::Media, None) => {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
            }
            (CallTicketSocket::Media, Some(id))
                if id.is_empty()
                    || id.len() > 200
                    || !id.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
                    }) =>
            {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
            }
            _ => {}
        }
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "operation_failed"))?;
        entries.retain(|_, item| item.expires_at > now);
        if entries.len() >= MAX_PENDING_TICKETS {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "capacity"));
        }
        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        let mut ticket = String::with_capacity(64);
        for byte in raw {
            use std::fmt::Write as _;
            let _ = write!(&mut ticket, "{byte:02x}");
        }
        let digest: [u8; 32] = Sha256::digest(ticket.as_bytes()).into();
        let expires_at_unix = wechat_store::unix_seconds() + i64::from(TICKET_TTL_SECONDS);
        entries.insert(
            digest,
            IssuedTicket {
                account: account.to_owned(),
                claims,
                socket,
                call_id: call_id.clone(),
                expires_at: now + TICKET_TTL,
            },
        );
        Ok(CallTicketResponse {
            ticket,
            expires_at: expires_at_unix,
            expires_in_seconds: TICKET_TTL_SECONDS,
            socket,
            call_id,
        })
    }

    pub(crate) fn consume(
        &self,
        ticket: &str,
        account: &str,
        socket: CallTicketSocket,
        call_id: Option<&str>,
    ) -> Option<Claims> {
        if ticket.len() != 64 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let now = Instant::now();
        let digest: [u8; 32] = Sha256::digest(ticket.as_bytes()).into();
        let mut entries = self.entries.lock().ok()?;
        entries.retain(|_, item| item.expires_at > now);
        let entry = entries.remove(&digest)?;
        if entry.expires_at <= now
            || entry.account != account
            || entry.socket != socket
            || entry.call_id.as_deref() != call_id
        {
            return None;
        }
        Some(entry.claims)
    }
}

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
        protocol_version: CALL_CONTROL_PROTOCOL_VERSION,
        doc: "Call commands and events as JSON text. Send call.hello within five seconds of connecting, then wait for hello. Negotiates v4 (adds call.peer_status; clients ignore unknown future event types after hello) or legacy v3 (suppresses call.peer_status). Authenticate with an Authorization: Bearer header or a single-use ?ticket= issued by POST /v1/accounts/{a}/calls/tickets.",
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
            variants: Variants::Except(&["media.ready", "video.keyframe_requested"]),
        }],
    },
    CallSocket {
        name: "media",
        path: MEDIA_PATH,
        scope: CALLS_SCOPE,
        protocol_version: CALL_PROTOCOL_VERSION,
        doc: "Audio and video for one call you own. Send media.attach within five seconds of connecting, then wait for media.ready. JSON text messages carry track lifecycle and keyframe requests; binary messages carry a 36-byte big-endian WXCM v3 header followed by PCM16 audio or Annex-B H.265/H.264 or JPEG video bytes. Authenticate with an Authorization: Bearer header or a single-use ?ticket= issued by POST /v1/accounts/{a}/calls/tickets.",
        client: &[SocketMessage {
            type_name: "CallMediaCommand",
            variants: Variants::All,
        }],
        server: &[SocketMessage {
            type_name: "CallEvent",
            variants: Variants::Only(&[
                "call.error",
                "media.ready",
                "protocol.error",
                "track.added",
                "track.removed",
                "track.updated",
                "video.keyframe_requested",
            ]),
        }],
    },
];

pub(super) fn routes() -> Router<App> {
    Router::new()
        .api_route("/v1/accounts/{a}/calls/current", get(current))
        .api_route(TICKETS_PATH, post(create_ticket))
        .api_route(CONTROL_PATH, get(control))
        .api_route(MEDIA_PATH, get(media))
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

async fn create_ticket(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path(account): Path<String>,
    Json(body): Json<CallTicketRequest>,
) -> ApiResult<impl IntoResponse> {
    claims.require(Some(&account), CALLS_SCOPE)?;
    ensure_current(&app, &claims).await?;
    app.core.store.account(&account)?;
    let response = app
        .call_tickets
        .issue(&account, claims, body.socket, body.call_id)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(response)))
}

/// Accepts header bearer auth or a single-use short-lived `?ticket=` bound to this
/// exact account and socket. Persistent bearer credentials are never accepted in URLs.
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
        .max_message_size(MEDIA_BINARY_LIMIT)
        .max_frame_size(MEDIA_BINARY_LIMIT)
        .on_upgrade(move |socket| media_socket(socket, app, claims, account, call)))
}

async fn ensure_current(app: &App, claims: &Claims) -> ApiResult<()> {
    if app.core.stopping.load(std::sync::atomic::Ordering::Acquire) {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon_stopping"));
    }
    if claims.grant.is_none() {
        return Ok(());
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
    supported_versions: &[u32],
) -> Option<(serde_json::Value, u32)> {
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
        Ok(ref versions) => {
            if let Some(selected) = supported_versions
                .iter()
                .copied()
                .filter(|version| versions.contains(version))
                .max()
            {
                return first.map(|value| (value, selected));
            }
            CallEvent::ProtocolError {
                code: CallProtocolErrorCode::UnsupportedProtocolVersion,
                client_versions: versions.clone(),
                server_versions: supported_versions.to_vec(),
            }
        }
        Err(()) => CallEvent::ProtocolError {
            code: CallProtocolErrorCode::InvalidProtocolVersion,
            client_versions: vec![],
            server_versions: supported_versions.to_vec(),
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
    let Some((first, negotiated_version)) =
        negotiate(&mut sink, &mut input, CALL_CONTROL_SUPPORTED_VERSIONS).await
    else {
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
    let emit_peer_status = negotiated_version >= CALL_CONTROL_PROTOCOL_VERSION;
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
                let Some(mut event) = event else { break; };
                if !emit_peer_status && matches!(event, CallEvent::PeerStatus { .. }) {
                    continue;
                }
                if let CallEvent::Hello { protocol_version, .. } = &mut event {
                    *protocol_version = negotiated_version;
                }
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
    let Some((first, _)) = negotiate(&mut sink, &mut input, &[CALL_PROTOCOL_VERSION]).await else {
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
    let CallEvent::MediaReady { send_tracks, .. } = &connection.ready else {
        service.detach_media(&connection.id).await;
        return;
    };
    let Some(CallTrack::Audio {
        id: audio_track_id,
        format: audio_format,
        ..
    }) = send_tracks
        .iter()
        .find(|track| matches!(track, CallTrack::Audio { id, .. } if *id == AUDIO_SEND_TRACK_ID))
    else {
        service.detach_media(&connection.id).await;
        return;
    };
    let Ok(format) = AudioFormat::new(
        audio_format.sample_rate,
        audio_format.channels,
        audio_format.frame_ms,
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
            &mut receive_trace,
            shutdown.clone(),
        ));
        let reader = pin!(read_media(
            &scope,
            &mut input,
            pongs,
            *audio_track_id,
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
                        if ensure_current(scope.app, scope.claims).await.is_err() {
                            return "media-track-event-fenced";
                        }
                        match scope.service.media_delivery_allowed(scope.id).await {
                            Ok(true) => {
                                let Ok(text) = serde_json::to_string(&event) else { return "media-track-encode-failed"; };
                                if !send(sink, Message::Text(text.into())).await { return "media-track-send-failed"; }
                            }
                            Ok(false) => {}
                            Err(_) => return "media-track-event-fenced",
                        }
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
                if credential.is_err() || frame.bytes.len() > MEDIA_BINARY_LIMIT { return "media-receive-fenced"; }
                let start = Instant::now();
                let delivery = scope.service.media_delivery_allowed(scope.id).await;
                if let Some(trace) = trace {
                    trace.delivery_check_max = trace.delivery_check_max.max(start.elapsed());
                }
                match delivery {
                    Ok(true) => {
                        if WireMediaFrame::decode(&frame.bytes).is_err() {
                            return "media-received-frame-invalid";
                        }
                        let start = Instant::now();
                        let sent = send(sink, Message::Binary(frame.bytes.into())).await;
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
    audio_track: u32,
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
                    Message::Text(text) if text.len() <= CONTROL_LIMIT => {
                        let Ok(command) = serde_json::from_str::<CallMediaCommand>(&text) else { return "media-message-invalid"; };
                        match command {
                            CallMediaCommand::TrackStart { track_id, generation } => {
                                if scope.service.start_media_track(scope.id, track_id, generation).await.is_err() {
                                    return "media-generation-rejected";
                                }
                            }
                            CallMediaCommand::VideoRequestKeyframe { track_id } => {
                                if scope.service.request_video_keyframe(scope.id, track_id).await.is_err() {
                                    return "media-keyframe-rejected";
                                }
                            }
                            CallMediaCommand::Attach { .. } => return "media-unexpected-message",
                        }
                    }
                    Message::Binary(bytes) if bytes.len() <= MEDIA_BINARY_LIMIT => {
                        let Ok(frame) = WireMediaFrame::decode(&bytes) else {
                            return "media-binary-invalid";
                        };
                        match frame.kind {
                            MediaPayloadKind::PcmS16le => {
                                let Ok(current) = scope.service.media_generation(scope.id).await else {
                                    return "media-generation-unavailable";
                                };
                                let track_id = frame.header.track_id;
                                let generation = frame.header.generation;
                                if track_id != audio_track || generation == 0 || generation > current {
                                    return "media-track-rejected";
                                }
                                let payload = frame.payload();
                                if payload.is_empty()
                                    || !payload.len().is_multiple_of(format.channels() as usize * 2)
                                    || payload.len() > MAX_AUDIO_PAYLOAD_BYTES
                                {
                                    return "media-pcm-invalid";
                                }
                                if generation < current {
                                    continue;
                                }
                                let start = Instant::now();
                                let Ok(pending) = scope.service.submit_pcm(scope.id, track_id, generation, payload).await else {
                                    return "media-pcm-rejected";
                                };
                                if let Some(trace) = trace {
                                    trace.chunks += 1;
                                    trace.bytes += payload.len();
                                    trace.peak_pending_bytes = trace.peak_pending_bytes.max(pending);
                                    trace.backend_max = trace.backend_max.max(start.elapsed());
                                }
                            }
                            kind if kind.is_video() => {
                                if scope
                                    .service
                                    .submit_video(
                                        scope.id,
                                        frame.header.track_id,
                                        frame.header.generation,
                                        kind,
                                        frame.flags,
                                        frame.header.timestamp_micros,
                                        frame.payload(),
                                    )
                                    .await
                                    .is_err()
                                {
                                    return "media-video-rejected";
                                }
                            }
                            _ => return "media-binary-invalid",
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
