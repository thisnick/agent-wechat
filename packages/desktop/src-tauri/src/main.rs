#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod config;
mod credentials;
mod emoji;
mod files;
mod hub;
mod i18n;
#[cfg(target_os = "macos")]
mod macos_notifications;
mod notification_sound;
mod notifications;
mod transport;
mod updates;

use anyhow::{bail, Context, Result};
use config::{Config, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, AtomicU8, Ordering},
        Arc,
    },
};
use tauri::{Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

pub struct Active {
    connection: Connection,
    key: Zeroizing<String>,
    http: reqwest::Client,
}
pub struct Inner {
    config: Config,
    hub: Option<hub::OwnedHub>,
    active: Option<Arc<Active>>,
    /// Why this device's hub last failed to start, until it starts.
    local_error: Option<LocalHubError>,
}
#[derive(Clone)]
struct LocalHubError {
    message: String,
    /// A hub from an earlier run of this app holds the local port.
    leftover: bool,
}
pub struct Desktop {
    inner: Mutex<Inner>,
    config_file: PathBuf,
    data: PathBuf,
    binaries: PathBuf,
    uploads: Mutex<HashMap<String, transport::Upload>>,
    requests: Arc<Mutex<HashMap<String, transport::Control>>>,
    quit: AtomicU8,
    /// Process ID of this device's hub while it runs or starts, else 0. It is
    /// read without the `inner` lock when the app ends.
    hub_pid: AtomicU32,
    notifications: notifications::Notifications,
    updates: updates::Updates,
}
/// How long an ending app waits for its hub to stop.
#[cfg(unix)]
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Whether this device's hub should run: while "This device" is selected, or
/// while another hub is selected if the user chose to keep it running.
fn local_wanted(config: &Config) -> bool {
    config.connections.iter().any(|connection| connection.local)
        && (config.active.as_deref() == Some("local") || config.keep_local_hub_running)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    endpoint: String,
    local: bool,
    status: Option<serde_json::Value>,
}
impl From<&Connection> for Session {
    fn from(value: &Connection) -> Self {
        Self {
            id: value.id.clone(),
            endpoint: value.endpoint.clone(),
            local: value.local,
            status: None,
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    connections: Vec<Connection>,
    active: Option<String>,
    local_port: u16,
    network_port: u16,
    network_enabled: bool,
    network_addresses: Vec<String>,
    certificate: Option<String>,
    cli_path: String,
    notifications_enabled: bool,
    notification_sound_enabled: bool,
    notification_status: String,
    keep_local_hub_running: bool,
    local_hub_error: Option<String>,
    leftover_hub: bool,
}
impl Desktop {
    async fn start_local(&self, inner: &mut Inner, key: Zeroizing<String>) -> Result<()> {
        // Bootstrap may be queued by a WebView reload while Quit drains the hub.
        // Check under the same inner lock used by shutdown before starting it again.
        if self.quit.load(Ordering::Acquire) != 0 {
            bail!("The desktop app is shutting down");
        }
        if inner
            .hub
            .as_mut()
            .is_some_and(|owned| owned.child.try_wait().ok().flatten().is_some())
        {
            inner.hub = None;
            self.hub_pid.store(0, Ordering::Release);
        }
        if inner.hub.is_none() {
            let started = hub::start(
                &inner.config,
                &self.data,
                &self.binaries,
                Zeroizing::new(key.to_string()),
                &self.hub_pid,
            )
            .await;
            match started {
                Ok(owned) => {
                    inner.hub = Some(owned);
                    inner.local_error = None;
                }
                Err(error) => {
                    self.hub_pid.store(0, Ordering::Release);
                    let leftover = hub::leftover(&inner.config, &self.data, &key)
                        .await
                        .is_some();
                    // The port error alone suggests choosing another port, but
                    // the fix here is to stop the hub an earlier run left behind.
                    let error = if leftover {
                        anyhow::anyhow!(
                            "An earlier AgentWechat session left its hub running on port {}",
                            inner.config.local_port
                        )
                    } else {
                        error
                    };
                    inner.local_error = Some(LocalHubError {
                        message: error.to_string(),
                        leftover,
                    });
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    /// Starts this device's hub if the user keeps it running while another hub
    /// is selected. Selecting "This device" starts it through `activate`.
    async fn resume_local(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if !local_wanted(&inner.config) {
            return Ok(());
        }
        let key = credentials::read(&inner.config.installation_id, "local")?
            .context("The local hub access key is missing; restore it before starting this hub")?;
        self.start_local(&mut inner, key).await
    }
    /// Stops this device's hub if it runs. Callers cancel open requests first.
    async fn stop_local(&self, inner: &mut Inner) -> Result<()> {
        if let Some(owned) = inner.hub.as_mut() {
            hub::stop(owned).await?;
        }
        inner.hub = None;
        self.hub_pid.store(0, Ordering::Release);
        Ok(())
    }
    /// Stops this device's hub once nothing needs it, such as after switching
    /// to another hub. A hub that refuses to stop stays owned and stops at Quit.
    async fn stop_unused_local(&self) {
        {
            let inner = self.inner.lock().await;
            if inner.hub.is_none() || local_wanted(&inner.config) {
                return;
            }
        }
        for control in self.requests.lock().await.values() {
            control.cancel.cancel();
        }
        let mut inner = self.inner.lock().await;
        if inner.hub.is_some() && !local_wanted(&inner.config) {
            let _ = self.stop_local(&mut inner).await;
        }
    }
    /// Signals this device's hub to stop and waits briefly, for an app the
    /// system is ending without asking first (on macOS: Quit from the Dock,
    /// logging out or shutting down). The hub also stops by itself once the app
    /// is gone; this only starts that sooner, so a quick relaunch finds the
    /// local port free. It never blocks longer than [`EXIT_WAIT`].
    fn stop_hub_now(&self) {
        let pid = self.hub_pid.swap(0, Ordering::AcqRel);
        if pid == 0 {
            return;
        }
        #[cfg(unix)]
        {
            // The process is this app's unreaped child, so its ID is not reused.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            tauri::async_runtime::block_on(async {
                let _ = tokio::time::timeout(EXIT_WAIT, async {
                    if let Some(owned) = self.inner.lock().await.hub.as_mut() {
                        let _ = owned.child.wait().await;
                    }
                })
                .await;
            });
        }
    }
    async fn bootstrap(&self) -> Result<Option<Session>> {
        let id = self.inner.lock().await.config.active.clone();
        if id.as_deref() != Some("local") {
            // A kept-running local hub must not stop the selected hub from
            // opening. Its error stays in settings, with a fix when there is one.
            if let Err(error) = self.resume_local().await {
                let mut inner = self.inner.lock().await;
                if inner.local_error.is_none() {
                    inner.local_error = Some(LocalHubError {
                        message: error.to_string(),
                        leftover: false,
                    });
                }
            }
        }
        match id {
            Some(id) => self.activate(&id).await.map(Some),
            None => Ok(None),
        }
    }
    async fn activate(&self, id: &str) -> Result<Session> {
        let mut inner = self.inner.lock().await;
        let connection = inner
            .config
            .connections
            .iter()
            .find(|connection| connection.id == id)
            .context("Saved hub was not found")?
            .clone();
        let key = credentials::read(&inner.config.installation_id, id)?
            .context("The saved access key is missing; reconnect to this hub")?;
        let mut connection = if connection.local {
            hub::local_connection(&inner.config)
        } else {
            connection
        };
        let identity = if connection.local {
            self.start_local(&mut inner, Zeroizing::new(key.to_string()))
                .await?;
            inner.hub.as_ref().unwrap().identity.clone()
        } else {
            hub::identity(&connection, &key).await?
        };
        connection.server_id = Some(
            identity["serverId"]
                .as_str()
                .context("Missing hub identity")?
                .into(),
        );
        let active = Arc::new(Active {
            http: hub::http(&connection)?,
            connection: connection.clone(),
            key,
        });
        let mut session = Session::from(&connection);
        session.status = Some(identity);
        let local = connection.local;
        inner.config.connections.retain(|value| value.id != id);
        inner.config.connections.push(connection);
        inner.config.active = Some(id.into());
        inner.config.save(&self.config_file)?;
        inner.active = Some(active);
        drop(inner);
        if !local {
            self.stop_unused_local().await;
        }
        Ok(session)
    }
    async fn stop_hub(&self) -> Result<()> {
        for control in self.requests.lock().await.values() {
            control.cancel.cancel();
        }
        let mut inner = self.inner.lock().await;
        self.stop_local(&mut inner).await
    }
    async fn stop(&self) -> Result<()> {
        self.stop_hub().await?;
        self.inner.lock().await.active = None;
        self.notifications.cancel.cancel();
        Ok(())
    }
}
#[tauri::command]
async fn desktop_bootstrap(state: State<'_, Desktop>) -> Result<Option<Session>, String> {
    state.bootstrap().await.map_err(|error| error.to_string())
}
#[tauri::command]
async fn desktop_local(state: State<'_, Desktop>) -> Result<Session, String> {
    let result = async {
        let mut inner = state.inner.lock().await;
        if !inner.config.connections.iter().any(|value| value.local) {
            // Never replace a missing key for an existing hub or create a second identity.
            if state.data.exists() && std::fs::read_dir(&state.data)?.next().is_some() { bail!("This hub already has data but no saved connection; restore its access key before continuing"); }
            let key = match credentials::read(&inner.config.installation_id, "local")? { Some(key) => key, None => {
                let key = hub::fresh_key(); credentials::write(&inner.config.installation_id, "local", &key)?; key
            } };
            drop(key);
            let connection = hub::local_connection(&inner.config);
            inner.config.connections.push(connection);
            inner.config.save(&state.config_file)?;
        }
        drop(inner);
        state.activate("local").await
    }.await;
    result.map_err(|error: anyhow::Error| error.to_string())
}
#[tauri::command]
async fn desktop_remote(
    state: State<'_, Desktop>,
    name: String,
    endpoint: String,
    key: String,
    certificate: Option<String>,
) -> Result<Session, String> {
    let key = Zeroizing::new(key);
    let result = async {
        config::token(&key)?;
        let url = config::endpoint(&endpoint)?;
        if name.trim().is_empty() || name.len() > 128 {
            bail!("Enter a hub name of at most 128 bytes");
        }
        if certificate
            .as_ref()
            .is_some_and(|value| value.len() > 64 * 1024)
        {
            bail!("Certificate is too large");
        }
        let mut connection = Connection {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().into(),
            endpoint: url.origin().ascii_serialization(),
            local: false,
            server_id: None,
            certificate,
        };
        let identity = hub::identity(&connection, &key).await?;
        connection.server_id = identity["serverId"].as_str().map(str::to_owned);
        let mut inner = state.inner.lock().await;
        if inner.config.connections.len() >= 32 {
            bail!("Remove a saved hub before adding another");
        }
        credentials::write(&inner.config.installation_id, &connection.id, &key)?;
        let id = connection.id.clone();
        inner.config.connections.push(connection);
        inner.config.save(&state.config_file)?;
        drop(inner);
        state.activate(&id).await
    }
    .await;
    result.map_err(|error: anyhow::Error| error.to_string())
}
#[tauri::command]
async fn desktop_select(state: State<'_, Desktop>, id: String) -> Result<Session, String> {
    state.activate(&id).await.map_err(|error| error.to_string())
}
#[tauri::command]
async fn desktop_settings(state: State<'_, Desktop>) -> Result<Settings, String> {
    let inner = state.inner.lock().await;
    let config = &inner.config;
    let certificate = std::fs::read_to_string(state.data.join("network/certificate.pem")).ok();
    let addresses = if config.network_enabled {
        wechat_platform::local_ipv4_addresses()
            .unwrap_or_default()
            .iter()
            .map(|ip| format!("https://{ip}:{}", config.network_port))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Settings {
        connections: config.connections.clone(),
        active: config.active.clone(),
        local_port: config.local_port,
        network_port: config.network_port,
        network_enabled: config.network_enabled,
        network_addresses: addresses,
        certificate,
        cli_path: hub::executable(&state.binaries, "wx")
            .map_err(|error| error.to_string())?
            .display()
            .to_string(),
        notifications_enabled: config.notifications_enabled,
        notification_sound_enabled: config.notification_sound_enabled,
        notification_status: notifications::status(&state).await,
        keep_local_hub_running: config.keep_local_hub_running,
        local_hub_error: inner
            .local_error
            .as_ref()
            .map(|error| error.message.clone()),
        leftover_hub: inner
            .local_error
            .as_ref()
            .is_some_and(|error| error.leftover),
    })
}
/// Keeps this device's hub running while another hub is selected, or lets it
/// stop then. The hub stops when the app quits either way.
#[tauri::command]
async fn desktop_keep_local_hub(state: State<'_, Desktop>, enabled: bool) -> Result<(), String> {
    let result = async {
        {
            let mut inner = state.inner.lock().await;
            let mut next = inner.config.clone();
            next.keep_local_hub_running = enabled;
            next.save(&state.config_file)?;
            inner.config = next;
        }
        if enabled {
            state.resume_local().await
        } else {
            state.stop_unused_local().await;
            Ok(())
        }
    }
    .await;
    result.map_err(|error: anyhow::Error| error.to_string())
}
/// Stops a hub that an earlier run of this app left on the local port, after
/// checking it uses this app's data folder and key, then starts this device's
/// hub if it should run. Returns the session when "This device" is selected.
#[tauri::command]
async fn desktop_stop_leftover_hub(state: State<'_, Desktop>) -> Result<Option<Session>, String> {
    let result = async {
        let config = state.inner.lock().await.config.clone();
        let key = credentials::read(&config.installation_id, "local")?
            .context("The local hub access key is missing; restore it before starting this hub")?;
        hub::stop_leftover(&config, &state.data, &key).await?;
        state.inner.lock().await.local_error = None;
        if config.active.as_deref() == Some("local") {
            return state.activate("local").await.map(Some);
        }
        state.resume_local().await?;
        Ok(None)
    }
    .await;
    result.map_err(|error: anyhow::Error| error.to_string())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalSettings {
    local_port: u16,
    network_port: u16,
    network_enabled: bool,
}
#[tauri::command]
async fn desktop_save_local(
    state: State<'_, Desktop>,
    settings: LocalSettings,
) -> Result<Option<Session>, String> {
    let result = async {
        let mut inner = state.inner.lock().await;
        let previous = inner.config.clone();
        let mut next = previous.clone();
        next.local_port = settings.local_port;
        next.network_port = settings.network_port;
        next.network_enabled = settings.network_enabled;
        next.validate()?;
        if !next.connections.iter().any(|connection| connection.local) {
            next.save(&state.config_file)?;
            inner.config = next;
            return Ok(None);
        }
        if next.network_enabled {
            hub::tls_files(&state.data)?;
        }
        state.stop_local(&mut inner).await?;
        inner.active = None;
        inner.config = next;
        drop(inner);
        match state.activate("local").await {
            Ok(session) => Ok(Some(session)),
            Err(error) => {
                let mut inner = state.inner.lock().await;
                inner.config = previous;
                inner.config.save(&state.config_file)?;
                drop(inner);
                let _ = state.activate("local").await;
                Err(error)
            }
        }
    }
    .await;
    result.map_err(|error: anyhow::Error| error.to_string())
}
#[tauri::command]
async fn desktop_reveal(state: State<'_, Desktop>, id: String) -> Result<String, String> {
    let inner = state.inner.lock().await;
    if !inner.config.connections.iter().any(|value| value.id == id) {
        return Err("Saved hub was not found".into());
    }
    credentials::read(&inner.config.installation_id, &id)
        .map_err(|error| error.to_string())?
        .map(|value| value.to_string())
        .ok_or("The saved access key is missing".into())
}
#[tauri::command]
async fn desktop_copy_key(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    id: String,
) -> Result<(), String> {
    let inner = state.inner.lock().await;
    if !inner
        .config
        .connections
        .iter()
        .any(|connection| connection.id == id)
    {
        return Err("Saved hub was not found".into());
    }
    let key = credentials::read(&inner.config.installation_id, &id)
        .map_err(|error| error.to_string())?
        .ok_or("The saved access key is missing")?;
    app.clipboard()
        .write_text(key.to_string())
        .map_err(|_| "Cannot copy the access key".into())
}
#[tauri::command]
async fn desktop_forget(state: State<'_, Desktop>, id: String) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if id == "local" {
        return Err("The local hub cannot be removed here; its profiles are preserved".into());
    }
    credentials::remove(&inner.config.installation_id, &id).map_err(|error| error.to_string())?;
    inner.config.connections.retain(|value| value.id != id);
    if inner.config.active.as_ref() == Some(&id) {
        inner.config.active = None;
        inner.active = None;
    }
    inner
        .config
        .save(&state.config_file)
        .map_err(|error| error.to_string())
}
#[tauri::command]
fn desktop_install_cli(state: State<'_, Desktop>) -> Result<String, String> {
    install_cli(&state.binaries).map_err(|error| error.to_string())
}
fn install_cli(binaries: &std::path::Path) -> Result<String> {
    let source = hub::executable(binaries, "wx")?;
    #[cfg(unix)]
    {
        let directory =
            PathBuf::from(std::env::var_os("HOME").context("Home folder is unavailable")?)
                .join(".local/bin");
        std::fs::create_dir_all(&directory)?;
        let destination = directory.join("wx");
        if let Ok(existing) = std::fs::symlink_metadata(&destination) {
            if !existing.file_type().is_symlink() || std::fs::read_link(&destination)? != source {
                bail!(
                    "Another wx command is already installed in ~/.local/bin; it was not replaced"
                );
            }
        } else {
            std::os::unix::fs::symlink(source, &destination)?;
        }
        Ok("Installed in ~/.local/bin. Add that folder to your shell PATH if needed, then open a new terminal.".into())
    }
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        let directory = PathBuf::from(
            std::env::var_os("LOCALAPPDATA").context("Local application folder is unavailable")?,
        )
        .join("AgentWechat/bin");
        std::fs::create_dir_all(&directory)?;
        std::fs::copy(source, directory.join("wx.exe"))?;
        let (environment, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Environment")?;
        let current: String = environment.get_value("Path").unwrap_or_default();
        let selected = directory.to_string_lossy();
        if !current
            .split(';')
            .any(|item| item.eq_ignore_ascii_case(&selected))
        {
            let value = if current.is_empty() {
                selected.to_string()
            } else {
                format!("{current};{selected}")
            };
            let bytes: Vec<u8> = value
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            environment.set_raw_value(
                "Path",
                &winreg::RegValue {
                    bytes,
                    vtype: winreg::enums::RegType::REG_EXPAND_SZ,
                },
            )?;
        }
        let environment_name: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
        // Notify Explorer so newly launched terminals inherit the updated user PATH.
        // The UTF-16 string stays alive throughout the synchronous, timed call.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageTimeoutW(
                windows_sys::Win32::UI::WindowsAndMessaging::HWND_BROADCAST,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE,
                0,
                environment_name.as_ptr() as isize,
                windows_sys::Win32::UI::WindowsAndMessaging::SMTO_ABORTIFHUNG,
                1000,
                std::ptr::null_mut(),
            );
        }
        Ok("Installed for this Windows user. Restart your terminal to use wx.".into())
    }
}
#[tauri::command]
fn desktop_quit(app: tauri::AppHandle) {
    request_quit(app);
}
/// Tray and app menu items whose labels follow the UI language.
struct TrayMenu {
    show: tauri::menu::MenuItem<tauri::Wry>,
    quit: tauri::menu::MenuItem<tauri::Wry>,
    #[cfg(target_os = "macos")]
    app_quit: tauri::menu::MenuItem<tauri::Wry>,
}
/// Menu item IDs. Every Quit item stops the hub before the app exits.
const MENU_SHOW: &str = "show";
const MENU_QUIT: &str = "quit";
#[cfg(target_os = "macos")]
const MENU_APP_QUIT: &str = "app-quit";
fn menu_event(app: &tauri::AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_QUIT => request_quit(app.clone()),
        #[cfg(target_os = "macos")]
        MENU_APP_QUIT => request_quit(app.clone()),
        _ => {}
    }
}
/// Tauri's default macOS menu, with its Quit item replaced. The standard
/// item ends the app at once, without the chance to stop the hub that
/// `request_quit` needs; this one runs `request_quit` for the same ⌘Q.
#[cfg(target_os = "macos")]
fn app_menu(
    app: &tauri::AppHandle,
    quit: &tauri::menu::MenuItem<tauri::Wry>,
) -> Result<tauri::menu::Menu<tauri::Wry>> {
    let menu = tauri::menu::Menu::default(app)?;
    let application = menu
        .items()?
        .into_iter()
        .next()
        .and_then(|item| item.as_submenu().cloned())
        .context("Missing application menu")?;
    let last = application
        .items()?
        .len()
        .checked_sub(1)
        .context("Missing application menu")?;
    // The default application menu ends with the standard Quit item.
    match application.remove_at(last)? {
        Some(tauri::menu::MenuItemKind::Predefined(_)) => {}
        _ => bail!("Unexpected application menu"),
    }
    application.append(quit)?;
    Ok(menu)
}
/// Records the language the UI shows, for tray and notification text.
#[tauri::command]
async fn desktop_language(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    language: String,
) -> Result<(), String> {
    let language = i18n::supported(language.trim()).ok_or("Unsupported display language")?;
    let mut inner = state.inner.lock().await;
    if inner.config.language.as_deref() != Some(language) {
        let mut next = inner.config.clone();
        next.language = Some(language.into());
        next.save(&state.config_file)
            .map_err(|error| error.to_string())?;
        inner.config = next;
    }
    drop(inner);
    i18n::set_language(language);
    if let Some(tray) = app.try_state::<TrayMenu>() {
        let _ = tray.show.set_text(i18n::text("trayOpen"));
        let _ = tray.quit.set_text(i18n::text("trayQuit"));
        #[cfg(target_os = "macos")]
        let _ = tray.app_quit.set_text(i18n::text("trayQuit"));
    }
    Ok(())
}
fn request_quit(app: tauri::AppHandle) {
    let state = app.state::<Desktop>();
    if state
        .quit
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Desktop>();
        match state.stop().await {
            Ok(()) => {
                state.quit.store(2, Ordering::Release);
                app.exit(0);
            }
            Err(_) => {
                state.quit.store(0, Ordering::Release);
                app.dialog()
                    .message(i18n::text("quitStillStopping"))
                    .title("AgentWechat")
                    .show(|_| {});
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
            }
        }
    });
}
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .setup(|app| {
            let config_file = app.path().app_config_dir()?.join("settings/desktop.json");
            let config = Config::load(&config_file)?;
            config.save(&config_file)?;
            i18n::set_language(config.language.as_deref().unwrap_or("en"));
            let data = app.path().app_local_data_dir()?.join("hub");
            #[cfg(target_os = "linux")]
            let binaries = app.path().resource_dir()?.join("bin");
            #[cfg(not(target_os = "linux"))]
            let binaries = std::env::current_exe()?
                .parent()
                .context("Missing executable folder")?
                .to_owned();
            app.manage(Desktop {
                inner: Mutex::new(Inner {
                    config,
                    hub: None,
                    active: None,
                    local_error: None,
                }),
                config_file,
                data,
                binaries,
                requests: Arc::new(Mutex::new(HashMap::new())),
                uploads: Mutex::new(HashMap::new()),
                quit: AtomicU8::new(0),
                hub_pid: AtomicU32::new(0),
                notifications: notifications::Notifications::default(),
                updates: updates::Updates::default(),
            });
            notifications::start(app.handle().clone())?;
            updates::start(app.handle().clone());
            let show = tauri::menu::MenuItem::with_id(
                app,
                MENU_SHOW,
                i18n::text("trayOpen"),
                true,
                None::<&str>,
            )?;
            let quit = tauri::menu::MenuItem::with_id(
                app,
                MENU_QUIT,
                i18n::text("trayQuit"),
                true,
                None::<&str>,
            )?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            #[cfg(target_os = "macos")]
            let app_quit = {
                let item = tauri::menu::MenuItem::with_id(
                    app,
                    MENU_APP_QUIT,
                    i18n::text("trayQuit"),
                    true,
                    Some("CmdOrCtrl+Q"),
                )?;
                app.set_menu(app_menu(app.handle(), &item)?)?;
                item
            };
            app.manage(TrayMenu {
                show: show.clone(),
                quit: quit.clone(),
                #[cfg(target_os = "macos")]
                app_quit,
            });
            // Menu events from the tray and the app menu share one handler.
            app.on_menu_event(menu_event);
            tauri::tray::TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .context("Missing app icon")?
                        .clone(),
                )
                .menu(&menu)
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap,
            desktop_local,
            desktop_remote,
            desktop_select,
            desktop_settings,
            desktop_language,
            desktop_save_local,
            desktop_keep_local_hub,
            desktop_stop_leftover_hub,
            desktop_reveal,
            desktop_copy_key,
            desktop_forget,
            desktop_install_cli,
            desktop_quit,
            notifications::notifications_context,
            notifications::notifications_take_target,
            notifications::notifications_enabled,
            notifications::notifications_account_settings,
            notifications::notifications_sound_enabled,
            updates::updates_status,
            updates::updates_preferences,
            updates::updates_check,
            updates::updates_install,
            files::desktop_save_file,
            files::desktop_export_key,
            transport::http_body_begin,
            transport::http_body_chunk,
            transport::http_request,
            transport::http_ack,
            transport::http_cancel
        ])
        .build(tauri::generate_context!())
        .expect("Cannot initialize AgentWechat")
        .run(|app, event| match event {
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show_main_window(app),
            tauri::RunEvent::ExitRequested { api, .. } => {
                if app.state::<Desktop>().quit.load(Ordering::Acquire) != 2 {
                    api.prevent_exit();
                    request_quit(app.clone());
                }
            }
            // Reached without ExitRequested when the system ends the app, as
            // macOS does for Quit from the Dock, logging out or shutting down.
            tauri::RunEvent::Exit => app.state::<Desktop>().stop_hub_now(),
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(directory: &std::path::Path, config: Config) -> Desktop {
        Desktop {
            inner: Mutex::new(Inner {
                config,
                hub: None,
                active: None,
                local_error: None,
            }),
            config_file: directory.join("desktop.json"),
            data: directory.join("hub"),
            binaries: directory.to_owned(),
            uploads: Mutex::new(HashMap::new()),
            requests: Arc::new(Mutex::new(HashMap::new())),
            quit: AtomicU8::new(0),
            hub_pid: AtomicU32::new(0),
            notifications: notifications::Notifications::default(),
            updates: updates::Updates::default(),
        }
    }

    fn remote_selected_with_local_saved() -> Config {
        let mut config = Config {
            active: Some("remote".into()),
            ..Config::default()
        };
        config.connections.push(hub::local_connection(&config));
        config.connections.push(Connection {
            id: "remote".into(),
            name: "Fixture remote".into(),
            endpoint: "https://hub.example".into(),
            local: false,
            server_id: None,
            certificate: None,
        });
        config
    }

    #[test]
    fn local_hub_is_wanted_only_while_selected_or_kept_running() {
        let mut config = remote_selected_with_local_saved();
        assert!(!local_wanted(&config));
        config.keep_local_hub_running = true;
        assert!(local_wanted(&config));
        config.keep_local_hub_running = false;
        config.active = Some("local".into());
        assert!(local_wanted(&config));
        // Without a saved local connection there is no local hub to run.
        config.connections.retain(|connection| !connection.local);
        config.keep_local_hub_running = true;
        assert!(!local_wanted(&config));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_hub_does_not_start_while_another_hub_is_selected() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let marker = temporary.path().join("hub-launched");
        let executable = temporary.path().join("wx");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let desktop = fixture(temporary.path(), remote_selected_with_local_saved());
        desktop.resume_local().await.unwrap();
        assert!(!marker.exists(), "the local hub started while unused");
        assert!(desktop.inner.lock().await.local_error.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_ending_app_signals_its_hub_even_while_it_starts() {
        use std::os::unix::process::ExitStatusExt;
        let temporary = tempfile::tempdir().unwrap();
        let desktop = fixture(temporary.path(), Config::default());
        // Stands in for a hub that has been spawned but is not ready yet.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        desktop.hub_pid.store(child.id(), Ordering::Release);
        let started = std::time::Instant::now();
        desktop.stop_hub_now();
        assert!(started.elapsed() < EXIT_WAIT);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
        assert_eq!(desktop.hub_pid.load(Ordering::Acquire), 0);
        desktop.stop_hub_now();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn queued_local_start_cannot_launch_a_hub_after_quit_begins() {
        use std::os::unix::fs::PermissionsExt;

        for quit in [1, 2] {
            let temporary = tempfile::tempdir().unwrap();
            let marker = temporary.path().join("hub-launched");
            let executable = temporary.path().join("wx");
            std::fs::write(
                &executable,
                format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let desktop = Desktop {
                inner: Mutex::new(Inner {
                    config: Config {
                        local_port: 0,
                        ..Config::default()
                    },
                    hub: None,
                    active: None,
                    local_error: None,
                }),
                config_file: temporary.path().join("desktop.json"),
                data: temporary.path().join("hub"),
                binaries: temporary.path().to_owned(),
                uploads: Mutex::new(HashMap::new()),
                requests: Arc::new(Mutex::new(HashMap::new())),
                quit: AtomicU8::new(quit),
                hub_pid: AtomicU32::new(0),
                notifications: notifications::Notifications::default(),
                updates: updates::Updates::default(),
            };
            let mut inner = desktop.inner.lock().await;
            assert!(desktop
                .start_local(&mut inner, hub::fresh_key())
                .await
                .is_err());
            assert!(
                !marker.exists(),
                "a queued bootstrap launched a hub during quit"
            );
            assert!(!desktop.data.exists(), "shutdown must not create hub state");
            assert!(inner.hub.is_none());
        }
    }

    #[tokio::test]
    #[ignore = "requires WX_BINARY and an isolated, unlocked system credential store"]
    async fn local_hub_resumes_without_changing_the_selected_remote_connection() {
        let binary = PathBuf::from(std::env::var_os("WX_BINARY").expect("WX_BINARY is required"));
        let temporary = wechat_platform::private_tempdir("desktop-background-").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = Config {
            local_port: listener.local_addr().unwrap().port(),
            active: Some("remote".into()),
            ..Config::default()
        };
        drop(listener);
        if config.local_port == config.network_port {
            config.network_port += 1;
        }
        config.connections.push(hub::local_connection(&config));
        config.connections.push(Connection {
            id: "remote".into(),
            name: "Fixture remote".into(),
            endpoint: "http://127.0.0.1:6123".into(),
            local: false,
            server_id: None,
            certificate: None,
        });
        let installation = config.installation_id.clone();
        let key = hub::fresh_key();
        credentials::write(&installation, "local", &key).unwrap();
        let config_file = temporary.path().join("desktop.json");
        config.save(&config_file).unwrap();
        let desktop = Desktop {
            inner: Mutex::new(Inner {
                config,
                hub: None,
                active: None,
                local_error: None,
            }),
            config_file,
            data: temporary.path().join("hub"),
            binaries: binary.parent().unwrap().to_owned(),
            uploads: Mutex::new(HashMap::new()),
            requests: Arc::new(Mutex::new(HashMap::new())),
            quit: AtomicU8::new(0),
            hub_pid: AtomicU32::new(0),
            notifications: notifications::Notifications::default(),
            updates: updates::Updates::default(),
        };
        // Off by default: the local hub runs only while it is selected.
        desktop.resume_local().await.unwrap();
        assert!(desktop.inner.lock().await.hub.is_none());
        desktop.inner.lock().await.config.keep_local_hub_running = true;
        desktop.resume_local().await.unwrap();
        assert_ne!(desktop.hub_pid.load(Ordering::Acquire), 0);
        {
            let inner = desktop.inner.lock().await;
            assert_eq!(inner.config.active.as_deref(), Some("remote"));
            assert!(inner.active.is_none());
            let hub = inner.hub.as_ref().unwrap();
            assert_eq!(
                hub::identity(&hub.connection, &key).await.unwrap()["serverId"],
                hub.identity["serverId"]
            );
        }
        desktop.stop().await.unwrap();
        credentials::remove(&installation, "local").unwrap();
    }
}
