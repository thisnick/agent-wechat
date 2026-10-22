use crate::{
    contract::{Control, Request as RpcRequest, MAX_BODY},
    engine::Engine,
};
use anyhow::{ensure, Result};
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use subtle::ConstantTimeEq;

struct App {
    engine: Arc<Engine>,
    token_hash: [u8; 32],
    stopping: Arc<AtomicBool>,
}

pub fn router(engine: Arc<Engine>, token: &str) -> Result<(Router, Arc<AtomicBool>)> {
    ensure!(token.len() >= 32, "invalid_simulator_token");
    let stopping = Arc::new(AtomicBool::new(false));
    let app = Arc::new(App {
        engine,
        token_hash: Sha256::digest(token.as_bytes()).into(),
        stopping: stopping.clone(),
    });
    let protected = Router::new()
        .route("/rpc", post(rpc))
        .route("/control", post(control))
        .route("/state", get(snapshot))
        .route("/shutdown", post(shutdown))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
        .with_state(app);
    Ok((
        Router::new()
            .route(
                "/health",
                get(|| async { Json(json!({"status":"ok","upstream":"simulator"})) }),
            )
            .merge(protected)
            .layer(DefaultBodyLimit::max(MAX_BODY)),
        stopping,
    ))
}
async fn authorize(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let valid = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| {
            bool::from(<[u8; 32]>::from(Sha256::digest(token.as_bytes())).ct_eq(&app.token_hash))
        });
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        )
            .into_response();
    }
    next.run(request).await
}
async fn work(
    app: Arc<App>,
    task: impl FnOnce(&Engine) -> Result<Value> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(move || task(&app.engine)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        result => {
            let code = match result {
                Ok(Err(error)) => error.to_string(),
                _ => "simulator_unavailable".into(),
            };
            let safe =
                if code.len() <= 80 && code.bytes().all(|c| c.is_ascii_lowercase() || c == b'_') {
                    code.as_str()
                } else {
                    "simulator_request_failed"
                };
            let status = if safe == "simulated_lost_acknowledgment" {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(json!({"error":safe}))).into_response()
        }
    }
}
async fn rpc(State(app): State<Arc<App>>, Json(request): Json<RpcRequest>) -> Response {
    work(app, move |engine| engine.request(request)).await
}
async fn control(State(app): State<Arc<App>>, Json(control): Json<Control>) -> Response {
    work(app, move |engine| engine.control(control)).await
}
async fn snapshot(State(app): State<Arc<App>>) -> Response {
    work(app, Engine::snapshot).await
}
async fn shutdown(State(app): State<Arc<App>>) -> Json<Value> {
    app.stopping.store(true, Ordering::Release);
    Json(json!({"stopping":true}))
}
