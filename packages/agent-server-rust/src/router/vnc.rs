use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;

/// Proxy a noVNC WebSocket connection to the local websockify instance.
/// Auth is enforced by the middleware layer before this handler runs.
pub async fn vnc_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_vnc_ws(socket, 6080))
}

async fn handle_vnc_ws(ws: WebSocket, websockify_port: u16) {
    let tcp = match TcpStream::connect(format!("127.0.0.1:{websockify_port}")).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to connect to websockify: {e}");
            return;
        }
    };

    let ws_url = format!("ws://127.0.0.1:{websockify_port}/websockify");
    let req = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(&ws_url)
        .header("Sec-WebSocket-Protocol", "binary")
        .body(())
        .unwrap();

    let (upstream, _) = match tokio_tungstenite::client_async(req, tcp).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("WebSocket handshake with websockify failed: {e}");
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut up_tx, mut up_rx) = upstream.split();

    // Client → websockify
    let client_to_upstream = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let tung_msg = match msg {
                Message::Binary(data) => tokio_tungstenite::tungstenite::Message::Binary(data.into()),
                Message::Text(text) => tokio_tungstenite::tungstenite::Message::Text(text.as_str().into()),
                Message::Ping(data) => tokio_tungstenite::tungstenite::Message::Ping(data.into()),
                Message::Pong(data) => tokio_tungstenite::tungstenite::Message::Pong(data.into()),
                Message::Close(_) => break,
            };
            if up_tx.send(tung_msg).await.is_err() {
                break;
            }
        }
    };

    // Websockify → client
    let upstream_to_client = async {
        while let Some(Ok(msg)) = up_rx.next().await {
            let axum_msg = match msg {
                tokio_tungstenite::tungstenite::Message::Binary(data) => Message::Binary(data.into()),
                tokio_tungstenite::tungstenite::Message::Text(text) => Message::Text(text.as_str().into()),
                tokio_tungstenite::tungstenite::Message::Ping(data) => Message::Ping(data.into()),
                tokio_tungstenite::tungstenite::Message::Pong(data) => Message::Pong(data.into()),
                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                _ => continue,
            };
            if ws_tx.send(axum_msg).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = client_to_upstream => {}
        _ = upstream_to_client => {}
    }
}

/// Serve noVNC static files from /opt/novnc.
pub async fn vnc_static(uri: Uri) -> Response {
    let path = uri.path().strip_prefix("/vnc/").unwrap_or("");
    let path = if path.is_empty() { "vnc.html" } else { path };

    // Prevent path traversal
    if path.contains("..") {
        return StatusCode::FORBIDDEN.into_response();
    }

    let file_path = format!("/opt/novnc/{path}");

    let Ok(content) = tokio::fs::read(&file_path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let content_type = match file_path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };

    ([(header::CONTENT_TYPE, content_type)], content).into_response()
}
