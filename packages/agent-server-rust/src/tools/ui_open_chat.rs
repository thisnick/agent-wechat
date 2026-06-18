//! Version-robust open-chat via the WeChat search box (a11y + xdotool).
//!
//! Background (AW-FORK-7): the primary `chat-select` tool is a frida +
//! hardcoded-memory-offset approach keyed by WeChat BuildID. New WeChat builds
//! (e.g. 4.1.1.7, prefix `7b3f07cc`) have no profile, so `open_chat` hard-fails
//! and outbound send dies at "No action selected". This helper provides a
//! version-robust fallback that does what a human does — type the chat's display
//! name into the search box and click the first result — using only a11y +
//! xdotool, with NO per-build memory offsets.
//!
//! Privacy: the resolved display name / chatId are NEVER logged or returned.
//! Only redacted booleans/lengths/method are surfaced.

use super::exec::{exec_command, ExecOptions};
use super::wechat_chats;
use super::wechat_keys::get_stored_keys;
use crate::db::get_db;
use crate::ia::selectors::is_send_button_name;
use crate::sessions::manager::get_session;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct OpenChatOutcome {
    pub method: &'static str, // "a11y_search"
    pub chat_id_present: bool,
    pub resolved_name_present: bool,
    pub resolved_name_length: usize,
    pub search_box_present: bool,
    pub result_clicked: bool,
    pub open_confirmed: bool,
    /// One of the specific error codes (see module/endpoint docs); None on success.
    pub error: Option<&'static str>,
}

impl OpenChatOutcome {
    fn err(code: &'static str) -> Self {
        OpenChatOutcome {
            method: "a11y_search",
            error: Some(code),
            ..Default::default()
        }
    }
}

struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Resolve a chatId to its display name + is_group via the decrypted DB.
/// Sync (no await); never logs/returns the name itself.
fn resolve_target(chat_id: &str) -> Result<(String, bool), &'static str> {
    let session = get_session("default").ok_or("not_logged_in")?;
    let logged_in_user = session.logged_in_user.clone().ok_or("not_logged_in")?;
    let keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &logged_in_user)
    };
    if !keys.contains_key("session.db") || !keys.contains_key("contact.db") {
        // No decrypted chat DBs available → cannot resolve a name.
        return Err("chat_not_found_in_db");
    }
    match wechat_chats::get_chat_by_username(&logged_in_user, &keys, chat_id) {
        Some(chat) => {
            if chat.name.trim().is_empty() {
                Err("display_name_missing")
            } else {
                Ok((chat.name, chat.is_group))
            }
        }
        None => Err("chat_not_found_in_db"),
    }
}

async fn xdotool(args: &[&str]) -> Option<String> {
    let r = exec_command("xdotool", args, &ExecOptions::default()).await;
    if r.exit_code != 0 {
        return None;
    }
    Some(r.stdout)
}

async fn a11y_tree() -> Option<Value> {
    let r = exec_command(
        "/opt/tools/a11y-dump",
        &["--format", "json"],
        &ExecOptions {
            timeout_ms: 15_000,
            ..Default::default()
        },
    )
    .await;
    if r.exit_code != 0 {
        return None;
    }
    serde_json::from_str(&r.stdout).ok()
}

fn collect<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(node);
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for c in children {
            collect(c, out);
        }
    }
}

fn rect_of(node: &Value) -> Option<Rect> {
    let b = node.get("bounds")?;
    let x = b.get("x")?.as_f64()?;
    let y = b.get("y")?.as_f64()?;
    let w = b.get("width")?.as_f64()?;
    let h = b.get("height")?.as_f64()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(Rect {
        x: x.round() as i32,
        y: y.round() as i32,
        w: w.round() as i32,
        h: h.round() as i32,
    })
}

fn has_state(node: &Value, want: &str) -> bool {
    node.get("states")
        .and_then(|s| s.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(want)))
        .unwrap_or(false)
}

fn role_of(node: &Value) -> &str {
    node.get("role").and_then(|v| v.as_str()).unwrap_or("")
}

/// Find the search box: the topmost EDITABLE text/entry node. In the chat-list
/// state (no chat open) this is WeChat's search field.
fn find_search_box(tree: &Value) -> Option<Rect> {
    let mut nodes = Vec::new();
    collect(tree, &mut nodes);
    let mut best: Option<Rect> = None;
    for n in &nodes {
        let role = role_of(n);
        let editable = has_state(n, "EDITABLE");
        if editable && (role.contains("text") || role.contains("entry") || role.contains("field")) {
            if let Some(r) = rect_of(n) {
                // Prefer the topmost candidate (search box sits above the chat list).
                if best.as_ref().map(|b| r.y < b.y).unwrap_or(true) {
                    best = Some(r);
                }
            }
        }
    }
    best
}

/// Find the first search-result row to click: topmost `list-item` with bounds.
fn find_first_result(tree: &Value, below_y: i32) -> Option<Rect> {
    let mut nodes = Vec::new();
    collect(tree, &mut nodes);
    let mut best: Option<Rect> = None;
    for n in &nodes {
        if role_of(n) == "list-item" {
            if let Some(r) = rect_of(n) {
                if r.y >= below_y && best.as_ref().map(|b| r.y < b.y).unwrap_or(true) {
                    best = Some(r);
                }
            }
        }
    }
    best
}

/// True if a message composer is present → a chat is open. Locale-robust: the
/// send button is "Send(S)" on EN clients, "发送(S)" on ZH clients (the EMDE
/// locale). Confirmed via a send-like push-button; logs redacted counts only.
fn chat_is_open(tree: &Value) -> bool {
    let mut nodes = Vec::new();
    collect(tree, &mut nodes);
    let mut send_like = 0usize;
    let mut editable = 0usize;
    for n in &nodes {
        let name = n.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if role_of(n) == "push-button" && is_send_button_name(name) {
            send_like += 1;
        }
        if role_of(n).contains("text") && has_state(n, "EDITABLE") {
            editable += 1;
        }
    }
    tracing::info!(
        "[open-chat] confirm editable_count={} send_button_count={}",
        editable,
        send_like
    );
    send_like > 0
}

/// Largest visible Weixin window (the main UI).
async fn main_window_rect() -> Option<(String, Rect)> {
    let ids = xdotool(&["search", "--onlyvisible", "--name", "Weixin|微信|WeChat"]).await?;
    let mut best: Option<(String, Rect)> = None;
    for id in ids.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let geo = xdotool(&["getwindowgeometry", "--shell", id]).await.unwrap_or_default();
        let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        for line in geo.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("X=") { x = v.trim().parse().unwrap_or(0); }
            else if let Some(v) = line.strip_prefix("Y=") { y = v.trim().parse().unwrap_or(0); }
            else if let Some(v) = line.strip_prefix("WIDTH=") { w = v.trim().parse().unwrap_or(0); }
            else if let Some(v) = line.strip_prefix("HEIGHT=") { h = v.trim().parse().unwrap_or(0); }
        }
        let r = Rect { x, y, w, h };
        if r.w > 0 && r.h > 0 && best.as_ref().map(|(_, b)| r.w * r.h > b.w * b.h).unwrap_or(true) {
            best = Some((id.to_string(), r));
        }
    }
    best
}

async fn click_at(x: i32, y: i32) {
    let xs = x.to_string();
    let ys = y.to_string();
    let _ = xdotool(&["mousemove", xs.as_str(), ys.as_str(), "click", "1"]).await;
}

/// Open a chat by id using the version-robust a11y search path.
pub async fn open_chat_a11y_search(chat_id: &str, dry_run: bool) -> OpenChatOutcome {
    let mut out = OpenChatOutcome {
        method: "a11y_search",
        chat_id_present: !chat_id.is_empty(),
        ..Default::default()
    };

    // 1. Resolve display name from the decrypted DB (never logged).
    let (name, _is_group) = match resolve_target(chat_id) {
        Ok(v) => v,
        Err(code) => {
            out.error = Some(code);
            tracing::warn!("[open-chat] method=a11y_search resolve_failed error={code}");
            return out;
        }
    };
    out.resolved_name_present = true;
    out.resolved_name_length = name.chars().count();

    // 2. Locate + activate the main window.
    let (win_id, win) = match main_window_rect().await {
        Some(v) => v,
        None => {
            out.error = Some("xdotool_missing");
            tracing::warn!("[open-chat] method=a11y_search no_main_window");
            return out;
        }
    };
    let _ = xdotool(&["windowactivate", win_id.as_str()]).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // 3. Find the search box via a11y.
    let tree = a11y_tree().await;
    if tree.is_none() {
        out.error = Some("a11y_unavailable");
        tracing::warn!("[open-chat] method=a11y_search a11y_unavailable");
        return out;
    }
    let tree = tree.unwrap();
    let search_box = find_search_box(&tree);
    // Coordinate fallback: WeChat's search field sits near the top-left.
    let (sx, sy) = match &search_box {
        Some(r) => {
            out.search_box_present = true;
            (r.x + r.w / 2, r.y + r.h / 2)
        }
        None => (win.x + (win.w as f64 * 0.12) as i32, win.y + 45),
    };

    tracing::info!(
        "[open-chat] method=a11y_search search_box_present={} dry_run={}",
        out.search_box_present,
        dry_run
    );

    if dry_run {
        // Resolve + detect only; no typing/clicking.
        return out;
    }

    // 4. Focus search, clear, type the resolved name.
    click_at(sx, sy).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = xdotool(&["key", "--clearmodifiers", "ctrl+a"]).await;
    let _ = xdotool(&["key", "--clearmodifiers", "Delete"]).await;
    let _ = xdotool(&["type", "--clearmodifiers", "--", name.as_str()]).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // 5. Click the first search result (below the search box).
    let after_type = match a11y_tree().await {
        Some(t) => t,
        None => {
            out.error = Some("a11y_unavailable");
            return out;
        }
    };
    let result = find_first_result(&after_type, sy + 1);
    let result = match result {
        Some(r) => r,
        None => {
            out.error = Some("result_not_found");
            tracing::warn!("[open-chat] method=a11y_search result_not_found");
            return out;
        }
    };
    click_at(result.x + result.w / 2, result.y + result.h / 2).await;
    out.result_clicked = true;
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    // 6. Confirm a chat opened (message composer / Send button present).
    if let Some(confirm_tree) = a11y_tree().await {
        out.open_confirmed = chat_is_open(&confirm_tree);
    }
    if !out.open_confirmed {
        out.error = Some("open_not_confirmed");
    }
    tracing::info!(
        "[open-chat] method=a11y_search result_clicked={} open_confirmed={}",
        out.result_clicked,
        out.open_confirmed
    );
    out
}
