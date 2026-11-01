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
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::time::{interval, timeout, Instant};
use wechat_core::{
    call_types::{CallCommand, CallErrorCode, CallEvent, CallMediaCommand, CallSnapshot},
    calls::service::CallServiceError,
};

const CONTROL_LIMIT: usize = 16 * 1024;
const MEDIA_LIMIT: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

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
    claims.require(Some(&account), "calls.observe")?;
    ensure_current(&app, &claims).await?;
    app.core.store.account(&account)?;
    let snapshot: CallSnapshot = app
        .core
        .call_service()
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
    claims.require(Some(&account), "calls.observe")?;
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
    claims.require(Some(&account), "calls.media")?;
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
    let service = app.core.call_service();
    let control_allowed = claims.require(Some(&account), "calls.control").is_ok();
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
                if last_seen.elapsed() > IDLE_TIMEOUT || ensure_current(&app, &claims).await.is_err() { break; }
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
                        if !passive && claims.require(Some(&account), "calls.control").is_err() {
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
    let service = app.core.call_service();
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
    let mut valid_tick = interval(Duration::from_secs(1));
    let mut ping_tick = interval(Duration::from_secs(15));
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            _ = valid_tick.tick() => {
                if last_seen.elapsed() > IDLE_TIMEOUT || ensure_current(&app, &claims).await.is_err()
                    || service.validate_media(&connection.id).await.is_err() { break; }
            }
            _ = ping_tick.tick() => { if !send(&mut sink, Message::Ping(Default::default())).await { break; } }
            frame = connection.frames.recv() => {
                let Some(frame) = frame else { break; };
                if ensure_current(&app, &claims).await.is_err() || frame.len() > MEDIA_LIMIT { break; }
                match service.media_delivery_allowed(&connection.id).await {
                    Ok(true) => { if !send(&mut sink, Message::Binary(frame.into())).await { break; } }
                    Ok(false) => {},
                    Err(_) => break,
                }
            }
            message = input.next() => {
                let Some(Ok(message)) = message else { break; };
                last_seen = Instant::now();
                if ensure_current(&app, &claims).await.is_err() { break; }
                match message {
                    Message::Binary(bytes) if bytes.len() <= MEDIA_LIMIT => {
                        if service.media_frame(&connection.id, &bytes).await.is_err() { break; }
                    }
                    Message::Ping(bytes) => { if !send(&mut sink, Message::Pong(bytes)).await { break; } }
                    Message::Pong(_) => {}
                    _ => break,
                }
            }
        }
    }
    service.detach_media(&connection.id).await;
    let _ = send(&mut sink, Message::Close(None)).await;
}

#[cfg(test)]
mod socket_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn socket_origins_require_one_matching_http_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "localhost:6175".parse().unwrap());
        assert!(check_origin(&headers).is_ok());
        for origin in [
            "null",
            "https://elsewhere.example",
            "http://localhost:6175/path",
            "http://localhost:6175/?token=value",
        ] {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert!(check_origin(&headers).is_err());
        }
        headers.insert(header::ORIGIN, "http://localhost:6175".parse().unwrap());
        assert!(check_origin(&headers).is_ok());
        headers.append(header::ORIGIN, "http://localhost:6175".parse().unwrap());
        assert!(check_origin(&headers).is_err());
    }
}
