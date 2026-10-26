//! Native listeners continue when the webview is hidden. Notifications only read
//! the local API; they never initiate a WeChat lookup or media transfer.
use crate::{config, Active, Desktop};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Notifications {
    pub cancel: CancellationToken,
    focus: Mutex<Option<Focus>>,
    pending: Mutex<Option<Target>>,
    status: Mutex<String>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Focus {
    connection_id: String,
    account_id: Option<String>,
    thread_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    connection_id: String,
    server_id: String,
    account_id: String,
    thread_id: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    cursor: String,
    seen: VecDeque<String>,
}
impl Ledger {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        ensure!(
            wechat_platform::private_path(path)? && std::fs::metadata(path)?.len() <= 512 * 1024,
            "Invalid notification state file"
        );
        let ledger: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            ledger.seen.len() <= 200 && ledger.seen.iter().all(|id| id.len() <= 2048),
            "Invalid notification receipts"
        );
        Ok(ledger)
    }
    fn claim(&mut self, path: &Path, id: &str, cursor: &str) -> Result<bool> {
        if self.seen.iter().any(|prior| prior == id) {
            return Ok(false);
        }
        ensure!(id.len() <= 2048, "Invalid notification message identity");
        self.seen.push_back(id.into());
        while self.seen.len() > 200 {
            self.seen.pop_front();
        }
        self.cursor = cursor.into();
        // Claim before calling the OS: an uncertain result must not duplicate an
        // alert. Only IDs are saved here, never content or access keys.
        config::private_write(path, &serde_json::to_vec(self)?)?;
        Ok(true)
    }
}
fn api(active: &Active, segments: &[&str]) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&active.connection.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid hub address"))?
        .clear()
        .extend(segments.iter().copied());
    Ok(url)
}
async fn response(active: &Active, url: reqwest::Url) -> Result<reqwest::Response> {
    let response = tokio::time::timeout(
        Duration::from_secs(15),
        active.http.get(url).bearer_auth(active.key.as_str()).send(),
    )
    .await??;
    Ok(response)
}
async fn get(active: &Active, url: reqwest::Url) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut response = response(active, url).await?.error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "Notification API response exceeds limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await?
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let value = value[key]
        .as_str()
        .context("Missing notification identity")?;
    ensure!(
        !value.is_empty() && value.len() <= 2048 && !value.chars().any(char::is_control),
        "Invalid notification identity"
    );
    Ok(value)
}
fn live(event: &Value) -> bool {
    event["kind"] == "message.committed"
        && event["data"]["origin"] == "live"
        && event["data"]["direction"] == "incoming"
        && event["createdAt"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|time| {
                (chrono::Utc::now() - time.with_timezone(&chrono::Utc))
                    .num_seconds()
                    .abs()
                    <= 120
            })
}
fn preview(message: &Value) -> Option<String> {
    if !message["recall"].is_null() || message["direction"] != "incoming" {
        return None;
    }
    let text = match message["type"].as_u64()? {
        1 => message["body"].as_str()?.to_owned(),
        3 => "Photo".into(),
        34 => "Voice message".into(),
        43 => "Video".into(),
        47 => "Sticker".into(),
        49 if message["appMessageType"] == 57 || message["quote"].is_object() => message["body"]
            .as_str()
            .filter(|body| !body.trim().is_empty())
            .unwrap_or("Reply")
            .into(),
        49 => match message["appMessageType"].as_u64() {
            Some(5) => "Shared link",
            Some(6) => "File",
            _ => "Attachment",
        }
        .into(),
        _ => return None,
    };
    Some(
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(180)
            .collect(),
    )
}
fn muted_or_read(thread: &Value, focus: Option<&Focus>, target: &Target, reading: bool) -> bool {
    thread["muted"].as_bool() != Some(false)
        || reading
            && focus.is_some_and(|focus| {
                focus.connection_id == target.connection_id
                    && focus.account_id.as_deref() == Some(target.account_id.as_str())
                    && focus.thread_id.as_deref() == Some(target.thread_id.as_str())
            })
}
pub fn start(app: tauri::AppHandle) -> Result<()> {
    let clicked = app.clone();
    app.notification().on_action(move |action| {
        if action.action_id() != "tap" {
            return;
        }
        let Some(target) = action
            .notification()
            .and_then(|notification| notification.extra().get("target"))
            .and_then(|target| serde_json::from_value::<Target>(target.clone()).ok())
        else {
            return;
        };
        let app = clicked.clone();
        tauri::async_runtime::spawn(async move {
            let desktop = app.state::<Desktop>();
            let active = desktop.inner.lock().await.active.clone();
            if active.is_none_or(|active| {
                active.connection.id != target.connection_id
                    || active.connection.server_id.as_deref() != Some(target.server_id.as_str())
            }) {
                return;
            }
            *desktop.notifications.pending.lock().await = Some(target.clone());
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
            let _ = app.emit("desktop-open-thread", target);
        });
    })?;
    tauri::async_runtime::spawn(coordinate(app));
    Ok(())
}
async fn coordinate(app: tauri::AppHandle) {
    let mut current: Option<Arc<Active>> = None;
    let mut cancel = CancellationToken::new();
    let mut tasks: HashMap<String, tauri::async_runtime::JoinHandle<()>> = HashMap::new();
    let mut check_at = tokio::time::Instant::now();
    loop {
        let desktop = app.state::<Desktop>();
        if desktop.notifications.cancel.is_cancelled() {
            break;
        }
        let (active, enabled) = {
            let inner = desktop.inner.lock().await;
            (inner.active.clone(), inner.config.notifications_enabled)
        };
        let active = if enabled { active } else { None };
        let unchanged = match (&current, &active) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            cancel.cancel();
            for (_, task) in tasks.drain() {
                task.abort();
            }
            current = active;
            cancel = CancellationToken::new();
            check_at = tokio::time::Instant::now();
        }
        if let Some(active) = current
            .as_ref()
            .filter(|_| tokio::time::Instant::now() >= check_at)
        {
            let result = match api(active, &["v1", "accounts"]) {
                Ok(url) => get(active, url).await,
                Err(error) => Err(error),
            };
            if let Ok(page) = result {
                let accounts: Vec<_> = page["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|account| account["authenticated"] == true)
                    .filter_map(|account| account["id"].as_str().map(str::to_owned))
                    .take(64)
                    .collect();
                tasks.retain(|id, task| {
                    if !accounts.contains(id) {
                        task.abort();
                        false
                    } else {
                        !task.inner().is_finished()
                    }
                });
                for account in accounts {
                    if tasks.contains_key(&account) {
                        continue;
                    }
                    let active = active.clone();
                    let app = app.clone();
                    let child = cancel.child_token();
                    let id = account.clone();
                    tasks.insert(id, tauri::async_runtime::spawn(async move {
                        tokio::select! { _ = child.cancelled() => {}, _ = listen(app, active, account) => {} }
                    }));
                }
                *desktop.notifications.status.lock().await = "Listening for new messages".into();
            } else {
                *desktop.notifications.status.lock().await = "Waiting for the hub".into();
            }
            check_at = tokio::time::Instant::now() + Duration::from_secs(15);
        } else if current.is_none() {
            *desktop.notifications.status.lock().await = if enabled {
                "Connect to a hub to receive notifications"
            } else {
                "Notifications off"
            }
            .into();
        }
        tokio::select! { _ = desktop.notifications.cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
    }
    cancel.cancel();
    for (_, task) in tasks {
        task.abort();
    }
}
async fn tail(active: &Active, account: &str) -> Result<u64> {
    let mut url = api(active, &["v1", "accounts", account, "threads"])?;
    url.query_pairs_mut().append_pair("limit", "1");
    let page = get(active, url).await?;
    Ok(field(&page, "asOfEventCursor")?.parse()?)
}
async fn listen(app: tauri::AppHandle, active: Arc<Active>, account: String) -> Result<()> {
    let server = active
        .connection
        .server_id
        .as_deref()
        .context("Missing hub identity")?;
    let path = app
        .state::<Desktop>()
        .config_file
        .parent()
        .context("Missing settings directory")?
        .join("notifications")
        .join(format!(
            "{:x}.json",
            Sha256::digest(serde_json::to_vec(&(server, &account))?)
        ));
    let mut ledger = Ledger::load(&path)?;
    let mut cursor = None;
    loop {
        // App startup and hub selection begin at the current event boundary.
        // Network reconnects within this listener resume the in-memory cursor.
        if cursor.is_none() {
            match tail(&active, &account).await {
                Ok(tail) => cursor = Some(tail),
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        }
        let mut url = api(&active, &["v1", "accounts", &account, "events", "stream"])?;
        url.query_pairs_mut()
            .append_pair("after", &cursor.unwrap().to_string());
        if let Ok(mut response) = response(&active, url).await {
            if response.status() == reqwest::StatusCode::GONE {
                cursor = None;
            } else if response.status().is_success() {
                let mut parser = Frames::default();
                loop {
                    let chunk =
                        tokio::time::timeout(Duration::from_secs(45), response.chunk()).await;
                    let Ok(Ok(Some(chunk))) = chunk else {
                        break;
                    };
                    let frames = match parser.push(&chunk) {
                        Ok(frames) => frames,
                        Err(_) => {
                            cursor = None;
                            break;
                        }
                    };
                    for frame in frames {
                        if frame.get("error").is_some() {
                            cursor = None;
                            break;
                        }
                        let next = field(&frame, "cursor")?.parse::<u64>()?;
                        if next <= cursor.unwrap_or(0) {
                            continue;
                        }
                        if live(&frame) {
                            let _ =
                                alert(&app, &active, &account, &frame, &mut ledger, &path).await;
                        }
                        cursor = Some(next);
                    }
                    if cursor.is_none() {
                        break;
                    }
                }
            }
        }
        // A transport failure keeps the last received cursor. Only a stream
        // reset response or malformed frame requires a fresh baseline.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
async fn alert(
    app: &tauri::AppHandle,
    active: &Arc<Active>,
    account: &str,
    event: &Value,
    ledger: &mut Ledger,
    path: &Path,
) -> Result<()> {
    let id = field(&event["data"], "messageId")?;
    let thread_id = field(&event["data"], "threadId")?;
    let status = get(active, api(active, &["v1", "accounts", account])?).await?;
    if status["authenticated"] != true || status["contactSyncReady"] != true {
        return Ok(());
    }
    let message_url = api(active, &["v1", "accounts", account, "messages", id])?;
    let mut message = None;
    for attempt in 0..3 {
        match get(active, message_url.clone()).await {
            Ok(value) => {
                message = Some(value);
                break;
            }
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    let message = message.context("Notification message unavailable")?;
    if message["id"] != id || message["accountId"] != account || message["threadId"] != thread_id {
        bail!("Notification message identity mismatch");
    }
    let Some(body) = preview(&message) else {
        return Ok(());
    };
    let thread = get(
        active,
        api(active, &["v1", "accounts", account, "threads", thread_id])?,
    )
    .await?;
    let target = Target {
        connection_id: active.connection.id.clone(),
        server_id: active
            .connection
            .server_id
            .clone()
            .context("Missing hub identity")?,
        account_id: account.into(),
        thread_id: thread_id.into(),
    };
    let desktop = app.state::<Desktop>();
    let inner = desktop.inner.lock().await;
    if !inner.config.notifications_enabled
        || inner
            .active
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(active, current))
    {
        return Ok(());
    }
    let window = app.get_webview_window("main");
    let reading = window.is_some_and(|window| {
        window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
    });
    if muted_or_read(
        &thread,
        desktop.notifications.focus.lock().await.as_ref(),
        &target,
        reading,
    ) {
        return Ok(());
    }
    if !ledger.claim(path, id, field(event, "cursor")?)? {
        return Ok(());
    }
    let title = thread["displayName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("New message");
    let title: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    #[cfg(target_os = "linux")]
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .extra("target", &target)
        .show()?;
    Ok(())
}
#[derive(Default)]
struct Frames {
    pending: Vec<u8>,
    data: Vec<u8>,
}
impl Frames {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        ensure!(
            self.pending.len() + chunk.len() <= 1024 * 1024,
            "Notification stream frame exceeds limit"
        );
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<_> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    frames.push(serde_json::from_slice(&self.data)?);
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                ensure!(
                    self.data.len() + data.len() < 1024 * 1024,
                    "Notification data exceeds limit"
                );
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(data);
            }
        }
        Ok(frames)
    }
}
#[tauri::command]
pub async fn notifications_context(
    state: State<'_, Desktop>,
    focus: Option<Focus>,
) -> Result<(), String> {
    if focus.as_ref().is_some_and(|focus| {
        focus.connection_id.len() > 64
            || focus.account_id.as_ref().is_some_and(|id| id.len() > 2048)
            || focus.thread_id.as_ref().is_some_and(|id| id.len() > 2048)
    }) {
        return Err("Invalid reading context".into());
    }
    *state.notifications.focus.lock().await = focus;
    Ok(())
}
#[tauri::command]
pub async fn notifications_take_target(
    state: State<'_, Desktop>,
) -> Result<Option<Target>, String> {
    let inner = state.inner.lock().await;
    Ok(state
        .notifications
        .pending
        .lock()
        .await
        .take()
        .filter(|target| {
            inner.active.as_ref().is_some_and(|active| {
                active.connection.id == target.connection_id
                    && active.connection.server_id.as_deref() == Some(target.server_id.as_str())
            })
        }))
}
#[tauri::command]
pub async fn notifications_enabled(state: State<'_, Desktop>, enabled: bool) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    let mut config = inner.config.clone();
    config.notifications_enabled = enabled;
    config
        .save(&state.config_file)
        .map_err(|_| "Cannot save notification settings")?;
    inner.config = config;
    Ok(())
}
pub async fn status(desktop: &Desktop) -> String {
    desktop.notifications.status.lock().await.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn stream_frames_survive_chunk_boundaries_and_comments() {
        let mut parser = Frames::default();
        assert!(parser
            .push(b": keepalive\r\ndata: {\"cursor\":\"")
            .unwrap()
            .is_empty());
        assert_eq!(
            parser.push(b"4\"}\r\n\r\n").unwrap(),
            vec![json!({"cursor":"4"})]
        );
        assert!(parser.push(&vec![b'x'; 1024 * 1024 + 1]).is_err());
    }
    #[test]
    fn unknown_mute_and_focused_chat_are_suppressed_but_hidden_chat_is_not() {
        let target = Target {
            connection_id: "hub".into(),
            server_id: "server".into(),
            account_id: "one".into(),
            thread_id: "room".into(),
        };
        let focus = Focus {
            connection_id: "hub".into(),
            account_id: Some("one".into()),
            thread_id: Some("room".into()),
        };
        for thread in [json!({}), json!({"muted":null}), json!({"muted":true})] {
            assert!(muted_or_read(&thread, None, &target, false));
        }
        assert!(muted_or_read(
            &json!({"muted":false}),
            Some(&focus),
            &target,
            true
        ));
        assert!(!muted_or_read(
            &json!({"muted":false}),
            Some(&focus),
            &target,
            false
        ));
    }
    #[test]
    fn previews_never_show_protocol_xml_recalled_content_or_outgoing_messages() {
        assert_eq!(
            preview(&json!({"type":49,"body":"<msg>private</msg>","direction":"incoming"})),
            Some("Attachment".into())
        );
        assert!(
            preview(&json!({"type":10002,"body":"<sysmsg/>","direction":"incoming"})).is_none()
        );
        assert!(
            preview(&json!({"type":1,"body":"secret","direction":"incoming","recall":{}}))
                .is_none()
        );
        assert!(preview(&json!({"type":1,"body":"secret","direction":"outgoing"})).is_none());
    }
    #[test]
    fn quoted_reply_previews_show_the_comment_instead_of_an_attachment_or_the_quote() {
        for quote in [
            Value::Null,
            json!({"embeddedText":"Original quoted message"}),
        ] {
            assert_eq!(
                preview(
                    &json!({"type":49,"appMessageType":57,"body":"Yes,\n that works",
                    "quote":quote,"direction":"incoming"})
                ),
                Some("Yes, that works".into())
            );
        }
        // Older hubs may include the quote without an application subtype.
        assert_eq!(
            preview(
                &json!({"type":49,"body":"My reply","quote":{"type":1},"direction":"incoming"})
            ),
            Some("My reply".into())
        );
        for body in [Value::Null, json!(""), json!(" \n ")] {
            assert_eq!(
                preview(&json!({"type":49,"appMessageType":57,"body":body,"direction":"incoming"})),
                Some("Reply".into())
            );
        }
        assert_eq!(
            preview(&json!({"type":49,"appMessageType":57,"body":"😀".repeat(200),"direction":"incoming"})).unwrap().chars().count(),
            180
        );
    }
    #[test]
    fn durable_claims_never_repeat() {
        let temp = wechat_platform::private_tempdir("notifications-").unwrap();
        let path = temp.path().join("state.json");
        let mut ledger = Ledger::default();
        assert!(ledger.claim(&path, "message", "7").unwrap());
        assert!(!Ledger::load(&path)
            .unwrap()
            .claim(&path, "message", "8")
            .unwrap());
        assert!(wechat_platform::private_path(&path).unwrap());
    }
    #[test]
    fn live_events_exclude_history_local_sends_and_old_replays() {
        let mut event = json!({"kind":"message.committed","data":{"origin":"live","direction":"incoming"},"createdAt":chrono::Utc::now().to_rfc3339()});
        assert!(live(&event));
        event["data"]["origin"] = json!("catchup");
        assert!(!live(&event));
        event["data"]["origin"] = json!("live");
        event["data"]["direction"] = json!("outgoing");
        assert!(!live(&event));
        event["data"]["direction"] = json!("incoming");
        event["createdAt"] = json!("2020-01-01T00:00:00Z");
        assert!(!live(&event));
    }
}
