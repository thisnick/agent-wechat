//! UI-hygiene endpoints (token-protected): close known WeChat popups,
//! version-robust open-chat.

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::tools::ui_open_chat::open_chat_a11y_search;
use crate::tools::ui_popups::close_known_popups;

#[derive(Serialize)]
pub struct ClosePopupsResponse {
    /// "ok" | "xdotool_missing".
    status: &'static str,
    popup_detected: bool,
    popup_closed: bool,
    closed_count: usize,
    windows_seen: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    popup_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<&'static str>,
}

/// POST /api/ui/close-known-popups — close whitelisted popups (currently the
/// Weixin version-update window) by clicking their top-right close X. Redacted
/// response: no window titles, screenshots, or chat/contact/message data.
pub async fn close_known_popups_handler() -> Json<ClosePopupsResponse> {
    let o = close_known_popups().await;
    Json(ClosePopupsResponse {
        status: if o.xdotool_available { "ok" } else { "xdotool_missing" },
        popup_detected: o.popup_detected,
        popup_closed: o.popup_closed,
        closed_count: o.closed_count,
        windows_seen: o.windows_seen,
        popup_type: o.popup_type,
        warning: if o.xdotool_available {
            None
        } else {
            Some("xdotool_unavailable")
        },
    })
}

#[derive(Deserialize)]
pub struct OpenChatRequest {
    #[serde(rename = "chatId")]
    chat_id: String,
    #[serde(default, rename = "dryRun")]
    dry_run: bool,
}

#[derive(Serialize)]
pub struct OpenChatResponse {
    /// "ok" on success; otherwise mirrors `error`.
    status: &'static str,
    method: &'static str,
    chat_id_present: bool,
    resolved_name_present: bool,
    resolved_name_length: usize,
    search_box_present: bool,
    result_clicked: bool,
    open_confirmed: bool,
    /// not_logged_in | chat_not_found_in_db | display_name_missing |
    /// search_box_not_found | result_not_found | open_not_confirmed |
    /// unknown_build_profile | xdotool_missing | a11y_unavailable
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

/// POST /api/ui/open-chat — open a chat by id via the version-robust a11y
/// search path (id → display name via decrypted DB → search box → first
/// result). Body: `{ "chatId": "...", "dryRun": false }`. Redacted: never
/// returns the chat name/id or any chat content.
pub async fn open_chat_handler(Json(req): Json<OpenChatRequest>) -> Json<OpenChatResponse> {
    let o = open_chat_a11y_search(&req.chat_id, req.dry_run).await;
    Json(OpenChatResponse {
        status: if o.error.is_none() { "ok" } else { o.error.unwrap() },
        method: o.method,
        chat_id_present: o.chat_id_present,
        resolved_name_present: o.resolved_name_present,
        resolved_name_length: o.resolved_name_length,
        search_box_present: o.search_box_present,
        result_clicked: o.result_clicked,
        open_confirmed: o.open_confirmed,
        error: o.error,
    })
}
