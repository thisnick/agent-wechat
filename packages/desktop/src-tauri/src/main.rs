#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod config;
mod credentials;
mod files;
mod hub;
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
        atomic::{AtomicU8, Ordering},
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
}
pub struct Desktop {
    inner: Mutex<Inner>,
    config_file: PathBuf,
    data: PathBuf,
    binaries: PathBuf,
    uploads: Mutex<HashMap<String, transport::Upload>>,
    requests: Arc<Mutex<HashMap<String, transport::Control>>>,
    quit: AtomicU8,
    notifications: notifications::Notifications,
    updates: updates::Updates,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    endpoint: String,
    local: bool,
}
impl From<&Connection> for Session {
    fn from(value: &Connection) -> Self {
        Self {
            id: value.id.clone(),
            endpoint: value.endpoint.clone(),
            local: value.local,
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
    notification_status: String,
}
impl Desktop {
    async fn start_local(&self, inner: &mut Inner, key: Zeroizing<String>) -> Result<()> {
        if inner
            .hub
            .as_mut()
            .is_some_and(|owned| owned.child.try_wait().ok().flatten().is_some())
        {
            inner.hub = None;
        }
        if inner.hub.is_none() {
            inner.hub = Some(hub::start(&inner.config, &self.data, &self.binaries, key).await?);
        }
        Ok(())
    }
    async fn resume_local(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if !inner
            .config
            .connections
            .iter()
            .any(|connection| connection.local)
        {
            return Ok(());
        }
        let key = credentials::read(&inner.config.installation_id, "local")?
            .context("The local hub access key is missing; restore it before starting this hub")?;
        self.start_local(&mut inner, key).await
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
        let session = Session::from(&connection);
        inner.config.connections.retain(|value| value.id != id);
        inner.config.connections.push(connection);
        inner.config.active = Some(id.into());
        inner.config.save(&self.config_file)?;
        inner.active = Some(active);
        Ok(session)
    }
    async fn stop_hub(&self) -> Result<()> {
        for control in self.requests.lock().await.values() {
            control.cancel.cancel();
        }
        let mut inner = self.inner.lock().await;
        if let Some(owned) = inner.hub.as_mut() {
            hub::stop(owned).await?;
        }
        inner.hub = None;
        Ok(())
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
    let id = state.inner.lock().await.config.active.clone();
    // A configured local hub also serves other clients while this UI uses a remote hub.
    if id.as_deref() != Some("local") {
        state
            .resume_local()
            .await
            .map_err(|error| error.to_string())?;
    }
    match id {
        Some(id) => state
            .activate(&id)
            .await
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
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
        notification_status: notifications::status(&state).await,
    })
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
        if let Some(owned) = inner.hub.as_mut() {
            hub::stop(owned).await?;
        }
        inner.hub = None;
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
                    .message("The hub is still stopping. Wait and try Quit again.")
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
                }),
                config_file,
                data,
                binaries,
                requests: Arc::new(Mutex::new(HashMap::new())),
                uploads: Mutex::new(HashMap::new()),
                quit: AtomicU8::new(0),
                notifications: notifications::Notifications::default(),
                updates: updates::Updates::default(),
            });
            notifications::start(app.handle().clone())?;
            updates::start(app.handle().clone());
            let show = tauri::menu::MenuItem::with_id(
                app,
                "show",
                "Open AgentWechat",
                true,
                None::<&str>,
            )?;
            let quit = tauri::menu::MenuItem::with_id(
                app,
                "quit",
                "Quit AgentWechat",
                true,
                None::<&str>,
            )?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            tauri::tray::TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .context("Missing app icon")?
                        .clone(),
                )
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => request_quit(app.clone()),
                    _ => {}
                })
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
            desktop_save_local,
            desktop_reveal,
            desktop_copy_key,
            desktop_forget,
            desktop_install_cli,
            desktop_quit,
            notifications::notifications_context,
            notifications::notifications_take_target,
            notifications::notifications_enabled,
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
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

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
            }),
            config_file,
            data: temporary.path().join("hub"),
            binaries: binary.parent().unwrap().to_owned(),
            uploads: Mutex::new(HashMap::new()),
            requests: Arc::new(Mutex::new(HashMap::new())),
            quit: AtomicU8::new(0),
            notifications: notifications::Notifications::default(),
            updates: updates::Updates::default(),
        };
        desktop.resume_local().await.unwrap();
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
