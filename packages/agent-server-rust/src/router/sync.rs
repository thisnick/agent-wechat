//! Account rescan / data-plane recovery endpoint.
//!
//! Context (issue #153 + GRAILA AW-FORK findings): the WeChat account-dir
//! detection (`tools::wechat_db::find_account_dir`) that persists
//! `session.logged_in_user` runs ONLY inside the `WS /api/ws/login` LoginPlan,
//! in the `DetectingUser` phase. Two real login paths bypass it entirely:
//!   - phone-confirm login (the client logs in out-of-band), and
//!   - connecting `/api/ws/login` while already logged in (the plan sees
//!     `mainWindow=chat` and only normalizes the window — never DetectingUser).
//! In both cases `auth_status` independently reports `logged_in` (via a11y),
//! but `logged_in_user` stays NULL, so `/api/chats`, `/api/contacts` and
//! `/api/messages` short-circuit to empty.
//!
//! This endpoint lets an operator actively re-run detection + key extraction
//! against the already-running WeChat client WITHOUT logging out or re-scanning.
//! It reuses the exact same helpers the LoginPlan uses, so behavior matches the
//! normal happy path.
//!
//! All logging and the JSON response are REDACTED: no wxid, no filesystem path,
//! no key material, no token.

use axum::Json;
use rusqlite::params;
use serde::Serialize;

use crate::db::{get_db, queries};
use crate::sessions::manager::get_session;
use crate::tools::wechat_db::{find_account_dir_with_method, find_wechat_pid};
use crate::tools::wechat_keys::{extract_keys_async, needs_key_extraction, store_keys};

#[derive(Serialize)]
pub struct RescanResponse {
    /// "ok" on success; otherwise a machine-readable failure reason:
    /// "no_session" | "no_wechat_pid" | "no_account_dir".
    status: &'static str,
    wechat_pid_present: bool,
    account_detected: bool,
    /// "pid_fd" | "related_pid_fd" | "filesystem_fallback" | "none".
    account_detect_method: &'static str,
    logged_in_user_before: bool,
    logged_in_user_after: bool,
    key_extraction_needed: bool,
    key_extraction_triggered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    notes: Vec<&'static str>,
}

impl RescanResponse {
    fn failure(
        status: &'static str,
        reason: &'static str,
        wechat_pid_present: bool,
        account_detect_method: &'static str,
        logged_in_user_before: bool,
    ) -> Self {
        RescanResponse {
            status,
            wechat_pid_present,
            account_detected: false,
            account_detect_method,
            logged_in_user_before,
            logged_in_user_after: logged_in_user_before,
            key_extraction_needed: false,
            key_extraction_triggered: false,
            reason: Some(reason),
            notes: Vec::new(),
        }
    }
}

/// POST /api/sync/rescan — recover the data plane when WeChat is logged in but
/// `logged_in_user` was never persisted (issue #153). Token-protected by the
/// global auth middleware. Body is ignored (no params required).
pub async fn rescan() -> Json<RescanResponse> {
    // 0. Best-effort: dismiss any whitelisted popup (e.g. the Weixin update
    //    window) that could obscure the main UI / disrupt detection. Safe and
    //    side-effect free if none is present.
    let popups = crate::tools::ui_popups::close_known_popups().await;
    if popups.popup_detected {
        tracing::info!(
            "[rescan] pre_close_popups detected={} closed={}",
            popups.popup_detected,
            popups.popup_closed
        );
    }

    // 1. Resolve the session.
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            tracing::warn!("[rescan] no_session");
            return Json(RescanResponse::failure(
                "no_session",
                "no_session",
                false,
                "none",
                false,
            ));
        }
    };
    let session_id = session.id.clone();
    let logged_in_user_before = session.logged_in_user.is_some();

    // 2. Resolve the WeChat PID (prefer the session's, fall back to a live scan).
    let wechat_pid = match session.wechat_pid.or_else(find_wechat_pid) {
        Some(p) => p,
        None => {
            tracing::warn!("[rescan] wechat_pid_present=false");
            return Json(RescanResponse::failure(
                "no_wechat_pid",
                "no_wechat_pid",
                false,
                "none",
                logged_in_user_before,
            ));
        }
    };
    tracing::info!("[rescan] wechat_pid_present=true");

    // Persist the PID back onto the session (cheap; keeps DetectingUser-equivalent state).
    {
        let db = get_db();
        let now = chrono::Utc::now().to_rfc3339();
        db.execute(
            "UPDATE sessions SET wechat_pid = ?1, updated_at = ?2 WHERE id = ?3",
            params![wechat_pid, now, session_id],
        )
        .ok();
    }

    // 3. Detect the account dir (patched multi-PID + filesystem fallback).
    let (account_dir, method) = find_account_dir_with_method(wechat_pid);
    let account_dir = match account_dir {
        Some(a) => a,
        None => {
            tracing::warn!("[rescan] account_detected=false method={method}");
            return Json(RescanResponse::failure(
                "no_account_dir",
                "no_account_dir",
                true,
                method,
                logged_in_user_before,
            ));
        }
    };
    tracing::info!("[rescan] account_detected=true method={method}");

    // 4. Persist logged_in_user (clearing stale data if the account changed).
    let key_extraction_needed;
    {
        let db = get_db();
        let previous = queries::get_session_logged_in_user(&db, &session_id);
        if previous.as_ref().filter(|p| *p != &account_dir).is_some() {
            queries::clear_session_data(&db, &session_id);
        }
        queries::update_session_logged_in_user(&db, &session_id, Some(&account_dir));
        // Evaluate while still holding the guard; this is a sync call.
        key_extraction_needed = needs_key_extraction(&db, &session_id, &account_dir);
    } // MutexGuard dropped before the await below.

    // 5. Trigger key extraction if needed (mirrors handle_detecting_user ->
    //    handle_extracting_keys). Non-fatal: logged_in_user is already set, and
    //    list_chats also lazily re-extracts on demand.
    let mut key_extraction_triggered = false;
    if key_extraction_needed {
        let keys = extract_keys_async(wechat_pid).await;
        if keys.is_empty() {
            tracing::error!("[rescan] key_extraction_failed");
        } else {
            let db = get_db();
            store_keys(&db, &session_id, &account_dir, &keys);
            key_extraction_triggered = true;
            tracing::info!("[rescan] key_extraction_triggered=true");
        }
    }

    Json(RescanResponse {
        status: "ok",
        wechat_pid_present: true,
        account_detected: true,
        account_detect_method: method,
        logged_in_user_before,
        logged_in_user_after: true,
        key_extraction_needed,
        key_extraction_triggered,
        reason: None,
        notes: Vec::new(),
    })
}
