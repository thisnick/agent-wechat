use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use base64::Engine;
use crate::context::create_context;
use crate::db::get_db;
use crate::execution::run_execution_loop;
use crate::ia::types::*;
use crate::ia::{find_state_by_id, identify_states};
use crate::plans::login::{LoginParams, LoginPlan};
use crate::plans::logout::{LogoutParams, LogoutPlan};
use crate::sessions::manager::get_session;
use crate::tools::a11y::get_a11y_desktop;
use crate::tools::exec::ExecOptions;
use crate::tools::qr::{decode_qr_from_base64, to_data_url};
use crate::tools::screenshot::capture_screenshot;

pub async fn get_status() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "container": "running",
        "loginState": { "status": "logged_out" },
        "version": "0.1.0"
    }))
}

/// Check auth status via one FSM observation cycle.
///
/// Gets the a11y tree, identifies the current state, and runs
/// the reducer. Chat states set `is_logged_in = true`.
pub async fn auth_status() -> Json<serde_json::Value> {
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            return Json(serde_json::json!({
                "status": "unknown",
            }))
        }
    };

    // Check if WeChat process is running first
    let wechat_running = crate::tools::wechat_db::find_wechat_pid().is_some();
    if !wechat_running {
        return Json(serde_json::json!({
            "status": "app_not_running",
            "loggedInUser": session.logged_in_user,
        }));
    }

    let exec_options = ExecOptions {
        session: Some(session.clone()),
        timeout_ms: 30_000,
    };

    // Run one observation: a11y → identify → reduce
    let a11y = match get_a11y_desktop(&exec_options).await {
        Ok(tree) => tree,
        Err(_) => {
            return Json(serde_json::json!({
                "status": "unknown",
                "loggedInUser": session.logged_in_user,
            }))
        }
    };

    let screenshot = capture_screenshot(&exec_options)
        .await
        .unwrap_or_default();
    let identified = identify_states(&a11y, &screenshot);

    // Load persisted state and apply reduce
    let mut context = {
        let db = get_db();
        create_context(session.clone(), &db)
    };

    if let Some(ref mw) = identified.main_window {
        if let Some(state_impl) = find_state_by_id(&mw.state_id) {
            let screenshot_bytes = base64::engine::general_purpose::STANDARD
                .decode(&screenshot)
                .unwrap_or_default();
            context.state = state_impl.reduce(&ReduceArgs {
                prev: &context.state,
                a11y: &a11y,
                screenshot: &screenshot_bytes,
            });
        }
    }

    // Save updated state
    {
        let db = get_db();
        context.save(&db);
    }

    let status = if context.state.main_window.is_logged_in {
        "logged_in"
    } else {
        "logged_out"
    };

    tracing::info!(
        "[auth_status] view={:?}, status={}",
        context.state.main_window.view,
        status
    );

    Json(serde_json::json!({
        "status": status,
        "loggedInUser": session.logged_in_user,
    }))
}

/// Log out of WeChat via FSM execution loop.
pub async fn logout() -> Json<serde_json::Value> {
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            return Json(serde_json::json!({
                "success": false,
                "error": "No session available"
            }))
        }
    };

    // Quick auth check first
    let exec_options = ExecOptions {
        session: Some(session.clone()),
        timeout_ms: 30_000,
    };

    let a11y = match get_a11y_desktop(&exec_options).await {
        Ok(tree) => tree,
        Err(e) => {
            return Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to get a11y tree: {e}")
            }))
        }
    };

    let screenshot = capture_screenshot(&exec_options).await.unwrap_or_default();
    let identified = identify_states(&a11y, &screenshot);

    // Load persisted state and check if logged in
    let mut context = {
        let db = get_db();
        create_context(session.clone(), &db)
    };

    if let Some(ref mw) = identified.main_window {
        if let Some(state_impl) = find_state_by_id(&mw.state_id) {
            let screenshot_bytes = base64::engine::general_purpose::STANDARD
                .decode(&screenshot)
                .unwrap_or_default();
            context.state = state_impl.reduce(&ReduceArgs {
                prev: &context.state,
                a11y: &a11y,
                screenshot: &screenshot_bytes,
            });
        }
    }

    if !context.state.main_window.is_logged_in {
        return Json(serde_json::json!({
            "success": false,
            "error": "Not logged in"
        }));
    }

    // Run logout FSM
    let cancel = CancellationToken::new();
    let plan = LogoutPlan;
    let params = LogoutParams;
    let emit = |_event: SubscriptionEvent| {};
    let (result, _) = run_execution_loop(&plan, &params, &mut context, &emit, cancel).await;

    if result.success {
        // Clear logged_in_user from session
        let db = get_db();
        crate::db::queries::update_session_logged_in_user(&db, &session.id, None);
    }

    Json(serde_json::json!({
        "success": result.success,
        "error": result.error
    }))
}

pub async fn login() -> Json<serde_json::Value> {
    let screenshot = capture_screenshot(&ExecOptions::default()).await;

    match screenshot {
        Ok(b64) => {
            if let Some(qr_result) = decode_qr_from_base64(&b64) {
                let data_url = to_data_url(&qr_result.data).ok();
                return Json(serde_json::json!({
                    "success": false,
                    "state": { "status": "qr_pending" },
                    "qrDataUrl": data_url
                }));
            }

            Json(serde_json::json!({
                "success": false,
                "state": { "status": "qr_pending" }
            }))
        }
        Err(_) => Json(serde_json::json!({
            "success": false,
            "state": { "status": "logged_out" }
        })),
    }
}

#[derive(Deserialize)]
pub struct LoginWsParams {
    #[serde(rename = "timeoutMs", default = "default_timeout")]
    timeout_ms: u64,
    #[serde(rename = "newAccount", default)]
    new_account: bool,
}

fn default_timeout() -> u64 {
    300_000
}

pub async fn login_ws(
    ws: WebSocketUpgrade,
    Query(params): Query<LoginWsParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_login_ws(socket, params))
}

async fn handle_login_ws(mut socket: WebSocket, params: LoginWsParams) {
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            let msg = serde_json::to_string(&LoginSubscriptionEvent::Error {
                message: "No session available".to_string(),
            })
            .unwrap();
            let _ = socket.send(Message::Text(msg.into())).await;
            return;
        }
    };

    // Send initial status
    let msg = serde_json::to_string(&LoginSubscriptionEvent::Status {
        message: "Navigating login flow...".to_string(),
    })
    .unwrap();
    if socket.send(Message::Text(msg.into())).await.is_err() {
        return;
    }

    // Channel to bridge sync emit callback → async WebSocket sends
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubscriptionEvent>();
    let cancel = CancellationToken::new();
    let cancel_for_exec = cancel.clone();
    let login_params = LoginParams {
        new_account: params.new_account,
    };

    // Spawn the execution loop in a separate task
    let exec_handle = tokio::spawn(async move {
        let mut context = {
            let db = get_db();
            create_context(session, &db)
        };
        let plan = LoginPlan;
        let emit = move |event: SubscriptionEvent| {
            let _ = tx.send(event);
        };
        run_execution_loop(&plan, &login_params, &mut context, &emit, cancel_for_exec).await.0
    });

    // Main loop: bridge events to WebSocket, handle timeout + disconnect
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(params.timeout_ms));
    tokio::pin!(timeout);
    let mut sent_terminal = false;
    let mut client_disconnected = false;
    let mut server_timeout = false;

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(evt) => {
                        let ws_event = subscription_event_to_login_event(evt);
                        if is_terminal_login_event(&ws_event) {
                            sent_terminal = true;
                        }
                        let msg = serde_json::to_string(&ws_event).unwrap();
                        if socket.send(Message::Text(msg.into())).await.is_err() {
                            cancel.cancel();
                            client_disconnected = true;
                            break;
                        }
                    }
                    None => break, // channel closed = execution done
                }
            }
            _ = &mut timeout => {
                cancel.cancel();
                server_timeout = true;
                sent_terminal = true;
                let msg = serde_json::to_string(&LoginSubscriptionEvent::LoginTimeout).unwrap();
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    client_disconnected = true;
                }
                break;
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(_)) => continue,
                    _ => {
                        cancel.cancel();
                        client_disconnected = true;
                        break;
                    }
                }
            }
        }
    }

    // Wait for execution to finish and emit a fallback terminal event if needed.
    let exec_result = exec_handle.await.ok();
    if !client_disconnected && !sent_terminal {
        let fallback = match exec_result {
            Some(result) if result.success => LoginSubscriptionEvent::LoginSuccess { user_id: None },
            Some(result) => {
                let message = result.error.unwrap_or_else(|| "Login failed".to_string());
                if message.starts_with("Unknown state for")
                    || message.starts_with("Execution timeout after")
                    || (server_timeout && message == "Aborted")
                {
                    LoginSubscriptionEvent::LoginTimeout
                } else {
                    LoginSubscriptionEvent::Error { message }
                }
            }
            None => LoginSubscriptionEvent::Error {
                message: "Login execution task failed".to_string(),
            },
        };
        let msg = serde_json::to_string(&fallback).unwrap();
        let _ = socket.send(Message::Text(msg.into())).await;
    }
}

fn is_terminal_login_event(event: &LoginSubscriptionEvent) -> bool {
    matches!(
        event,
        LoginSubscriptionEvent::LoginSuccess { .. }
            | LoginSubscriptionEvent::LoginTimeout
            | LoginSubscriptionEvent::Error { .. }
    )
}

/// Convert generic SubscriptionEvent (from plans) to typed LoginSubscriptionEvent (for WS).
fn subscription_event_to_login_event(event: SubscriptionEvent) -> LoginSubscriptionEvent {
    match event.event_type.as_str() {
        "status" => LoginSubscriptionEvent::Status {
            message: event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "qr" => {
            let qr_data = event
                .data
                .get("qrData")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let qr_data_url = to_data_url(&qr_data).ok();
            LoginSubscriptionEvent::Qr {
                qr_data,
                qr_binary_data: None,
                qr_data_url,
            }
        }
        "phone_confirm" => LoginSubscriptionEvent::PhoneConfirm {
            message: event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "login_success" => LoginSubscriptionEvent::LoginSuccess {
            user_id: event
                .data
                .get("userId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "login_timeout" => LoginSubscriptionEvent::LoginTimeout,
        "error" => LoginSubscriptionEvent::Error {
            message: event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string(),
        },
        _ => LoginSubscriptionEvent::Status {
            message: format!("Unknown event: {}", event.event_type),
        },
    }
}
