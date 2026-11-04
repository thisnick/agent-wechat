//! Authenticated control and owner-bound PCM sockets. No account keys cross these routes.
use super::{ApiError, ApiResult, App, Claims};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{
    future::{select, Either},
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use std::{collections::VecDeque, pin::pin, time::Duration};
use tokio::{
    sync::{broadcast::error::RecvError, mpsc, watch},
    time::{interval, timeout, Instant, MissedTickBehavior},
};
use wechat_calls::{
    audio::AudioFormat,
    frame::{encode_payload, MediaFrame, MediaHeader},
};
use wechat_core::{
    call_types::{CallCommand, CallErrorCode, CallEvent, CallMediaCommand, CallSnapshot},
    calls::service::{CallService, CallServiceError, MediaReceiveQueue},
};

const CONTROL_LIMIT: usize = 16 * 1024;
const MEDIA_LIMIT: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Socket-local PCM queue. Only the daemon constructs the internal ordered
/// envelopes consumed by the core; none of those headers cross the API.
struct PcmInput {
    format: AudioFormat,
    track: u32,
    generation: Option<u32>,
    pending: VecDeque<u8>,
    sequence: u64,
    timestamp: u64,
}
impl PcmInput {
    fn start(&mut self, generation: u32, current: u32) -> Result<(), ()> {
        if generation == 0
            || generation > current
            || self.generation.is_some_and(|prior| generation <= prior)
        {
            return Err(());
        }
        self.generation = Some(generation);
        self.pending.clear();
        self.sequence = 0;
        self.timestamp = 0;
        Ok(())
    }
    fn push(&mut self, bytes: &[u8], current: u32) -> Result<(), ()> {
        let generation = self.generation.ok_or(())?;
        if bytes.is_empty() || !bytes.len().is_multiple_of(2) || bytes.len() > MEDIA_LIMIT {
            return Err(());
        }
        if generation < current {
            self.pending.clear();
            return Ok(());
        }
        // Eight seconds, including any partial codec frame. Faster producers
        // cannot grow daemon memory indefinitely or silently lose samples.
        let limit = self.format.sample_rate() as usize * self.format.channels() as usize * 2 * 8;
        if self.pending.len() + bytes.len() > limit {
            return Err(());
        }
        self.pending.extend(bytes);
        Ok(())
    }
    fn next_frame(&mut self, current: u32) -> Result<Option<Vec<u8>>, ()> {
        let Some(generation) = self.generation else {
            return Ok(None);
        };
        if generation < current {
            self.pending.clear();
            return Ok(None);
        }
        let length = self.format.samples_per_frame() * 2;
        if self.pending.len() < length {
            return Ok(None);
        }
        let payload: Vec<u8> = self.pending.drain(..length).collect();
        let frame = encode_payload(
            MediaHeader {
                track_id: self.track,
                generation,
                sequence: self.sequence,
                timestamp_micros: self.timestamp,
            },
            self.format,
            &payload,
        )
        .map_err(|_| ())?;
        self.sequence = self.sequence.checked_add(1).ok_or(())?;
        self.timestamp = self
            .timestamp
            .checked_add(u64::from(self.format.frame_ms()) * 1000)
            .ok_or(())?;
        Ok(Some(frame))
    }
}

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/v1/accounts/{a}/calls/current", get(current))
        .route("/v1/accounts/{a}/calls/socket", get(control))
        .route("/v1/accounts/{a}/calls/{call}/media", get(media))
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
    claims.require(Some(&account), "calls")?;
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
    claims.require(Some(&account), "calls")?;
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
    claims.require(Some(&account), "calls")?;
    ensure_current(&app, &claims).await?;
    check_origin(&headers)?;
    app.core.store.account(&account)?;
    Ok(upgrade
        .max_message_size(MEDIA_LIMIT)
        .max_frame_size(MEDIA_LIMIT)
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

async fn control_socket(socket: WebSocket, app: App, claims: Claims, account: String) {
    let Ok(service) = app.core.call_service(&account) else {
        return;
    };
    let control_allowed = claims.require(Some(&account), "calls").is_ok();
    let Ok(mut connection) = service
        .connect(&account, &principal(&claims), control_allowed)
        .await
    else {
        return;
    };
    let client_id = connection.client_id.clone();
    let (mut sink, mut input) = socket.split();
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
                        if !passive && claims.require(Some(&account), "calls").is_err() {
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
    let first = timeout(ATTACH_TIMEOUT, input.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = first else {
        return;
    };
    if text.len() > CONTROL_LIMIT || ensure_current(&app, &claims).await.is_err() {
        return;
    }
    let Ok(CallMediaCommand::Attach {
        client_id,
        owner_token,
    }) = serde_json::from_str(&text)
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
    let pcm = PcmInput {
        format,
        track: send_track.id,
        generation: None,
        pending: VecDeque::new(),
        sequence: 0,
        timestamp: 0,
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
            pcm,
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
            frame = frames.recv_frame() => {
                let frame = match frame {
                    Ok(frame) => frame,
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
                        let sent = send(sink, Message::Binary(frame.payload().to_vec().into())).await;
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
    mut pcm: PcmInput,
    trace: &mut Option<ReplyTrace>,
    mut shutdown: watch::Receiver<()>,
) -> &'static str {
    let mut audio_tick = interval(Duration::from_millis(u64::from(pcm.format.frame_ms())));
    // Keep a steady sample clock without adding processing time to every
    // period or emitting catch-up bursts after a scheduling stall.
    audio_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut valid_tick = interval(Duration::from_secs(1));
    let mut trace_tick = interval(Duration::from_secs(1));
    trace_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_seen = Instant::now();
    while !shutdown.has_changed().unwrap_or(true) {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = audio_tick.tick() => {
                if pcm.pending.is_empty() { continue; }
                let Ok(current) = scope.service.media_generation(scope.id).await else { return "media-generation-unavailable"; };
                match pcm.next_frame(current) {
                    Ok(Some(frame)) => {
                        let start = Instant::now();
                        let credential = ensure_current(scope.app, scope.claims).await;
                        if let Some(trace) = trace {
                            trace.credential_max = trace.credential_max.max(start.elapsed());
                        }
                        if credential.is_err() { return "media-credential-fenced"; }
                        let start = Instant::now();
                        let result = scope.service.media_frame(scope.id, &frame).await;
                        if let Some(trace) = trace {
                            trace.backend_max = trace.backend_max.max(start.elapsed());
                            trace.submitted_frames += u64::from(result.is_ok());
                        }
                        if result.is_err() {
                            return "media-frame-rejected";
                        }
                    }
                    Ok(None) => {},
                    Err(()) => return "media-frame-invalid",
                }
            }
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
                    trace.peak_pending_bytes = pcm.pending.len();
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
                    Message::Binary(bytes) if bytes.len() <= MEDIA_LIMIT => {
                        let Ok(current) = scope.service.media_generation(scope.id).await else { return "media-generation-unavailable"; };
                        if let Some(trace) = trace {
                            trace.chunks += 1;
                            trace.bytes += bytes.len();
                        }
                        if pcm.push(&bytes, current).is_err() {
                            return "media-pcm-rejected";
                        }
                        if let Some(trace) = trace {
                            trace.peak_pending_bytes = trace.peak_pending_bytes.max(pcm.pending.len());
                        }
                    }
                    Message::Text(text) if text.len() <= CONTROL_LIMIT => {
                        let Ok(CallMediaCommand::AudioStart { generation }) = serde_json::from_str(&text) else { return "media-audio-start-invalid"; };
                        let Ok(current) = scope.service.media_generation(scope.id).await else { return "media-generation-unavailable"; };
                        if pcm.start(generation, current).is_err() { return "media-generation-rejected"; }
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
