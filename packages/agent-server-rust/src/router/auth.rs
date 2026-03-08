use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::OnceLock;

static AUTH_TOKEN: OnceLock<String> = OnceLock::new();

/// Read the auth token from env var or file. Panics if neither is found.
pub fn init_token() {
    let token = std::env::var("AGENT_WECHAT_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/data/auth-token")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|t| !t.is_empty())
        })
        .expect(
            "No auth token found. Set AGENT_WECHAT_TOKEN env var or mount a token file at /data/auth-token",
        );

    AUTH_TOKEN.set(token).ok();
}

pub fn get_token() -> &'static str {
    AUTH_TOKEN.get().expect("init_token() must be called first")
}

pub async fn auth_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path();

    // Health endpoint is exempt
    if path == "/health" {
        return next.run(req).await;
    }

    // noVNC static assets are exempt (the WebSocket endpoint /vnc/websockify still requires auth)
    if path.starts_with("/vnc/") && path != "/vnc/websockify" {
        return next.run(req).await;
    }

    let expected = get_token();

    // Check Authorization: Bearer <token> header
    if let Some(val) = req.headers().get("authorization") {
        if let Ok(s) = val.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                if token == expected {
                    return next.run(req).await;
                }
            }
        }
    }

    // Check ?token=<token> query param
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("token=") {
                if val == expected {
                    return next.run(req).await;
                }
            }
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "Unauthorized"})),
    )
        .into_response()
}
