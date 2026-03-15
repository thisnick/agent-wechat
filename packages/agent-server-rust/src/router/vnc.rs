use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};

/// Proxy a noVNC WebSocket connection to the local websockify instance.
/// Auth is enforced by the middleware layer before this handler runs.
pub async fn vnc_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
    // noVNC requires the "binary" subprotocol — must echo it back or the client drops the connection
    ws.protocols(["binary"])
        .on_upgrade(|socket| handle_vnc_ws(socket, 6080))
}

async fn handle_vnc_ws(ws: WebSocket, websockify_port: u16) {
    let ws_url = format!("ws://127.0.0.1:{websockify_port}/websockify");

    let (upstream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("WebSocket connection to websockify failed: {e}");
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

/// Landing page: if ?token= is present, load noVNC with auto-connect.
/// Otherwise show a password prompt.
pub async fn vnc_static(uri: Uri) -> Response {
    let path = uri.path().strip_prefix("/vnc/").unwrap_or("");
    let path = if path.is_empty() { "" } else { path };

    // Serve the landing page at /vnc/ (no file path)
    if path.is_empty() {
        let has_token = uri
            .query()
            .map(|q| q.split('&').any(|p| p.starts_with("token=")))
            .unwrap_or(false);

        if has_token {
            // Token present: serve noVNC with the token injected into the WebSocket path
            return serve_novnc_with_token(uri.query().unwrap_or("")).await;
        } else {
            // No token: show password prompt
            return Html(LOGIN_PAGE).into_response();
        }
    }

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

/// Read vnc.html from noVNC and inject a script that sets the WebSocket path
/// to include the auth token.
async fn serve_novnc_with_token(query: &str) -> Response {
    // Extract token from query string
    let token = query
        .split('&')
        .find_map(|p| p.strip_prefix("token="))
        .unwrap_or("");

    let autoconnect = query
        .split('&')
        .any(|p| p == "autoconnect=true" || p == "autoconnect=1");

    // Read the noVNC vnc.html
    let Ok(html_bytes) = tokio::fs::read("/opt/novnc/vnc.html").await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let html = String::from_utf8_lossy(&html_bytes);

    // Inject a script before </head> that overrides the WebSocket path to include the token
    let inject = format!(
        r#"<script>
        // Inject token into noVNC WebSocket connection
        (function() {{
            var token = "{}";
            var autoconnect = {};
            // Set URL params that noVNC reads
            var url = new URL(window.location);
            url.searchParams.set('path', 'vnc/websockify?token=' + encodeURIComponent(token));
            url.searchParams.set('token', token);
            if (autoconnect) url.searchParams.set('autoconnect', 'true');
            window.history.replaceState({{}}, '', url);
        }})();
        </script>"#,
        token.replace('\\', "\\\\").replace('"', "\\\""),
        autoconnect,
    );

    let modified = html.replacen("</head>", &format!("{inject}</head>"), 1);

    Html(modified).into_response()
}

const LOGIN_PAGE: &str = r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>noVNC - Login</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: #1a1a2e;
    color: #e0e0e0;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100vh;
  }
  .login-box {
    background: #16213e;
    border-radius: 8px;
    padding: 2rem;
    width: 100%;
    max-width: 360px;
    box-shadow: 0 4px 24px rgba(0,0,0,0.3);
  }
  h1 { font-size: 1.3rem; margin-bottom: 1.5rem; text-align: center; }
  label { display: block; margin-bottom: 0.4rem; font-size: 0.9rem; color: #aaa; }
  input[type="password"] {
    width: 100%;
    padding: 0.6rem 0.8rem;
    border: 1px solid #333;
    border-radius: 4px;
    background: #0f3460;
    color: #fff;
    font-size: 1rem;
    margin-bottom: 1rem;
  }
  input[type="password"]:focus { outline: none; border-color: #e94560; }
  button {
    width: 100%;
    padding: 0.7rem;
    background: #e94560;
    color: #fff;
    border: none;
    border-radius: 4px;
    font-size: 1rem;
    cursor: pointer;
  }
  button:hover { background: #c73e54; }
  .error { color: #e94560; font-size: 0.85rem; margin-top: 0.5rem; display: none; }
</style>
</head>
<body>
<div class="login-box">
  <h1>VNC Viewer</h1>
  <form id="form">
    <label for="token">Access Token</label>
    <input type="password" id="token" placeholder="Enter your token" autofocus required>
    <button type="submit">Connect</button>
    <div class="error" id="error">Connection failed. Check your token.</div>
  </form>
</div>
<script>
document.getElementById('form').addEventListener('submit', function(e) {
  e.preventDefault();
  var token = document.getElementById('token').value.trim();
  if (!token) return;
  // Verify token by hitting the websockify endpoint
  var wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  var ws = new WebSocket(wsProto + '//' + location.host + '/vnc/websockify?token=' + encodeURIComponent(token));
  ws.onopen = function() {
    ws.close();
    // Token works — redirect to noVNC with token
    window.location.href = '/vnc/?token=' + encodeURIComponent(token) + '&autoconnect=true';
  };
  ws.onerror = function() {
    document.getElementById('error').style.display = 'block';
  };
});
</script>
</body>
</html>"#;
