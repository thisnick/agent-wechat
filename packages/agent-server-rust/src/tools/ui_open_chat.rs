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
    /// AW-FORK-20: a send-safe Escape was issued before searching because another
    /// chat was open (so the search-results list becomes detectable).
    pub pre_search_unfocus: bool,
    /// One of the specific error codes (see module/endpoint docs); None on success.
    pub error: Option<&'static str>,
}

/// AW-FORK-20: whether to issue a send-safe pre-search "unfocus" (Escape). With a
/// chat already open, the post-type search-results list can't be distinguished
/// from the main chat list (candidate_lists=0) and a send-safe open fails closed
/// (AW-FORK-19C). We Escape back to the no-chat-open layout first — only in
/// send-safe mode, and only when a chat is actually open.
pub fn needs_pre_search_unfocus(send_safe: bool, chat_open: bool) -> bool {
    send_safe && chat_open
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

#[derive(Clone, Copy)]
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

/// Like `collect` but records each node's depth (root = 0).
fn collect_with_depth<'a>(node: &'a Value, depth: usize, out: &mut Vec<(usize, &'a Value)>) {
    out.push((depth, node));
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for c in children {
            collect_with_depth(c, depth + 1, out);
        }
    }
}

/// Direct `list-item` children of a node.
fn list_item_children(node: &Value) -> Vec<&Value> {
    node.get("children")
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().filter(|c| role_of(c) == "list-item").collect())
        .unwrap_or_default()
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

/// Find the search box with its tree depth: prefer a FOCUSED EDITABLE
/// text/entry node, else the topmost EDITABLE one. In the chat-list state this
/// is WeChat's search field. The depth lets us distinguish the (deep)
/// search-results list from the (shallow) main chat-list.
/// AW-FORK-21B: choose the search box from EDITABLE candidates as the **topmost**
/// one (smallest y). The WeChat search field sits at the top-left of the main
/// window; a chat's message composer sits at the BOTTOM and is FOCUSED while a
/// chat is open. The old `focused.or(topmost)` let a focused bottom composer win,
/// so the resolved name was typed into the composer and no search results appeared
/// (`candidate_lists=0`, AW-FORK-21). Never prefer a focused editable here.
/// Candidates are `(depth, rect, focused)`.
fn choose_search_editable(candidates: &[(usize, Rect, bool)]) -> Option<(usize, Rect, bool)> {
    candidates.iter().copied().min_by_key(|(_, r, _)| r.y)
}

fn find_search_box_node(tree: &Value) -> Option<(usize, Rect)> {
    let mut pairs = Vec::new();
    collect_with_depth(tree, 0, &mut pairs);
    let mut candidates: Vec<(usize, Rect, bool)> = Vec::new();
    for (depth, n) in &pairs {
        let role = role_of(n);
        if has_state(n, "EDITABLE")
            && (role.contains("text") || role.contains("entry") || role.contains("field"))
        {
            if let Some(r) = rect_of(n) {
                candidates.push((*depth, r, has_state(n, "FOCUSED")));
            }
        }
    }
    let chosen = choose_search_editable(&candidates);
    if let Some((_, r, _)) = &chosen {
        let focused_y = candidates
            .iter()
            .filter(|(_, _, f)| *f)
            .map(|(_, rr, _)| rr.y)
            .min()
            .unwrap_or(-1);
        let ignored = candidates.iter().any(|(_, rr, f)| *f && rr.y > r.y);
        tracing::info!(
            "[open-chat] search_box_policy=topmost_editable search_box_selected_y={} focused_editable_y={} focused_editable_ignored_as_composer={}",
            r.y,
            focused_y,
            ignored
        );
    }
    chosen.map(|(d, r, _)| (d, r))
}

/// Choose the first row of the **search-results** list (not the main chat-list).
/// Returns (lists_count, candidate_lists_count, Some((depth, item_count, first_item_rect))).
///
/// WeChat 4.1.1 renders search results as a SEPARATE, deeper `list` than the
/// main conversation list. The main chat-list is shallow (small depth) with many
/// items; the search-results list is deeper than the focused search box, with a
/// small item count, appearing below the search box. Selecting the global
/// topmost `list-item` (the old logic) wrongly hit a main-chat-list row.
fn select_result_first_item(
    pairs: &[(usize, &Value)],
    search_depth: usize,
    search_y: i32,
) -> (usize, usize, Option<(usize, usize, Rect)>) {
    let mut lists_count = 0usize;
    let mut candidates: Vec<(usize, usize, Rect)> = Vec::new();
    // AW-FORK-21 diagnostic: per-list shape + which heuristic condition excluded it.
    // Sanitized (numbers/booleans only — no text, names, or chat content).
    let mut diag: Vec<(usize, usize, i32, i32, usize, i32, bool, bool, bool)> = Vec::new();
    for (depth, n) in pairs {
        if role_of(n) != "list" {
            continue;
        }
        let idx = lists_count;
        lists_count += 1;
        let items = list_item_children(n);
        let list_rect = rect_of(n).unwrap_or(Rect { x: -1, y: -1, w: -1, h: -1 });
        let first = items.first().and_then(|it| rect_of(it));
        let first_y = first.map(|r| r.y).unwrap_or(-1);
        let cond_deeper = *depth > search_depth;
        let cond_items = !items.is_empty() && items.len() < 10;
        let cond_below = first.map(|r| r.y >= search_y).unwrap_or(false);
        diag.push((
            idx, *depth, list_rect.y, list_rect.h, items.len(), first_y,
            cond_deeper, cond_items, cond_below,
        ));
        // Search-results list heuristic: deeper than the search box, modest item
        // count (the main chat-list has many), first row at/below the search box.
        if cond_deeper && cond_items && cond_below {
            if let Some(r) = first {
                candidates.push((*depth, items.len(), r));
            }
        }
    }
    if candidates.is_empty() {
        tracing::info!(
            "[open-chat-diagnostic] candidate_lists=0 lists_count={} search_depth={} search_y={}",
            lists_count, search_depth, search_y
        );
        for (idx, depth, list_y, list_h, item_count, first_y, c_deep, c_items, c_below) in &diag {
            tracing::info!(
                "[open-chat-diagnostic] list idx={} depth={} list_y={} list_h={} item_count={} first_item_y={} cond_deeper_than_search={} cond_items_lt10={} cond_first_at_or_below_search={}",
                idx, depth, list_y, list_h, item_count, first_y, c_deep, c_items, c_below
            );
        }
    }
    let chosen = candidates.iter().copied().min_by(|a, b| {
        let da = (a.2.y - search_y).abs();
        let db = (b.2.y - search_y).abs();
        da.cmp(&db).then(b.0.cmp(&a.0))
    });
    (lists_count, candidates.len(), chosen)
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

/// True iff the currently-FOCUSED editable node is the search box (its center
/// lies within the known search-box rect). Used to gate any keyboard `Return`:
/// if focus is a chat composer (or unknown, or no search rect), this returns
/// false so we never press Return into a composer (AW-FORK-8D stray-send fix).
fn focused_editable_is_search_box(tree: &Value, search_rect: Option<Rect>) -> bool {
    let search = match search_rect {
        Some(r) => r,
        None => return false, // coordinate-fallback search box → cannot verify → unsafe
    };
    let mut nodes = Vec::new();
    collect(tree, &mut nodes);
    for n in &nodes {
        let role = role_of(n);
        if has_state(n, "FOCUSED")
            && has_state(n, "EDITABLE")
            && (role.contains("text") || role.contains("entry") || role.contains("field"))
        {
            return match rect_of(n) {
                Some(r) => {
                    let cx = r.x + r.w / 2;
                    let cy = r.y + r.h / 2;
                    cx >= search.x - 5
                        && cx <= search.x + search.w + 5
                        && cy >= search.y - 5
                        && cy <= search.y + search.h + 5
                }
                None => false,
            };
        }
    }
    false
}

/// Issue Down+Return to open the first search result — but ONLY after verifying
/// the focused editable is the search box. Returns true if the keys were sent,
/// false if blocked (unsafe focus → never press Return into a composer).
async fn safe_keyboard_open(search_rect: Option<Rect>) -> bool {
    let tree = match a11y_tree().await {
        Some(t) => t,
        None => return false,
    };
    if !focused_editable_is_search_box(&tree, search_rect) {
        tracing::warn!("[open-chat] keyboard_fallback_blocked unsafe_focus=true focused_kind=composer_or_unknown");
        return false;
    }
    let _ = xdotool(&["key", "--clearmodifiers", "Down"]).await;
    let _ = xdotool(&["key", "--clearmodifiers", "Return"]).await;
    true
}

/// Open a chat by id using the version-robust a11y search path.
/// Options controlling open-chat behavior. `send_safe` (used by the outbound
/// send path) forbids ANY keyboard fallback (Down/Return) so the open step can
/// never inject a stray message into a focused composer — the root cause of the
/// AW-FORK-8 duplicate-send defect.
#[derive(Clone, Copy)]
pub struct OpenChatOptions {
    pub dry_run: bool,
    pub allow_keyboard_fallback: bool,
    pub send_safe: bool,
}

impl OpenChatOptions {
    /// Standalone `/api/ui/open-chat`: keyboard fallback allowed.
    pub fn endpoint(dry_run: bool) -> Self {
        OpenChatOptions { dry_run, allow_keyboard_fallback: true, send_safe: false }
    }
    /// Send path: click-only, no keyboard fallback, fail closed.
    pub fn send_safe() -> Self {
        OpenChatOptions { dry_run: false, allow_keyboard_fallback: false, send_safe: true }
    }
}

/// Open a chat by id (endpoint default: keyboard fallback allowed).
pub async fn open_chat_a11y_search(chat_id: &str, dry_run: bool) -> OpenChatOutcome {
    open_chat_a11y_search_with_options(chat_id, OpenChatOptions::endpoint(dry_run)).await
}

/// Send-safe open: click-only, never presses Return, so it cannot send a
/// message. Fails closed (`open_not_confirmed_send_safe` /
/// `keyboard_fallback_suppressed`) rather than risk a stray send.
pub async fn open_chat_a11y_search_send_safe(chat_id: &str) -> OpenChatOutcome {
    open_chat_a11y_search_with_options(chat_id, OpenChatOptions::send_safe()).await
}

/// Open a chat by id using the version-robust a11y search path.
pub async fn open_chat_a11y_search_with_options(
    chat_id: &str,
    opts: OpenChatOptions,
) -> OpenChatOutcome {
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
    let search_box = find_search_box_node(&tree);
    // Coordinate fallback: WeChat's search field sits near the top-left.
    // search_depth = usize::MAX when not found → forces the keyboard fallback.
    let (mut search_depth, mut search_rect, mut sx, mut sy) = match &search_box {
        Some((d, r)) => {
            out.search_box_present = true;
            (*d, Some(*r), r.x + r.w / 2, r.y + r.h / 2)
        }
        None => (usize::MAX, None, win.x + (win.w as f64 * 0.12) as i32, win.y + 45),
    };

    tracing::info!(
        "[open-chat] method=a11y_search search_box_present={} dry_run={} send_safe={}",
        out.search_box_present,
        opts.dry_run,
        opts.send_safe
    );

    if opts.dry_run {
        // Resolve + detect only; no typing/clicking (no Escape either).
        return out;
    }

    // AW-FORK-20: send-safe reliability when ANOTHER chat is already open. With a
    // chat open, the post-type search-results list can't be distinguished from the
    // main chat list (candidate_lists=0), so a send-safe open fails closed
    // (AW-FORK-19C). Before searching, if a chat is open, press Escape to return to
    // the no-chat-open layout where the results list is detectable. Escape NEVER
    // sends a message; we still never press Enter or type into a composer. If
    // Escape fails to clear it, the later candidate_lists=0 guard still fails closed
    // (safe — no mis-route).
    if needs_pre_search_unfocus(opts.send_safe, chat_is_open(&tree)) {
        let _ = xdotool(&["key", "--clearmodifiers", "Escape"]).await;
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let mut t2 = a11y_tree().await;
        if t2.as_ref().map(|t| chat_is_open(t)).unwrap_or(false) {
            // Still open → one more Escape.
            let _ = xdotool(&["key", "--clearmodifiers", "Escape"]).await;
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            t2 = a11y_tree().await;
        }
        let chat_open_after = t2.as_ref().map(|t| chat_is_open(t)).unwrap_or(false);
        // Re-locate the search box from the refreshed tree (coords may shift).
        if let Some(t) = &t2 {
            if let Some((d, r)) = find_search_box_node(t) {
                search_depth = d;
                search_rect = Some(r);
                sx = r.x + r.w / 2;
                sy = r.y + r.h / 2;
            }
        }
        out.pre_search_unfocus = true;
        tracing::info!(
            "[open-chat] send_safe pre_search_unfocus attempted=true method=escape chat_open_before=true chat_open_after={}",
            chat_open_after
        );
    }

    // 4. Focus search, clear, type the resolved name.
    click_at(sx, sy).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = xdotool(&["key", "--clearmodifiers", "ctrl+a"]).await;
    let _ = xdotool(&["key", "--clearmodifiers", "Delete"]).await;
    let _ = xdotool(&["type", "--clearmodifiers", "--", name.as_str()]).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // 5. Click the first row of the SEARCH-RESULTS list (not a main chat-list row).
    let after_type = match a11y_tree().await {
        Some(t) => t,
        None => {
            out.error = Some("a11y_unavailable");
            return out;
        }
    };
    let mut pairs = Vec::new();
    collect_with_depth(&after_type, 0, &mut pairs);
    let (lists_count, candidate_lists, chosen) =
        select_result_first_item(&pairs, search_depth, sy);

    let mut keyboard_fallback = false;
    match chosen {
        Some((sel_depth, sel_items, first)) => {
            tracing::info!(
                "[open-chat] search_results lists_count={} candidate_lists={} selected_depth={} selected_items={}",
                lists_count,
                candidate_lists,
                sel_depth,
                sel_items
            );
            click_at(first.x + first.w / 2, first.y + first.h / 2).await;
            out.result_clicked = true;
        }
        None => {
            if opts.allow_keyboard_fallback {
                // No distinguishable results list → keyboard fallback, but ONLY
                // if the search box is focused (never Return into a composer).
                tracing::info!(
                    "[open-chat] search_results lists_count={} candidate_lists=0 keyboard_fallback_attempt=true",
                    lists_count
                );
                if safe_keyboard_open(search_rect).await {
                    keyboard_fallback = true;
                    out.result_clicked = true;
                } else {
                    out.error = Some("keyboard_fallback_unsafe_focus");
                    return out;
                }
            } else {
                // Send-safe: NEVER press Return (could send a stray message into
                // a focused composer). Fail closed instead.
                tracing::warn!(
                    "[open-chat] keyboard_fallback_suppressed send_safe={} lists_count={} candidate_lists=0",
                    opts.send_safe,
                    lists_count
                );
                out.error = Some("keyboard_fallback_suppressed");
                return out;
            }
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    // 6. Confirm a chat opened (locale-robust composer detection). In endpoint
    //    mode, a click that didn't confirm may retry via keyboard fallback. In
    //    send-safe mode we NEVER press Return — fail closed instead.
    let mut confirmed = a11y_tree().await.map(|t| chat_is_open(&t)).unwrap_or(false);
    if !confirmed && !keyboard_fallback {
        if opts.allow_keyboard_fallback {
            tracing::info!("[open-chat] click_not_confirmed retry keyboard_fallback_attempt=true");
            if safe_keyboard_open(search_rect).await {
                keyboard_fallback = true;
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                confirmed = a11y_tree().await.map(|t| chat_is_open(&t)).unwrap_or(false);
            } else {
                tracing::warn!("[open-chat] retry keyboard_fallback_blocked unsafe_focus=true");
            }
        } else {
            tracing::warn!("[open-chat] click_not_confirmed send_safe=true no_keyboard_retry");
        }
    }
    out.open_confirmed = confirmed;
    if !out.open_confirmed {
        out.error = Some(if opts.send_safe {
            "open_not_confirmed_send_safe"
        } else {
            "open_not_confirmed"
        });
    }
    tracing::info!(
        "[open-chat] method=a11y_search result_clicked={} open_confirmed={} keyboard_fallback={}",
        out.result_clicked,
        out.open_confirmed,
        keyboard_fallback
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{choose_search_editable, Rect};

    fn r(y: i32) -> Rect {
        Rect { x: 0, y, w: 200, h: 30 }
    }

    // AW-FORK-21 regression: with a chat open, the FOCUSED composer sits at the
    // bottom; the search box is the topmost editable. The topmost must win.
    #[test]
    fn topmost_editable_beats_focused_bottom_composer() {
        let search_box = (14usize, r(69), false); // top, not focused
        let composer = (16usize, r(697), true); // bottom, focused
        let chosen = choose_search_editable(&[composer, search_box]).unwrap();
        assert_eq!(chosen.1.y, 69);
        assert!(!chosen.2, "must not pick the focused composer");
    }

    #[test]
    fn focused_does_not_auto_win() {
        let top = (14usize, r(45), false);
        let focused_bottom = (16usize, r(700), true);
        assert_eq!(choose_search_editable(&[top, focused_bottom]).unwrap().1.y, 45);
    }

    #[test]
    fn single_editable_is_chosen() {
        assert_eq!(choose_search_editable(&[(14usize, r(50), true)]).unwrap().1.y, 50);
    }

    #[test]
    fn no_editable_is_none() {
        assert!(choose_search_editable(&[]).is_none());
    }
}

#[cfg(test)]
mod unfocus_tests {
    use super::needs_pre_search_unfocus;

    // AW-FORK-19C regression: a send-safe open while ANOTHER chat is open must
    // Escape back to the no-chat-open layout first (else candidate_lists=0 → fail
    // closed → no delivery).
    #[test]
    fn send_safe_with_chat_open_needs_unfocus() {
        assert!(needs_pre_search_unfocus(true, true));
    }

    #[test]
    fn send_safe_with_no_chat_open_skips_unfocus() {
        assert!(!needs_pre_search_unfocus(true, false));
    }

    // Endpoint (non-send-safe) mode keeps its keyboard fallback; no Escape pre-step.
    #[test]
    fn non_send_safe_never_unfocuses() {
        assert!(!needs_pre_search_unfocus(false, true));
        assert!(!needs_pre_search_unfocus(false, false));
    }
}
