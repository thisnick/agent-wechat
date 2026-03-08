pub mod auth;
mod chats;
mod contacts;
mod debug;
mod events;
mod messages;
mod sessions;
mod status;
mod vnc;

use axum::{
    extract::DefaultBodyLimit,
    http::Method,
    routing::{get, post},
    Json, Router,
};
use tower_http::cors::{Any, CorsLayer};

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// Build the full axum Router.
pub fn build_router() -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_origin(Any)
        .allow_headers(Any);

    Router::new()
        // Health (exempt from auth via middleware check)
        .route("/health", get(health))
        // Status
        .route("/api/status", get(status::get_status))
        .route("/api/status/auth", get(status::auth_status))
        .route("/api/status/login", post(status::login))
        .route("/api/status/logout", post(status::logout))
        // Chats
        .route("/api/chats", get(chats::list_chats))
        .route("/api/chats/{id}", get(chats::get_chat))
        .route("/api/chats/find", get(chats::find_chats))
        .route("/api/chats/{id}/open", post(chats::open_chat))
        // Contacts
        .route("/api/contacts", get(contacts::list_contacts))
        .route("/api/contacts/find", get(contacts::find_contacts))
        // Messages
        .route("/api/messages/{chat_id}", get(messages::list_messages))
        .route(
            "/api/messages/{chat_id}/media/{local_id}",
            get(messages::get_media),
        )
        .route("/api/messages/send", post(messages::send_message))
        // Debug
        .route("/api/debug/screenshot", get(debug::screenshot))
        .route("/api/debug/a11y", get(debug::a11y))
        // Sessions
        .route("/api/sessions", get(sessions::list_sessions).post(sessions::create_session))
        .route("/api/sessions/{id}", get(sessions::get_session).delete(sessions::delete_session))
        .route("/api/sessions/{id}/start", post(sessions::start_session))
        .route("/api/sessions/{id}/stop", post(sessions::stop_session))
        // WebSocket for login subscription
        .route("/api/ws/login", get(status::login_ws))
        // Events WebSocket
        .route("/api/ws/events", get(events::events_ws))
        // VNC: WebSocket proxy + static files (behind auth)
        .route("/vnc/websockify", get(vnc::vnc_ws))
        .route("/vnc/{*path}", get(vnc::vnc_static))
        .route("/vnc/", get(vnc::vnc_static))
        // Middleware: auth → body limit → CORS (applied bottom-up)
        .layer(axum::middleware::from_fn(auth::auth_middleware))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)) // 50 MB for media uploads
        .layer(cors)
}
