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
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

struct PostedCallAlert {
    alert: crate::notification_sound::ActiveCallAlert,
    call_id: String,
    sounding: bool,
}

pub struct Notifications {
    pub cancel: CancellationToken,
    focus: Mutex<Option<Focus>>,
    pending: Mutex<Option<Target>>,
    pending_call: Mutex<Option<CallActionTarget>>,
    active_call_alerts: Mutex<HashMap<String, PostedCallAlert>>,
    sounding_call_ids: watch::Sender<HashMap<String, String>>,
    status: Mutex<String>,
    last_sound: Mutex<Option<Instant>>,
}

impl Default for Notifications {
    fn default() -> Self {
        let (sounding_call_ids, _) = watch::channel(HashMap::new());
        Self {
            cancel: CancellationToken::new(),
            focus: Mutex::new(None),
            pending: Mutex::new(None),
            pending_call: Mutex::new(None),
            active_call_alerts: Mutex::new(HashMap::new()),
            sounding_call_ids,
            status: Mutex::new(String::new()),
            last_sound: Mutex::new(None),
        }
    }
}

impl Notifications {
    /// Subscribes to the live map of `account_id -> call_id` for incoming calls
    /// currently playing a sounding OS notification alert.
    pub(crate) fn subscribe_sounding_calls(&self) -> watch::Receiver<HashMap<String, String>> {
        self.sounding_call_ids.subscribe()
    }
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Focus {
    connection_id: String,
    account_id: Option<String>,
    thread_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallActionTarget {
    pub connection_id: String,
    pub server_id: String,
    pub account_id: String,
    pub peer_id: String,
    pub call_id: String,
    pub revision: u32,
    pub call_type: String,
    pub action: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    connection_id: String,
    server_id: String,
    account_id: String,
    thread_id: String,
    /// The message that raised the alert, so the window can scroll to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}
#[cfg(target_os = "macos")]
impl Target {
    /// Groups one chat's alerts together in Notification Center.
    pub(crate) fn thread_key(&self) -> String {
        thread_key(&self.connection_id, &self.account_id, &self.thread_id)
    }
    /// Identifies the alert for one message, so a repeat replaces it.
    pub(crate) fn request_id(&self) -> String {
        format!(
            "{}|{}|{}",
            self.connection_id,
            self.account_id,
            self.message_id.as_deref().unwrap_or(&self.thread_id)
        )
    }
}
#[cfg(target_os = "macos")]
fn thread_key(connection: &str, account: &str, thread: &str) -> String {
    format!("{connection}|{account}|{thread}")
}
/// What a macOS message alert carries, so a click or reply still works after the app restarts.
#[cfg(target_os = "macos")]
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Payload {
    pub(crate) target: Target,
    /// WeChat's ID of the message, needed to quote it. Reply is offered only with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) server_id: Option<String>,
}
/// Union of macOS alert payloads; message payloads stay untagged for compatibility
/// with delivered alerts persisted across app restarts.
#[cfg(target_os = "macos")]
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MacPayload {
    Call(CallActionTarget),
    Message(Payload),
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
#[cfg(test)]
fn preview(message: &Value) -> Option<String> {
    preview_in(message, "en")
}
fn preview_in(message: &Value, language: &str) -> Option<String> {
    let label = |key| crate::i18n::text_in(language, key);
    if !message["recall"].is_null() || message["direction"] != "incoming" {
        return None;
    }
    let text = match message["type"].as_u64()? {
        1 => message["body"].as_str()?.to_owned(),
        3 => label("previewPhoto"),
        34 => label("previewVoiceMessage"),
        43 => label("previewVideo"),
        47 => label("previewSticker"),
        49 if message["appMessageType"] == 57 || message["quote"].is_object() => message["body"]
            .as_str()
            .filter(|body| !body.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| label("previewReply")),
        49 => match message["appMessageType"].as_u64() {
            Some(5) => label("previewSharedLink"),
            Some(6) => label("previewFile"),
            _ => label("previewAttachment"),
        },
        _ => return None,
    };
    Some(
        crate::emoji::emoji_text(&text)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(180)
            .collect(),
    )
}
fn group_message(thread: &Value, message: &Value) -> bool {
    thread["kind"] == "group"
        || message["threadId"]
            .as_str()
            .is_some_and(|id| id.ends_with("@chatroom"))
}
fn sender_label(value: &Value, sender: &str) -> Option<String> {
    let name: String = value
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    (!name.is_empty() && name != sender).then_some(name)
}
fn cached_sender(thread: &Value, message: &Value) -> Option<String> {
    let sender = field(message, "senderId").ok()?;
    let latest = &thread["lastMessage"];
    if latest["senderId"] == sender {
        if let Some(name) = sender_label(&latest["senderName"], sender) {
            return Some(name);
        }
    }
    thread["avatarMembers"]
        .as_array()?
        .iter()
        .find_map(|member| {
            (member["id"] == sender)
                .then(|| sender_label(&member["displayName"], sender))
                .flatten()
        })
}
async fn group_sender(
    active: &Active,
    account: &str,
    thread_id: &str,
    thread: &Value,
    message: &Value,
) -> Option<String> {
    if !group_message(thread, message) {
        return None;
    }
    if let Some(name) = cached_sender(thread, message) {
        return Some(name);
    }
    let sender = field(message, "senderId").ok()?;
    let url = api(
        active,
        &[
            "v1", "accounts", account, "groups", thread_id, "members", sender,
        ],
    )
    .ok()?;
    // Read only the hub's cached member. Missing names must not hold up an alert
    // or start a contact refresh, roster download or other WeChat request.
    let lookup = tokio::time::timeout(Duration::from_secs(1), get(active, url))
        .await
        .ok()?
        .ok()?;
    if lookup["groupId"] != thread_id
        || lookup["username"] != sender
        || lookup["member"]["username"] != sender
    {
        return None;
    }
    sender_label(&lookup["member"]["displayName"], sender)
        .or_else(|| sender_label(&lookup["member"]["roomDisplayName"], sender))
}
#[cfg(test)]
fn notification_heading(
    thread: &Value,
    message: &Value,
    sender: Option<&str>,
) -> (String, Option<String>) {
    notification_heading_in(thread, message, sender, "en")
}
fn notification_heading_in(
    thread: &Value,
    message: &Value,
    sender: Option<&str>,
    language: &str,
) -> (String, Option<String>) {
    let fallback = crate::i18n::text_in(language, "newMessage");
    let name: String = thread["displayName"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&fallback)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    if group_message(thread, message) {
        (
            sender.map_or_else(|| crate::i18n::text_in(language, "member"), str::to_owned),
            Some(name),
        )
    } else {
        (name, None)
    }
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
    // Installed while the app launches, so a click that launched it is delivered.
    #[cfg(target_os = "macos")]
    install_macos(&app);
    let clicked = app.clone();
    app.notification().on_action(move |action| {
        if action.action_id() != "tap" {
            return;
        }
        let Some(notification) = action.notification() else {
            return;
        };
        if let Some(call_target) = notification
            .extra()
            .get("callTarget")
            .and_then(|value| serde_json::from_value::<CallActionTarget>(value.clone()).ok())
        {
            open_call_action(clicked.clone(), call_target);
            return;
        }
        let Some(target) = notification
            .extra()
            .get("target")
            .and_then(|target| serde_json::from_value::<Target>(target.clone()).ok())
        else {
            return;
        };
        open_target(clicked.clone(), target);
    })?;
    tauri::async_runtime::spawn(coordinate(app));
    Ok(())
}
pub(crate) fn open_call_action(app: tauri::AppHandle, target: CallActionTarget) {
    tauri::async_runtime::spawn(async move {
        let desktop = app.state::<Desktop>();
        let active = desktop.inner.lock().await.active.clone();
        if active.is_some_and(|active| {
            active.connection.id != target.connection_id
                || active.connection.server_id.as_deref() != Some(target.server_id.as_str())
        }) {
            return;
        }
        clear_call_alert(&desktop, &target.account_id).await;
        *desktop.notifications.pending_call.lock().await = Some(target.clone());
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        let _ = app.emit("desktop-call-action", target);
    });
}
fn sync_sounding_calls(
    alerts: &HashMap<String, PostedCallAlert>,
    tx: &watch::Sender<HashMap<String, String>>,
) {
    let next: HashMap<String, String> = alerts
        .iter()
        .filter(|(_, entry)| entry.sounding)
        .map(|(account, entry)| (account.clone(), entry.call_id.clone()))
        .collect();
    let _ = tx.send_if_modified(|current| {
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
}
fn os_call_sound_permitted() -> bool {
    #[cfg(target_os = "macos")]
    if crate::macos_notifications::available()
        && crate::macos_notifications::permitted() == Some(false)
    {
        return false;
    }
    true
}
async fn store_call_alert_if_unfocused(
    notifications: &Notifications,
    account_id: String,
    call_id: String,
    sounding: bool,
    alert: crate::notification_sound::ActiveCallAlert,
    focused_now: impl FnOnce() -> bool,
) -> bool {
    let replaced = {
        let mut alerts = notifications.active_call_alerts.lock().await;
        if focused_now() {
            drop(alerts);
            alert.clear();
            return false;
        }
        let replaced = alerts.insert(
            account_id,
            PostedCallAlert {
                alert,
                call_id,
                sounding,
            },
        );
        sync_sounding_calls(&alerts, &notifications.sounding_call_ids);
        replaced
    };
    if let Some(old) = replaced {
        old.alert.clear();
    }
    true
}
async fn clear_call_alert(desktop: &Desktop, account_id: &str) {
    let removed = {
        let mut alerts = desktop.notifications.active_call_alerts.lock().await;
        let removed = alerts.remove(account_id);
        if removed.is_some() {
            sync_sounding_calls(&alerts, &desktop.notifications.sounding_call_ids);
        }
        removed
    };
    if let Some(entry) = removed {
        entry.alert.clear();
    }
}
pub(crate) async fn clear_all_call_alerts(desktop: &Desktop) {
    let alerts: Vec<_> = {
        let mut guard = desktop.notifications.active_call_alerts.lock().await;
        let drained: Vec<_> = guard.drain().map(|(_, entry)| entry.alert).collect();
        if !drained.is_empty() {
            sync_sounding_calls(&guard, &desktop.notifications.sounding_call_ids);
        }
        drained
    };
    for alert in alerts {
        alert.clear();
    }
}
fn is_window_focused(app: &tauri::AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
    })
}
fn should_post_call_alert(account_enabled: bool, window_focused: bool) -> bool {
    account_enabled && !window_focused
}
#[derive(Default)]
struct ConferenceTracker {
    seen: VecDeque<String>,
}
impl ConferenceTracker {
    fn seed(
        &mut self,
        invitations: &[wechat_call_contracts::call_types::CallConferenceInvitation],
    ) {
        for inv in invitations {
            let key = format!("{}:{}", inv.invitation_id, inv.revision);
            if !self.seen.iter().any(|item| item == &key) {
                self.seen.push_back(key);
                if self.seen.len() > 128 {
                    self.seen.pop_front();
                }
            }
        }
    }
    fn take_fresh<'a>(
        &mut self,
        invitations: &'a [wechat_call_contracts::call_types::CallConferenceInvitation],
    ) -> Vec<&'a wechat_call_contracts::call_types::CallConferenceInvitation> {
        let mut fresh = Vec::new();
        for inv in invitations {
            let key = format!("{}:{}", inv.invitation_id, inv.revision);
            if !self.seen.iter().any(|item| item == &key) {
                self.seen.push_back(key);
                if self.seen.len() > 128 {
                    self.seen.pop_front();
                }
                fresh.push(inv);
            }
        }
        fresh
    }
}
async fn cached_thread_info(
    active: &Active,
    account: &str,
    thread_id: &str,
) -> (Option<String>, Option<bool>) {
    let Ok(url) = api(active, &["v1", "accounts", account, "threads", thread_id]) else {
        return (None, None);
    };
    let Ok(Ok(thread)) = tokio::time::timeout(Duration::from_secs(1), get(active, url)).await
    else {
        return (None, None);
    };
    let name = thread["displayName"]
        .as_str()
        .map(|name| {
            name.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .filter(|c| !c.is_control())
                .take(100)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty());
    (name, thread["muted"].as_bool())
}
async fn cached_thread_name(active: &Active, account: &str, thread_id: &str) -> Option<String> {
    cached_thread_info(active, account, thread_id).await.0
}
fn has_any_call_capability(caps: &wechat_call_contracts::call_types::CallCapabilities) -> bool {
    caps.direct_audio
        || caps.direct_video
        || caps.conference_create
        || caps.conference_join
        || caps.conference_invite
        || caps.conference_video
}
const NO_CAPABILITY_INITIAL_BACKOFF: Duration = Duration::from_secs(15);
const NO_CAPABILITY_MAX_BACKOFF: Duration = Duration::from_secs(60);

fn call_observer_reject_backoff(status: u16) -> Duration {
    match status {
        401 | 403 => Duration::from_secs(30),
        404 | 501 => Duration::from_secs(60),
        _ => Duration::from_secs(5),
    }
}
fn next_no_capability_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(NO_CAPABILITY_MAX_BACKOFF)
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct IncomingCallSummary {
    call_id: String,
    revision: u32,
    call_type: wechat_call_contracts::call_types::CallType,
    caller_id: String,
}
impl From<&wechat_call_contracts::call_types::CallOffer> for IncomingCallSummary {
    fn from(offer: &wechat_call_contracts::call_types::CallOffer) -> Self {
        Self {
            call_id: offer.call_id.clone(),
            revision: offer.revision,
            call_type: offer.call_type,
            caller_id: offer.caller_id.clone(),
        }
    }
}
impl From<&wechat_call_contracts::call_types::CallPendingOffer> for IncomingCallSummary {
    fn from(offer: &wechat_call_contracts::call_types::CallPendingOffer) -> Self {
        Self {
            call_id: offer.call_id.clone(),
            revision: offer.revision,
            call_type: offer.call_type,
            caller_id: offer.caller_id.clone(),
        }
    }
}
/// Passive per-account `/calls/socket` observer for OS incoming-call and group-call
/// notifications. Strictly passive: NEVER sends `call.ready`.
async fn listen_calls(app: tauri::AppHandle, active: Arc<Active>, account: String) {
    use wechat_call_client::transport::{CallClientError, CallEndpoint, ControlClient};
    use wechat_call_contracts::call_types::{CallEvent, CallType};

    let Ok(base) = crate::config::endpoint(&active.connection.endpoint) else {
        return;
    };
    let Some(server_id) = active.connection.server_id.clone() else {
        return;
    };
    let endpoint = CallEndpoint {
        http: active.http.clone(),
        base,
        token: active.key.as_str().to_owned(),
        account: account.clone(),
    };

    let mut conferences = ConferenceTracker::default();
    let mut posted_offer_call_id: Option<String> = None;
    let mut no_cap_backoff = NO_CAPABILITY_INITIAL_BACKOFF;

    loop {
        let connect_result = ControlClient::connect(&endpoint).await;
        let (mut control, hello) = match connect_result {
            Ok(pair) => pair,
            Err(error) => {
                let delay = error
                    .downcast_ref::<CallClientError>()
                    .and_then(|err| match err {
                        CallClientError::SocketRejected { status } => {
                            Some(call_observer_reject_backoff(*status))
                        }
                        _ => None,
                    })
                    .unwrap_or(Duration::from_secs(5));
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        if !has_any_call_capability(&control.capabilities) {
            // The daemon accepts the control socket even before an account's first
            // sync binds call authority (returning all-false capabilities). Retry
            // with bounded backoff instead of parking forever.
            control.close().await;
            let delay = no_cap_backoff;
            no_cap_backoff = next_no_capability_backoff(no_cap_backoff);
            tokio::time::sleep(delay).await;
            continue;
        }
        no_cap_backoff = NO_CAPABILITY_INITIAL_BACKOFF;
        // Seed existing conference invitations on connect so reconnects do not re-alert.
        conferences.seed(&control.conference_invitations);

        let handle_offer = |offer: IncomingCallSummary,
                            posted: &mut Option<String>,
                            app: &tauri::AppHandle,
                            active: &Arc<Active>,
                            account: &str,
                            server_id: &str| {
            let should_send = posted.as_deref() != Some(offer.call_id.as_str());
            let app = app.clone();
            let active = active.clone();
            let account = account.to_owned();
            let server_id = server_id.to_owned();
            async move {
                if !should_send {
                    return true;
                }
                let desktop = app.state::<Desktop>();
                let (enabled, sound_enabled) = {
                    let inner = desktop.inner.lock().await;
                    let same_active = inner
                        .active
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(&active, current));
                    (
                        same_active && inner.config.notifications_for(&server_id, &account),
                        inner.config.notification_sound_enabled,
                    )
                };
                if !should_post_call_alert(enabled, is_window_focused(&app)) {
                    return false;
                }
                let caller = cached_thread_name(&active, &account, &offer.caller_id)
                    .await
                    .unwrap_or_else(|| {
                        if offer.caller_id.is_empty() {
                            crate::i18n::text("callCallerFallback")
                        } else {
                            offer.caller_id.clone()
                        }
                    });
                // Re-check focus after the thread lookup so focusing during the
                // lookup does not flash an OS banner or play an overlapping chime.
                if is_window_focused(&app) {
                    return false;
                }
                let body = match offer.call_type {
                    CallType::Video => crate::i18n::text("callIncomingVideo"),
                    CallType::Voice => crate::i18n::text("callIncomingVoice"),
                };
                let request_id = format!(
                    "call|{}|{}|{}|{}",
                    active.connection.id, account, offer.call_id, offer.revision
                );
                let target = CallActionTarget {
                    connection_id: active.connection.id.clone(),
                    server_id,
                    account_id: account.clone(),
                    peer_id: offer.caller_id.clone(),
                    call_id: offer.call_id.clone(),
                    revision: offer.revision,
                    call_type: match offer.call_type {
                        CallType::Video => "video".into(),
                        CallType::Voice => "voice".into(),
                    },
                    action: "open".into(),
                };
                clear_call_alert(&desktop, &account).await;
                if let Ok(alert) = crate::notification_sound::show_call(
                    &app,
                    &request_id,
                    &caller,
                    &body,
                    target,
                    sound_enabled,
                )
                .await
                {
                    let sounding = sound_enabled && os_call_sound_permitted();
                    return store_call_alert_if_unfocused(
                        &desktop.notifications,
                        account,
                        offer.call_id,
                        sounding,
                        alert,
                        || is_window_focused(&app),
                    )
                    .await;
                }
                false
            }
        };

        if let CallEvent::Hello {
            current_offer,
            current_pending_offer,
            ..
        } = &hello
        {
            if let Some(summary) = current_offer
                .as_ref()
                .map(IncomingCallSummary::from)
                .or_else(|| {
                    current_pending_offer
                        .as_ref()
                        .map(IncomingCallSummary::from)
                })
            {
                let call_id = summary.call_id.clone();
                if handle_offer(
                    summary,
                    &mut posted_offer_call_id,
                    &app,
                    &active,
                    &account,
                    &server_id,
                )
                .await
                {
                    posted_offer_call_id = Some(call_id);
                }
            }
        }

        loop {
            match control.next_event().await {
                Ok(event) => {
                    control.update(&event);
                    match &event {
                        CallEvent::Offered { offer } => {
                            let call_id = offer.call_id.clone();
                            if handle_offer(
                                IncomingCallSummary::from(offer),
                                &mut posted_offer_call_id,
                                &app,
                                &active,
                                &account,
                                &server_id,
                            )
                            .await
                            {
                                posted_offer_call_id = Some(call_id);
                            }
                        }
                        CallEvent::Pending {
                            offer: Some(pending),
                        } => {
                            let call_id = pending.call_id.clone();
                            if handle_offer(
                                IncomingCallSummary::from(pending),
                                &mut posted_offer_call_id,
                                &app,
                                &active,
                                &account,
                                &server_id,
                            )
                            .await
                            {
                                posted_offer_call_id = Some(call_id);
                            }
                        }
                        CallEvent::Pending { offer: None } => {
                            if control.offer.is_none() && posted_offer_call_id.take().is_some() {
                                clear_call_alert(&app.state::<Desktop>(), &account).await;
                            }
                        }
                        CallEvent::Availability { availability } => {
                            if !availability.busy && posted_offer_call_id.take().is_some() {
                                clear_call_alert(&app.state::<Desktop>(), &account).await;
                            }
                        }
                        CallEvent::State { call } => {
                            if posted_offer_call_id.as_deref() == Some(call.call_id.as_str()) {
                                posted_offer_call_id = None;
                                clear_call_alert(&app.state::<Desktop>(), &account).await;
                            }
                        }
                        CallEvent::ConferenceInvitations { invitations } => {
                            let fresh = conferences.take_fresh(invitations);
                            for inv in fresh {
                                let desktop = app.state::<Desktop>();
                                let (enabled, sound_enabled) = {
                                    let inner = desktop.inner.lock().await;
                                    let same_active = inner
                                        .active
                                        .as_ref()
                                        .is_some_and(|current| Arc::ptr_eq(&active, current));
                                    (
                                        same_active
                                            && inner.config.notifications_for(&server_id, &account),
                                        inner.config.notification_sound_enabled,
                                    )
                                };
                                if !should_post_call_alert(enabled, is_window_focused(&app)) {
                                    continue;
                                }
                                let (cached_group_name, group_muted) =
                                    cached_thread_info(&active, &account, &inv.group_id).await;
                                if group_muted != Some(false) {
                                    continue;
                                }
                                let group_name = cached_group_name
                                    .unwrap_or_else(|| crate::i18n::text("callConferenceActive"));
                                let caller_name =
                                    cached_thread_name(&active, &account, &inv.caller_id).await;
                                let body = match caller_name {
                                    Some(caller) => crate::i18n::text_with(
                                        "callConferenceBody",
                                        &[("caller", &caller)],
                                    ),
                                    None => crate::i18n::text("callConferenceBodyUnnamed"),
                                };
                                let sound = sound_due(
                                    &mut *desktop.notifications.last_sound.lock().await,
                                    sound_enabled,
                                    Instant::now(),
                                );
                                #[cfg(target_os = "linux")]
                                let body = body
                                    .replace('&', "&amp;")
                                    .replace('<', "&lt;")
                                    .replace('>', "&gt;");
                                let target = Target {
                                    connection_id: active.connection.id.clone(),
                                    server_id: server_id.clone(),
                                    account_id: account.clone(),
                                    thread_id: inv.group_id.clone(),
                                    message_id: None,
                                };
                                let _ = crate::notification_sound::show(
                                    &app,
                                    &group_name,
                                    None,
                                    &body,
                                    Some(target),
                                    None,
                                    sound,
                                )
                                .await;
                            }
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    if posted_offer_call_id.take().is_some() {
                        clear_call_alert(&app.state::<Desktop>(), &account).await;
                    }
                    break;
                }
            }
        }
        control.close().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
pub(crate) fn open_target(app: tauri::AppHandle, target: Target) {
    tauri::async_runtime::spawn(async move {
        let desktop = app.state::<Desktop>();
        let active = desktop.inner.lock().await.active.clone();
        // A click can arrive before the hub is connected, such as one that launched the
        // app. Keep its target pending: the window takes it once that hub is connected.
        if active.is_some_and(|active| {
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
}
async fn coordinate(app: tauri::AppHandle) {
    let mut current: Option<Arc<Active>> = None;
    let mut cancel = CancellationToken::new();
    let mut tasks: HashMap<String, tauri::async_runtime::JoinHandle<()>> = HashMap::new();
    let mut call_tasks: HashMap<String, tauri::async_runtime::JoinHandle<()>> = HashMap::new();
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
            for (_, task) in call_tasks.drain() {
                task.abort();
            }
            clear_all_call_alerts(&desktop).await;
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
                let mut removed_call_accounts = Vec::new();
                call_tasks.retain(|id, task| {
                    if !accounts.contains(id) {
                        task.abort();
                        removed_call_accounts.push(id.clone());
                        false
                    } else {
                        !task.inner().is_finished()
                    }
                });
                for id in removed_call_accounts {
                    clear_call_alert(&desktop, &id).await;
                }
                for account in accounts {
                    if !tasks.contains_key(&account) {
                        let active = active.clone();
                        let app = app.clone();
                        let child = cancel.child_token();
                        let id = account.clone();
                        let acct = account.clone();
                        tasks.insert(
                            id,
                            tauri::async_runtime::spawn(async move {
                                tokio::select! { _ = child.cancelled() => {}, _ = listen(app, active, acct) => {} }
                            }),
                        );
                    }
                    if !call_tasks.contains_key(&account) {
                        let active = active.clone();
                        let app = app.clone();
                        let child = cancel.child_token();
                        let id = account.clone();
                        call_tasks.insert(
                            id,
                            tauri::async_runtime::spawn(async move {
                                tokio::select! { _ = child.cancelled() => {}, _ = listen_calls(app, active, account) => {} }
                            }),
                        );
                    }
                }
                *desktop.notifications.status.lock().await = listening_status();
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
    for (_, task) in call_tasks {
        task.abort();
    }
    clear_all_call_alerts(&app.state::<Desktop>()).await;
}
/// The status shown while listening, which on macOS also reports alerts that cannot
/// be shown.
fn listening_status() -> String {
    #[cfg(target_os = "macos")]
    if crate::macos_notifications::available() {
        if crate::macos_notifications::permitted() == Some(false) {
            return "Allow AgentWechat in System Settings > Notifications to see new messages"
                .into();
        }
        if let Some(error) = crate::macos_notifications::last_error() {
            return format!("Listening for new messages; the last alert failed: {error}");
        }
    }
    "Listening for new messages".into()
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
    let language = crate::i18n::language();
    let Some(body) = preview_in(&message, language) else {
        return Ok(());
    };
    let thread = get(
        active,
        api(active, &["v1", "accounts", account, "threads", thread_id])?,
    )
    .await?;
    if thread["muted"].as_bool() != Some(false) {
        return Ok(());
    }
    let sender = group_sender(active, account, thread_id, &thread, &message).await;
    let (title, subtitle) = notification_heading_in(&thread, &message, sender.as_deref(), language);
    let target = Target {
        connection_id: active.connection.id.clone(),
        server_id: active
            .connection
            .server_id
            .clone()
            .context("Missing hub identity")?,
        account_id: account.into(),
        thread_id: thread_id.into(),
        message_id: Some(id.into()),
    };
    let desktop = app.state::<Desktop>();
    let inner = desktop.inner.lock().await;
    if !inner.config.notifications_for(&target.server_id, account)
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
    #[cfg(target_os = "linux")]
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let enabled = inner.config.notification_sound_enabled;
    let sound = sound_due(
        &mut *desktop.notifications.last_sound.lock().await,
        enabled,
        Instant::now(),
    );
    drop(inner);
    let server_id = message["serverId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    crate::notification_sound::show(
        app,
        &title,
        subtitle.as_deref(),
        &body,
        Some(target),
        server_id,
        sound,
    )
    .await?;
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
    #[cfg(target_os = "macos")]
    if let Some(Focus {
        connection_id,
        account_id: Some(account),
        thread_id: Some(thread),
    }) = &focus
    {
        if crate::macos_notifications::available() {
            crate::macos_notifications::clear_thread(&thread_key(connection_id, account, thread));
        }
    }
    *state.notifications.focus.lock().await = focus.clone();
    if let Some(Focus {
        account_id: Some(account),
        ..
    }) = focus
    {
        clear_call_alert(&state, &account).await;
    }
    Ok(())
}
#[tauri::command]
pub async fn notifications_take_call_action(
    state: State<'_, Desktop>,
    connection_id: Option<String>,
) -> Result<Option<CallActionTarget>, String> {
    let inner = state.inner.lock().await;
    let mut pending = state.notifications.pending_call.lock().await;
    if let (Some(expected), Some(target)) = (connection_id.as_deref(), pending.as_ref()) {
        if target.connection_id != expected {
            return Ok(None);
        }
    }
    Ok(pending.take().filter(|target| {
        inner.active.as_ref().is_some_and(|active| {
            active.connection.id == target.connection_id
                && active.connection.server_id.as_deref() == Some(target.server_id.as_str())
        })
    }))
}
#[tauri::command]
pub async fn notifications_take_target(
    state: State<'_, Desktop>,
    connection_id: Option<String>,
) -> Result<Option<Target>, String> {
    let inner = state.inner.lock().await;
    let mut pending = state.notifications.pending.lock().await;
    // A caller that names its connection leaves another connection's target pending
    // instead of taking and discarding it.
    if let (Some(expected), Some(target)) = (connection_id.as_deref(), pending.as_ref()) {
        if target.connection_id != expected {
            return Ok(None);
        }
    }
    Ok(pending.take().filter(|target| {
        inner.active.as_ref().is_some_and(|active| {
            active.connection.id == target.connection_id
                && active.connection.server_id.as_deref() == Some(target.server_id.as_str())
        })
    }))
}
#[tauri::command]
pub async fn notifications_account_settings(
    state: State<'_, Desktop>,
    connection_id: String,
    server_id: String,
    account_id: String,
    enabled: Option<bool>,
) -> Result<Value, String> {
    let mut inner = state.inner.lock().await;
    if inner.active.as_ref().is_none_or(|active| {
        active.connection.id != connection_id
            || active.connection.server_id.as_deref() != Some(server_id.as_str())
    }) {
        return Err("Hub connection changed; reopen account settings".into());
    }
    if account_id.is_empty()
        || account_id.len() > 128
        || !account_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        return Err("Invalid account identity".into());
    }
    if let Some(enabled) = enabled {
        let mut config = inner.config.clone();
        config
            .set_account_notifications(&server_id, &account_id, enabled)
            .map_err(|error| error.to_string())?;
        config
            .save(&state.config_file)
            .map_err(|_| "Cannot save account notification settings")?;
        inner.config = config;
    }
    Ok(
        serde_json::json!({ "enabled": inner.config.account_notifications_enabled(&server_id, &account_id), "desktopEnabled": inner.config.notifications_enabled }),
    )
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
#[tauri::command]
pub async fn notifications_sound_enabled(
    state: State<'_, Desktop>,
    enabled: bool,
) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    let mut config = inner.config.clone();
    config.notification_sound_enabled = enabled;
    config
        .save(&state.config_file)
        .map_err(|_| "Cannot save notification sound settings")?;
    inner.config = config;
    Ok(())
}
fn sound_due(last: &mut Option<Instant>, enabled: bool, now: Instant) -> bool {
    if !enabled
        || last.is_some_and(|previous| {
            now.saturating_duration_since(previous) < Duration::from_secs(3)
        })
    {
        return false;
    }
    *last = Some(now);
    true
}
pub async fn status(desktop: &Desktop) -> String {
    desktop.notifications.status.lock().await.clone()
}

#[cfg(target_os = "macos")]
impl Payload {
    /// Whether the message can be quoted, which a reply needs.
    pub(crate) fn quotable(&self) -> bool {
        self.server_id.is_some() && self.target.message_id.is_some()
    }
}
/// Routes clicks and actions on macOS alerts to the window and the hub.
#[cfg(target_os = "macos")]
fn install_macos(app: &tauri::AppHandle) {
    use crate::macos_notifications::{self as mac, Action};
    if !mac::available() {
        return;
    }
    let app = app.clone();
    mac::install(
        move |mac::Response {
                  request_id,
                  action,
                  payload,
              }| {
            let Some(payload) = payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<MacPayload>(payload).ok())
            else {
                return;
            };
            let app = app.clone();
            match (payload, action) {
                (MacPayload::Call(mut call_target), Action::Open) => {
                    call_target.action = "open".into();
                    open_call_action(app, call_target);
                }
                (MacPayload::Call(mut call_target), Action::CallAnswer) => {
                    call_target.action = "answer".into();
                    open_call_action(app, call_target);
                }
                (MacPayload::Call(call_target), Action::CallDecline) => {
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = decline_call(&app, &call_target).await {
                            let fallback = Payload {
                                target: Target {
                                    connection_id: call_target.connection_id,
                                    server_id: call_target.server_id,
                                    account_id: call_target.account_id,
                                    thread_id: call_target.peer_id,
                                    message_id: None,
                                },
                                server_id: None,
                            };
                            action_failed(&request_id, &fallback, "callDeclineFailed", &error);
                        }
                    });
                }
                (MacPayload::Call(_), _) => {}
                (MacPayload::Message(payload), Action::Open) => open_target(app, payload.target),
                (MacPayload::Message(payload), Action::Reply(text)) => {
                    tauri::async_runtime::spawn(async move {
                        match send_reply(&app, &request_id, &payload, &text).await {
                            Err(error) => {
                                action_failed(&request_id, &payload, "replyNotSent", &error)
                            }
                            // Replying means the chat was read, as when replying on a phone.
                            Ok(true) => {
                                if let Err(error) = mark_read(&app, &payload).await {
                                    action_failed(
                                        &request_id,
                                        &payload,
                                        "repliedNotMarkedRead",
                                        &error,
                                    );
                                }
                            }
                            Ok(false) => {}
                        }
                    });
                }
                (MacPayload::Message(payload), Action::MarkRead) => {
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = mark_read(&app, &payload).await {
                            action_failed(&request_id, &payload, "notMarkedRead", &error);
                        }
                    });
                }
                (MacPayload::Message(_), _) => {}
            }
        },
    );
}
/// Declines an incoming call in the background from a macOS notification without
/// bringing the app forward.
///
/// If the call is still `Pending` (for example on a background signed-in account
/// whose UI controller paused `call.ready`), promotes only that single offer via
/// `CallReadyScope::Offer` before sending `call.reject`, and keeps the control
/// socket open until the daemon emits a terminal state so admission is never
/// revoked mid-RPC.
#[cfg(target_os = "macos")]
async fn decline_call(app: &tauri::AppHandle, target: &CallActionTarget) -> Result<()> {
    use wechat_call_client::transport::{CallEndpoint, ControlClient};
    use wechat_call_contracts::call_types::{CallCommand, CallEvent, CallReadyScope, CallState};

    let dummy_target = Target {
        connection_id: target.connection_id.clone(),
        server_id: target.server_id.clone(),
        account_id: target.account_id.clone(),
        thread_id: target.peer_id.clone(),
        message_id: None,
    };
    let active = connected(app, &dummy_target).await?;
    let endpoint = CallEndpoint {
        http: active.http.clone(),
        base: crate::config::endpoint(&active.connection.endpoint)?,
        token: active.key.as_str().to_owned(),
        account: target.account_id.clone(),
    };
    let (mut control, hello) = ControlClient::connect(&endpoint).await?;
    let mut reject_revision = target.revision;
    if let CallEvent::Hello {
        current_offer,
        current_pending_offer,
        ..
    } = &hello
    {
        if let Some(offer) = current_offer
            .as_ref()
            .filter(|offer| offer.call_id == target.call_id)
        {
            reject_revision = offer.revision;
        } else if let Some(pending) = current_pending_offer
            .as_ref()
            .filter(|offer| offer.call_id == target.call_id)
        {
            let _ = control
                .command(CallCommand::Ready {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    ready: true,
                    format: control.format,
                    scope: Some(CallReadyScope::Offer {
                        call_id: target.call_id.clone(),
                        expected_revision: pending.revision,
                    }),
                    video: None,
                })
                .await;
            if let Some(offer) = control
                .offer
                .as_ref()
                .filter(|offer| offer.call_id == target.call_id)
            {
                reject_revision = offer.revision;
            } else {
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    while let Ok(event) = control.next_event().await {
                        control.update(&event);
                        match &event {
                            CallEvent::Offered { offer } if offer.call_id == target.call_id => {
                                reject_revision = offer.revision;
                                break;
                            }
                            CallEvent::Pending { offer: None } => break,
                            CallEvent::Availability { availability } if !availability.busy => break,
                            _ => {}
                        }
                    }
                })
                .await;
            }
        }
    }
    let result = control
        .command(CallCommand::Reject {
            request_id: uuid::Uuid::new_v4().to_string(),
            call_id: target.call_id.clone(),
            expected_revision: reject_revision,
        })
        .await;
    if result.is_ok() {
        let already_terminal = control.call.as_ref().is_some_and(|call| {
            call.call_id == target.call_id
                && matches!(call.state, CallState::Ended | CallState::Uncertain)
        }) || !control.availability.busy;
        if !already_terminal {
            let _ = tokio::time::timeout(Duration::from_secs(15), async {
                while let Ok(event) = control.next_event().await {
                    control.update(&event);
                    match &event {
                        CallEvent::State { call }
                            if call.call_id == target.call_id
                                && matches!(
                                    call.state,
                                    CallState::Ended | CallState::Uncertain
                                ) =>
                        {
                            break;
                        }
                        CallEvent::Availability { availability } if !availability.busy => {
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await;
        }
    }
    control.close().await;
    clear_call_alert(&app.state::<Desktop>(), &target.account_id).await;
    result.map(|_| ())
}
/// The idempotency key and body of a quoted reply, or `None` when there is nothing to
/// send or the message cannot be quoted. The key is derived from the alert, so a
/// retried request sends at most once.
#[cfg(target_os = "macos")]
fn reply_request(request_id: &str, payload: &Payload, text: &str) -> Option<(String, Value)> {
    let text = text.trim();
    let server_id = payload.server_id.as_deref()?;
    let message_id = payload.target.message_id.as_deref()?;
    if text.is_empty() {
        return None;
    }
    let key = format!("desktop-reply-{:x}", Sha256::digest(request_id.as_bytes()));
    let body = serde_json::json!({
        "threadId": payload.target.thread_id,
        "text": text,
        "quote": { "messageId": message_id, "serverId": server_id },
    });
    Some((key, body))
}
/// Sends a reply typed into an alert as a quote of the alert's message. Returns
/// whether anything was sent.
#[cfg(target_os = "macos")]
async fn send_reply(
    app: &tauri::AppHandle,
    request_id: &str,
    payload: &Payload,
    text: &str,
) -> Result<bool> {
    let Some((key, body)) = reply_request(request_id, payload, text) else {
        return Ok(false);
    };
    let active = connected(app, &payload.target).await?;
    let url = api(
        &active,
        &["v1", "accounts", &payload.target.account_id, "sends"],
    )?;
    if let Some(reason) = post(&active, url, &body, Some(&key)).await? {
        bail!("The hub refused the reply ({reason})");
    }
    Ok(true)
}
/// Whether an incoming message newer than `message_id` is on a newest-first page
/// of a chat's messages: `None` when the page does not reach `message_id`.
#[cfg(target_os = "macos")]
fn newer_incoming(page: &Value, message_id: &str) -> Option<bool> {
    let mut newer = false;
    for message in page["items"].as_array()? {
        if message["id"] == message_id {
            return Some(newer);
        }
        newer |= message["direction"] == "incoming";
    }
    None
}
/// Marks the alert's chat read and clears its alerts, unless an incoming message newer
/// than the alerted one has arrived: the user has not seen it, so the chat stays
/// unread. Returns whether the chat was marked read.
#[cfg(target_os = "macos")]
async fn mark_read(app: &tauri::AppHandle, payload: &Payload) -> Result<bool> {
    let target = &payload.target;
    let Some(message_id) = target.message_id.as_deref() else {
        return Ok(false);
    };
    let active = connected(app, target).await?;
    let thread = [
        "v1",
        "accounts",
        &target.account_id,
        "threads",
        &target.thread_id,
    ];
    // The read-state version changes with every change to the profile, so it is read
    // just before the request; a change in between is a conflict, checked once more.
    for _ in 0..2 {
        let mut url = api(&active, &[&thread[..], &["messages"]].concat())?;
        url.query_pairs_mut().append_pair("limit", "50");
        if newer_incoming(&get(&active, url).await?, message_id) != Some(false) {
            return Ok(false);
        }
        let current = get(&active, api(&active, &thread)?).await?;
        let body = serde_json::json!({ "expectedVersion": field(&current, "readStateVersion")? });
        let url = api(&active, &[&thread[..], &["read-state"]].concat())?;
        match post(&active, url, &body, None).await? {
            None => {
                crate::macos_notifications::clear_thread(&target.thread_key());
                return Ok(true);
            }
            Some(reason) if reason == "read_state_conflict" => continue,
            Some(reason) => bail!("The hub refused it ({reason})"),
        }
    }
    Ok(false)
}
/// Posts JSON to the hub: `Ok(None)` when it accepted the request, `Ok(Some(code))`
/// with the hub's error code when it refused it.
#[cfg(target_os = "macos")]
async fn post(
    active: &Active,
    url: reqwest::Url,
    body: &Value,
    idempotency_key: Option<&str>,
) -> Result<Option<String>> {
    let mut request = active
        .http
        .post(url)
        .bearer_auth(active.key.as_str())
        .json(body);
    if let Some(key) = idempotency_key {
        request = request.header("Idempotency-Key", key);
    }
    let response = tokio::time::timeout(Duration::from_secs(15), request.send())
        .await
        .context("The hub did not answer in time")?
        .context("Cannot reach the hub")?;
    let status = response.status();
    if status.is_success() {
        return Ok(None);
    }
    let reason = response
        .json::<Value>()
        .await
        .ok()
        .and_then(|error| {
            error["error"]["code"]
                .as_str()
                .or(error["error"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| status.to_string());
    Ok(Some(reason))
}
/// The connection to the alert's hub, waiting briefly while the app is still
/// connecting, such as when the action launched it.
#[cfg(target_os = "macos")]
async fn connected(app: &tauri::AppHandle, target: &Target) -> Result<Arc<Active>> {
    for _ in 0..40 {
        let active = app.state::<Desktop>().inner.lock().await.active.clone();
        match active {
            Some(active)
                if active.connection.id == target.connection_id
                    && active.connection.server_id.as_deref()
                        == Some(target.server_id.as_str()) =>
            {
                return Ok(active)
            }
            Some(_) => bail!("AgentWechat is connected to a different hub"),
            None => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    bail!("AgentWechat is not connected to the hub")
}
/// Tells the user an action on an alert failed. Clicking it opens the chat.
#[cfg(target_os = "macos")]
/// Posts a failure alert; `title` is a `desktopShell.*` text key.
fn action_failed(request_id: &str, payload: &Payload, title: &str, error: &anyhow::Error) {
    let title = crate::i18n::text(title);
    let failed = Payload {
        target: payload.target.clone(),
        server_id: None,
    };
    let Ok(json) = serde_json::to_string(&failed) else {
        return;
    };
    crate::macos_notifications::post(&crate::macos_notifications::Alert {
        request_id: &format!("{request_id}|failed"),
        thread_key: &payload.target.thread_key(),
        title: &title,
        subtitle: None,
        body: &crate::i18n::text_with("actionFailedBody", &[("error", &error.to_string())]),
        sound: None,
        payload: &json,
        category: crate::macos_notifications::AlertCategory::Message,
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn sound_is_optional_and_bursts_do_not_repeat_it() {
        let now = Instant::now();
        let mut last = None;
        assert!(!sound_due(&mut last, false, now));
        assert!(last.is_none());
        assert!(sound_due(&mut last, true, now));
        assert!(!sound_due(
            &mut last,
            true,
            now + Duration::from_millis(2999)
        ));
        assert!(!sound_due(&mut last, false, now + Duration::from_secs(3)));
        assert!(sound_due(&mut last, true, now + Duration::from_secs(3)));
    }
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
            message_id: None,
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
    fn call_alerts_are_suppressed_when_window_is_focused_or_account_muted() {
        assert!(!should_post_call_alert(true, true));
        assert!(!should_post_call_alert(false, false));
        assert!(!should_post_call_alert(false, true));
        assert!(should_post_call_alert(true, false));
    }
    #[test]
    fn call_observer_backoff_retries_with_bounded_delay() {
        assert_eq!(call_observer_reject_backoff(401), Duration::from_secs(30));
        assert_eq!(call_observer_reject_backoff(403), Duration::from_secs(30));
        assert_eq!(call_observer_reject_backoff(404), Duration::from_secs(60));
        assert_eq!(call_observer_reject_backoff(501), Duration::from_secs(60));
        assert_eq!(call_observer_reject_backoff(503), Duration::from_secs(5));
        assert_eq!(
            next_no_capability_backoff(Duration::from_secs(15)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_no_capability_backoff(Duration::from_secs(30)),
            Duration::from_secs(60)
        );
        assert_eq!(
            next_no_capability_backoff(Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }
    #[tokio::test]
    async fn sounding_call_alerts_sync_watch_channel_and_clear_on_focus_race() {
        let notifications = Notifications::default();
        let rx = notifications.subscribe_sounding_calls();
        assert!(rx.borrow().is_empty());

        // Focus gained before storing rejects the alert and never publishes a sounding ID.
        let stored = store_call_alert_if_unfocused(
            &notifications,
            "acct-1".into(),
            "call-1".into(),
            true,
            crate::notification_sound::ActiveCallAlert::dummy_for_test(),
            || true,
        )
        .await;
        assert!(!stored);
        assert!(rx.borrow().is_empty());

        // Unfocused silent alert is stored but must not suppress the in-app ringtone.
        let stored = store_call_alert_if_unfocused(
            &notifications,
            "acct-1".into(),
            "call-1".into(),
            false,
            crate::notification_sound::ActiveCallAlert::dummy_for_test(),
            || false,
        )
        .await;
        assert!(stored);
        assert!(rx.borrow().is_empty());

        // Unfocused sounding alert publishes the call ID via the watch channel.
        let stored = store_call_alert_if_unfocused(
            &notifications,
            "acct-1".into(),
            "call-2".into(),
            true,
            crate::notification_sound::ActiveCallAlert::dummy_for_test(),
            || false,
        )
        .await;
        assert!(stored);
        assert_eq!(
            rx.borrow().get("acct-1").map(String::as_str),
            Some("call-2")
        );
    }
    #[test]
    fn conference_tracker_seeds_initial_invitations_and_deduplicates_by_revision() {
        use wechat_call_contracts::call_types::CallConferenceInvitation;
        let inv1 = CallConferenceInvitation {
            invitation_id: "inv-1".into(),
            revision: 1,
            group_id: "room@chatroom".into(),
            caller_id: "alice".into(),
        };
        let inv2 = CallConferenceInvitation {
            invitation_id: "inv-2".into(),
            revision: 1,
            group_id: "room2@chatroom".into(),
            caller_id: "bob".into(),
        };
        let inv2_rev2 = CallConferenceInvitation {
            revision: 2,
            ..inv2.clone()
        };
        let mut tracker = ConferenceTracker::default();
        tracker.seed(std::slice::from_ref(&inv1));
        // Initial invitation is not alerted again.
        assert!(tracker.take_fresh(std::slice::from_ref(&inv1)).is_empty());
        // Fresh invitation alerts once.
        assert_eq!(tracker.take_fresh(&[inv1.clone(), inv2.clone()]).len(), 1);
        assert!(tracker.take_fresh(&[inv1.clone(), inv2.clone()]).is_empty());
        // A bumped revision is treated as fresh.
        assert_eq!(tracker.take_fresh(&[inv1, inv2_rev2]).len(), 1);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_payload_deserializes_both_legacy_messages_and_call_actions() {
        let legacy = json!({
            "target": {
                "connectionId": "local",
                "serverId": "srv-1",
                "accountId": "acct-1",
                "threadId": "alice",
                "messageId": "msg-1"
            },
            "serverId": "wx-msg-9"
        });
        assert!(matches!(
            serde_json::from_value::<MacPayload>(legacy).unwrap(),
            MacPayload::Message(p) if p.server_id.as_deref() == Some("wx-msg-9")
        ));
        let call = json!({
            "connectionId": "local",
            "serverId": "srv-1",
            "accountId": "acct-1",
            "peerId": "alice",
            "callId": "call-7",
            "revision": 2,
            "callType": "video",
            "action": "open"
        });
        assert!(matches!(
            serde_json::from_value::<MacPayload>(call).unwrap(),
            MacPayload::Call(c) if c.call_id == "call-7" && c.revision == 2
        ));
    }
    #[test]
    fn face_aliases_are_displayed_before_notification_truncation() {
        for (kind, subtype) in [(1, Value::Null), (49, json!(57))] {
            let raw = "[Smile]".repeat(181);
            let message = json!({
                "type": kind, "appMessageType": subtype,
                "body": raw, "direction": "incoming"
            });
            assert_eq!(preview(&message), Some("🙂".repeat(180)));
            assert_eq!(message["body"], raw);
        }
        assert_eq!(
            preview(
                &json!({"type":1,"body":"Hello [微笑] [future face] ❤️", "direction":"incoming"})
            ),
            Some("Hello 🙂 [future face] ❤️".into())
        );
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
    fn groups_use_sender_title_and_group_subtitle_while_direct_titles_stay_unchanged() {
        let message = json!({"threadId":"room@chatroom","senderId":"alice"});
        assert_eq!(
            notification_heading(
                &json!({"kind":"group","displayName":"Team"}),
                &message,
                Some("Alice")
            ),
            ("Alice".into(), Some("Team".into()))
        );
        assert_eq!(
            notification_heading(&json!({"displayName":"Team"}), &message, None),
            ("Member".into(), Some("Team".into()))
        );
        assert_eq!(
            notification_heading(
                &json!({"kind":"direct","displayName":"Alice"}),
                &json!({"threadId":"alice"}),
                Some("Alice")
            ),
            ("Alice".into(), None)
        );
        assert_eq!(
            notification_heading(
                &json!({"kind":"group","displayName":"😀".repeat(200)}),
                &message,
                Some("Alice")
            )
            .1
            .unwrap()
            .chars()
            .count(),
            100
        );
    }
    #[test]
    fn sender_names_only_match_the_message_author_and_are_safe_for_previews() {
        let message = json!({"senderId":"alice"});
        let thread = json!({
            "lastMessage":{"senderId":"bob","senderName":"Bob"},
            "avatarMembers":[{"id":"alice","displayName":"Alice"}]
        });
        assert_eq!(cached_sender(&thread, &message).as_deref(), Some("Alice"));
        assert!(cached_sender(
            &json!({"lastMessage":{"senderId":"bob","senderName":"Bob"}}),
            &message
        )
        .is_none());
        assert_eq!(
            cached_sender(&json!({"lastMessage":{"senderId":"alice","senderName":" Alice\n\t Example\u{0000} "}}), &message).as_deref(),
            Some("Alice Example")
        );
        assert!(sender_label(&json!("alice"), "alice").is_none());
        assert!(sender_label(&json!("\n\t"), "alice").is_none());
        assert_eq!(
            sender_label(&json!("😀".repeat(100)), "alice")
                .unwrap()
                .chars()
                .count(),
            60
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

    #[cfg(target_os = "macos")]
    fn quoted(server_id: Option<&str>, message_id: Option<&str>) -> Payload {
        Payload {
            target: Target {
                connection_id: "hub".into(),
                server_id: "server".into(),
                account_id: "me".into(),
                thread_id: "team@chatroom".into(),
                message_id: message_id.map(Into::into),
            },
            server_id: server_id.map(Into::into),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_reply_quotes_the_alerted_message_once() {
        let payload = quoted(Some("7302551184426"), Some("m-1"));
        assert!(payload.quotable());
        let (key, body) = reply_request("hub|me|m-1", &payload, "  On my way \n").unwrap();
        assert_eq!(
            body,
            json!({
                "threadId": "team@chatroom",
                "text": "On my way",
                "quote": { "messageId": "m-1", "serverId": "7302551184426" },
            })
        );
        // Same alert, same key: a retried request cannot send twice.
        assert_eq!(
            key,
            reply_request("hub|me|m-1", &payload, "Other").unwrap().0
        );
        assert_ne!(
            key,
            reply_request("hub|me|m-2", &payload, "On my way")
                .unwrap()
                .0
        );
        assert!(key.len() <= 200 && key.bytes().all(|b| b.is_ascii_graphic()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reply_needs_a_quotable_message_and_some_text() {
        assert!(!quoted(None, Some("m-1")).quotable());
        assert!(!quoted(Some("73"), None).quotable());
        assert!(reply_request("id", &quoted(None, Some("m-1")), "Hi").is_none());
        assert!(reply_request("id", &quoted(Some("73"), None), "Hi").is_none());
        assert!(reply_request("id", &quoted(Some("73"), Some("m-1")), " \n ").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn alerts_group_by_chat_and_are_identified_by_message() {
        let payload = quoted(Some("73"), Some("m-1"));
        assert_eq!(payload.target.thread_key(), "hub|me|team@chatroom");
        assert_eq!(payload.target.request_id(), "hub|me|m-1");
        assert_eq!(
            thread_key("hub", "me", "team@chatroom"),
            payload.target.thread_key(),
            "the read chat clears the same group its alerts were posted in"
        );
        let json = serde_json::to_string(&payload).unwrap();
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.server_id.as_deref(), Some("73"));
        assert_eq!(back.target.request_id(), "hub|me|m-1");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mark_read_waits_for_newer_incoming_messages_to_be_seen() {
        let page = |items: Value| json!({ "items": items });
        let m = |id: &str, direction: &str| json!({ "id": id, "direction": direction });
        // Only our own reply came after the alerted message: safe to mark read.
        let replied = page(json!([
            m("r", "outgoing"),
            m("a", "incoming"),
            m("z", "incoming")
        ]));
        assert_eq!(newer_incoming(&replied, "a"), Some(false));
        // Someone wrote again after the alert: the user hasn't seen it.
        let newer = page(json!([
            m("b", "incoming"),
            m("r", "outgoing"),
            m("a", "incoming")
        ]));
        assert_eq!(newer_incoming(&newer, "a"), Some(true));
        // The alerted message isn't on the page: unknown, so not safe.
        assert_eq!(
            newer_incoming(&page(json!([m("b", "incoming")])), "a"),
            None
        );
        assert_eq!(newer_incoming(&json!({}), "a"), None);
    }
}
