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

/// AW-FORK-22: normalize a chat name for matching (trim, lowercase, drop
/// zero-width chars, collapse internal whitespace).
fn normalize_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(*c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}'))
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// AW-FORK-22: system chats that must NEVER be chosen as a send target.
fn is_denied_system_chat(normalized: &str) -> bool {
    matches!(
        normalized,
        "file transfer" | "weixin team" | "文件传输助手" | "微信团队"
    )
}

/// Collect every descendant `name` string of a node (the row's label may sit on a
/// child of the list-item).
fn collect_names(node: &Value, out: &mut Vec<String>) {
    if let Some(name) = node.get("name").and_then(|v| v.as_str()) {
        if !name.trim().is_empty() {
            out.push(name.to_string());
        }
    }
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for c in children {
            collect_names(c, out);
        }
    }
}

/// AW-FORK-22: among search-result rows, pick the one whose (normalized) name
/// EXACTLY equals the target. Denylist system chats. Require a UNIQUE match: 0 =>
/// `no_matching_target`, >1 => `ambiguous_matches`. NEVER falls back to the first
/// row (that strayed a group send to a wrong private chat, §63). Rows are
/// `(descendant_names, rect)`. Returns `(denied, exact_matches, selected, fail)`.
fn pick_matching_row(
    rows: &[(Vec<String>, Rect)],
    target_norm: &str,
) -> (usize, usize, Option<Rect>, Option<&'static str>) {
    let mut denied = 0usize;
    let mut matches: Vec<Rect> = Vec::new();
    for (names, rect) in rows {
        let norms: Vec<String> = names
            .iter()
            .map(|n| normalize_name(n))
            .filter(|s| !s.is_empty())
            .collect();
        if norms.iter().any(|s| is_denied_system_chat(s)) {
            denied += 1;
            continue;
        }
        if !target_norm.is_empty() && norms.iter().any(|s| s == target_norm) {
            matches.push(*rect);
        }
    }
    match matches.len() {
        1 => (denied, 1, Some(matches[0]), None),
        0 => (denied, 0, None, Some("no_matching_target")),
        n => (denied, n, None, Some("ambiguous_matches")),
    }
}

struct ResultSelection {
    candidate_lists: usize,
    rows_total: usize,
    denied_count: usize,
    exact_matches: usize,
    selected: Option<Rect>,
    fail_reason: Option<&'static str>,
}

/// AW-FORK-22: select the search-result row matching the resolved target (by name
/// + denylist) instead of blindly clicking the first row (§63 stray). Keeps the
/// AW-FORK-21 candidate-list-shape diagnostics for the no-results case.
fn select_result_matching_target(
    pairs: &[(usize, &Value)],
    search_depth: usize,
    search_y: i32,
    target_norm: &str,
) -> ResultSelection {
    let mut lists_count = 0usize;
    let mut candidate_lists = 0usize;
    let mut rows: Vec<(Vec<String>, Rect)> = Vec::new();
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
        diag.push((idx, *depth, list_rect.y, list_rect.h, items.len(), first_y, cond_deeper, cond_items, cond_below));
        if cond_deeper && cond_items && cond_below {
            candidate_lists += 1;
            for it in &items {
                if let Some(r) = rect_of(it) {
                    let mut names = Vec::new();
                    collect_names(it, &mut names);
                    rows.push((names, r));
                }
            }
        }
    }
    if candidate_lists == 0 {
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
    let (denied_count, exact_matches, selected, fail_reason) = pick_matching_row(&rows, target_norm);
    ResultSelection {
        candidate_lists,
        rows_total: rows.len(),
        denied_count,
        exact_matches,
        selected,
        fail_reason,
    }
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
    // AW-FORK-22: click the search-result row whose name MATCHES the resolved
    // target — never the blind first row (that strayed a group send to a wrong
    // private chat, §63). No unique match → fail closed (no click, no keyboard
    // fallback), for BOTH send-safe and endpoint modes.
    let target_norm = normalize_name(&name);
    let sel = select_result_matching_target(&pairs, search_depth, sy, &target_norm);
    tracing::info!(
        "[open-chat] result_match rows={} candidate_lists={} denied={} exact_matches={} selected={}",
        sel.rows_total,
        sel.candidate_lists,
        sel.denied_count,
        sel.exact_matches,
        sel.selected.is_some()
    );
    match sel.selected {
        Some(rect) => {
            click_at(rect.x + rect.w / 2, rect.y + rect.h / 2).await;
            out.result_clicked = true;
        }
        None => {
            tracing::warn!(
                "[open-chat] result_match_fail reason={} fail_closed=true",
                sel.fail_reason.unwrap_or("result_no_match")
            );
            out.error = Some(sel.fail_reason.unwrap_or("result_no_match"));
            return out;
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    // 6. Confirm a chat opened (locale-robust composer detection). With
    //    target-aware row matching (AW-FORK-22) we no longer keyboard-fallback; if
    //    the matched click didn't open a chat, fail closed.
    let confirmed = a11y_tree().await.map(|t| chat_is_open(&t)).unwrap_or(false);
    out.open_confirmed = confirmed;
    if !out.open_confirmed {
        out.error = Some(if opts.send_safe {
            "open_not_confirmed_send_safe"
        } else {
            "open_not_confirmed"
        });
    }
    tracing::info!(
        "[open-chat] method=a11y_search result_clicked={} open_confirmed={}",
        out.result_clicked,
        out.open_confirmed
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{choose_search_editable, is_denied_system_chat, normalize_name, pick_matching_row, Rect};

    fn r(y: i32) -> Rect {
        Rect { x: 0, y, w: 200, h: 30 }
    }
    fn row(name: &str, y: i32) -> (Vec<String>, Rect) {
        (vec![name.to_string()], r(y))
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

    // AW-FORK-22 result matching ------------------------------------------------

    #[test]
    fn normalize_trims_lowercases_collapses() {
        assert_eq!(normalize_name("  GRAILA  Test \u{200b}Group "), "graila test group");
    }

    #[test]
    fn denylist_blocks_system_chats() {
        assert!(is_denied_system_chat("file transfer"));
        assert!(is_denied_system_chat("文件传输助手"));
        assert!(is_denied_system_chat("微信团队"));
        assert!(!is_denied_system_chat("graila test group"));
    }

    // §63 regression: first row is a WRONG private chat; the exact target is a
    // later row → that later row must be selected (no first-row fallback).
    #[test]
    fn exact_target_selected_not_first_row() {
        let rows = vec![row("Some Other Person", 100), row("GRAILA Test Group", 140)];
        let (_, exact, sel, fail) = pick_matching_row(&rows, &normalize_name("GRAILA Test Group"));
        assert_eq!(exact, 1);
        assert_eq!(sel.unwrap().y, 140);
        assert!(fail.is_none());
    }

    #[test]
    fn denied_system_row_skipped_target_selected() {
        let rows = vec![row("File Transfer", 100), row("GRAILA Test Group", 140)];
        let (denied, _, sel, _) = pick_matching_row(&rows, &normalize_name("GRAILA Test Group"));
        assert_eq!(denied, 1);
        assert_eq!(sel.unwrap().y, 140);
    }

    #[test]
    fn no_matching_target_fails_closed() {
        let rows = vec![row("Someone Else", 100), row("Another Chat", 140)];
        let (_, _, sel, fail) = pick_matching_row(&rows, &normalize_name("GRAILA Test Group"));
        assert!(sel.is_none());
        assert_eq!(fail, Some("no_matching_target"));
    }

    #[test]
    fn ambiguous_matches_fail_closed() {
        let rows = vec![row("GRAILA Test Group", 100), row("graila test group", 140)];
        let (_, _, sel, fail) = pick_matching_row(&rows, &normalize_name("GRAILA Test Group"));
        assert!(sel.is_none());
        assert_eq!(fail, Some("ambiguous_matches"));
    }

    #[test]
    fn only_denied_row_no_match() {
        let rows = vec![row("Weixin Team", 100)];
        let (denied, _, sel, fail) = pick_matching_row(&rows, &normalize_name("Weixin Team"));
        assert_eq!(denied, 1);
        assert!(sel.is_none());
        assert_eq!(fail, Some("no_matching_target"));
    }

    #[test]
    fn empty_target_never_matches() {
        let rows = vec![row("Anything", 100)];
        let (_, _, sel, _) = pick_matching_row(&rows, "");
        assert!(sel.is_none());
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
