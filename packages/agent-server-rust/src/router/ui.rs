//! UI-hygiene endpoints (token-protected): close known WeChat popups.

use axum::Json;
use serde::Serialize;

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
