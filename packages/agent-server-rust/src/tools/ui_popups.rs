//! Known-popup auto-close (UI hygiene).
//!
//! WeChat occasionally raises a **version-update popup** as a SEPARATE
//! top-level X window (e.g. "Weixin 4.1.1", ~550x410) that overlaps the main
//! window. Because it is its own window — not a node inside the main window's
//! a11y tree — the execution engine's a11y `dismiss_popup` does NOT catch it.
//! Left open it obscures the main UI and can disrupt login automation, the
//! post-login state read, `/api/sync/rescan`, and any long-running adapter.
//!
//! This module closes ONLY whitelisted popups by clicking their top-right close
//! `X`, via the no-shell `exec_command` xdotool wrapper. It never types text and
//! never clicks chat/contact/input/send regions. All logging is redacted (no
//! window title text, no screenshot, no chat/contact/message data).

use super::exec::{exec_command, ExecOptions};

/// Stable tag for the only popup class currently whitelisted.
const POPUP_TYPE_WEIXIN_UPDATE: &str = "weixin_update";

#[derive(Debug, Default)]
pub struct ClosePopupsOutcome {
    pub xdotool_available: bool,
    pub windows_seen: usize,
    pub popup_detected: bool,
    pub popup_closed: bool,
    pub closed_count: usize,
    pub popup_type: Option<&'static str>,
}

struct WinInfo {
    id: String,
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Run xdotool with a fixed argument vector (no shell). Returns stdout on
/// success, None on non-zero exit / missing binary.
async fn xdotool(args: &[&str]) -> Option<String> {
    let r = exec_command("xdotool", args, &ExecOptions::default()).await;
    if r.exit_code != 0 {
        return None;
    }
    Some(r.stdout)
}

/// Enumerate visible windows with their name + geometry. Returns None when
/// xdotool is unavailable (so the caller can distinguish "no windows" from
/// "no xdotool").
async fn list_visible_windows() -> Option<Vec<WinInfo>> {
    let ids = xdotool(&["search", "--onlyvisible", "--name", "."]).await?;
    let mut wins = Vec::new();
    for id in ids.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let name = xdotool(&["getwindowname", id]).await.unwrap_or_default();
        let geo = xdotool(&["getwindowgeometry", "--shell", id])
            .await
            .unwrap_or_default();
        let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        for line in geo.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("X=") {
                x = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("Y=") {
                y = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("WIDTH=") {
                w = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("HEIGHT=") {
                h = v.trim().parse().unwrap_or(0);
            }
        }
        wins.push(WinInfo {
            id: id.to_string(),
            name: name.trim().to_string(),
            x,
            y,
            w,
            h,
        });
    }
    Some(wins)
}

fn is_weixin_named(name: &str) -> bool {
    name.contains("Weixin") || name.contains("微信") || name.contains("WeChat")
}

/// Geometry band for the update popup: ~500-650 wide, ~350-500 high. This
/// excludes the main window (h > 500) and the login small-window (w < 500).
fn in_update_popup_geometry(w: &WinInfo) -> bool {
    w.w >= 500 && w.w <= 650 && w.h >= 350 && w.h <= 500
}

fn is_update_popup_candidate(w: &WinInfo) -> bool {
    is_weixin_named(&w.name) && in_update_popup_geometry(w)
}

/// Close all whitelisted (Weixin update) popups. Best-effort and side-effect
/// safe: clicks ONLY the top-right close `X` of matching windows, once each.
pub async fn close_known_popups() -> ClosePopupsOutcome {
    let mut outcome = ClosePopupsOutcome::default();

    let windows = match list_visible_windows().await {
        Some(w) => w,
        None => {
            tracing::warn!("[popup-close] xdotool_unavailable or no windows; skipping");
            return outcome;
        }
    };
    outcome.xdotool_available = true;
    outcome.windows_seen = windows.len();

    // Safety guard: only treat a small Weixin window as the update popup when a
    // LARGER Weixin window (the main UI) is also present. Prevents ever closing
    // the main window, and avoids acting when the only window is e.g. the login
    // small-window or a transient state.
    let has_main_window = windows
        .iter()
        .any(|w| is_weixin_named(&w.name) && w.h > 500);

    let candidates: Vec<&WinInfo> = windows.iter().filter(|w| is_update_popup_candidate(w)).collect();

    if candidates.is_empty() || !has_main_window {
        return outcome; // popup_detected stays false
    }

    outcome.popup_detected = true;
    outcome.popup_type = Some(POPUP_TYPE_WEIXIN_UPDATE);

    for w in &candidates {
        // Top-right close X, nudged inside the corner.
        let cx = (w.x + w.w - 18).to_string();
        let cy = (w.y + 24).to_string();
        tracing::info!(
            "[popup-close] type={POPUP_TYPE_WEIXIN_UPDATE} action=click_close geometry={}x{} windows_seen={}",
            w.w,
            w.h,
            outcome.windows_seen
        );
        let _ = xdotool(&["windowactivate", w.id.as_str()]).await;
        let _ = xdotool(&["mousemove", cx.as_str(), cy.as_str(), "click", "1"]).await;
        outcome.closed_count += 1;
    }

    // Let the close animation settle, then verify the popup is gone.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    match list_visible_windows().await {
        Some(after) => {
            let still_present = after.iter().any(is_update_popup_candidate);
            outcome.popup_closed = !still_present;
        }
        None => {
            outcome.popup_closed = outcome.closed_count > 0;
        }
    }

    tracing::info!(
        "[popup-close] detected={} closed={} closed_count={}",
        outcome.popup_detected,
        outcome.popup_closed,
        outcome.closed_count
    );
    outcome
}
