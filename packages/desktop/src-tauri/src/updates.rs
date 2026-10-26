//! Release discovery uses GitHub; installation always uses Tauri's signed updater.
use crate::{desktop_bootstrap, Desktop};
use anyhow::{ensure, Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::Mutex as StdMutex, time::Duration};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

const REPOSITORY: &str = "https://github.com/thisnick/agent-wechat";
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    phase: String,
    version: Option<String>,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
}
#[derive(Default)]
pub struct Updates {
    status: StdMutex<Status>,
    pending: Mutex<Option<Update>>,
    gate: Mutex<()>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    status: Status,
    current_version: String,
    automatic: bool,
    prereleases: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preferences {
    automatic: bool,
    prereleases: bool,
}
fn publish(app: &tauri::AppHandle, status: Status) {
    *app.state::<Desktop>().updates.status.lock().unwrap() = status.clone();
    let _ = app.emit("desktop-update-status", status);
}
fn select_release(releases: &Value, current: &Version, prereleases: bool) -> Option<Version> {
    releases
        .as_array()?
        .iter()
        .filter(|r| r["draft"] == false)
        .filter_map(|release| {
            let version = Version::parse(release["tag_name"].as_str()?.strip_prefix('v')?).ok()?;
            if version <= *current
                || (!prereleases && (!version.pre.is_empty() || release["prerelease"] == true))
            {
                return None;
            }
            let asset = release["assets"]
                .as_array()?
                .iter()
                .find(|asset| asset["name"] == "desktop-updater.json")?;
            if !(1..=1024 * 1024).contains(&asset["size"].as_u64()?) {
                return None;
            }
            Some(version)
        })
        .max()
}
fn feed(version: &Version) -> Result<reqwest::Url> {
    Ok(format!("{REPOSITORY}/releases/download/v{version}/desktop-updater.json").parse()?)
}
async fn discover(app: &tauri::AppHandle) -> Result<Option<Update>> {
    let current = app.package_info().version.clone();
    let prereleases = app
        .state::<Desktop>()
        .inner
        .lock()
        .await
        .config
        .update_prereleases
        .unwrap_or(!current.pre.is_empty());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("AgentWechat desktop updater")
        .build()?;
    let mut response = client
        .get("https://api.github.com/repos/thisnick/agent-wechat/releases?per_page=100")
        .send()
        .await?
        .error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 4 * 1024 * 1024,
            "Release list exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let Some(version) = select_release(&serde_json::from_slice(&bytes)?, &current, prereleases)
    else {
        return Ok(None);
    };
    let mut update = app
        .updater_builder()
        .endpoints(vec![feed(&version)?])?
        .timeout(Duration::from_secs(30))
        .build()?
        .check()
        .await?
        .context("Update manifest did not match the release")?;
    ensure!(
        update.version == version.to_string()
            && update.download_url.as_str().starts_with(&format!(
                "{REPOSITORY}/releases/download/v{version}/AgentWechat-v{version}-"
            )),
        "Update identity mismatch"
    );
    update.timeout = None;
    Ok(Some(update))
}
async fn check(app: &tauri::AppHandle) -> Result<()> {
    let desktop = app.state::<Desktop>();
    let _guard = desktop
        .updates
        .gate
        .try_lock()
        .context("An update operation is already running")?;
    publish(
        app,
        Status {
            phase: "checking".into(),
            ..Default::default()
        },
    );
    match discover(app).await {
        Ok(update) => {
            let version = update.as_ref().map(|update| update.version.clone());
            *desktop.updates.pending.lock().await = update;
            publish(
                app,
                Status {
                    phase: if version.is_some() {
                        "available"
                    } else {
                        "current"
                    }
                    .into(),
                    version,
                    ..Default::default()
                },
            );
            Ok(())
        }
        Err(error) => {
            publish(
                app,
                Status {
                    phase: "error".into(),
                    error: Some("Could not check for updates. Try again later.".into()),
                    ..Default::default()
                },
            );
            Err(error)
        }
    }
}
pub fn start(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            let desktop = app.state::<Desktop>();
            let enabled = desktop.inner.lock().await.config.automatic_updates;
            if enabled
                && !cfg!(debug_assertions)
                && desktop.quit.load(std::sync::atomic::Ordering::Acquire) == 0
            {
                let _ = check(&app).await;
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}
#[tauri::command]
pub async fn updates_status(app: tauri::AppHandle) -> Result<Settings, String> {
    let desktop = app.state::<Desktop>();
    let config = &desktop.inner.lock().await.config;
    let status = desktop.updates.status.lock().unwrap().clone();
    Ok(Settings {
        status,
        current_version: app.package_info().version.to_string(),
        automatic: config.automatic_updates,
        prereleases: config
            .update_prereleases
            .unwrap_or(!app.package_info().version.pre.is_empty()),
    })
}
#[tauri::command]
pub async fn updates_preferences(
    app: tauri::AppHandle,
    preferences: Preferences,
) -> Result<(), String> {
    let desktop = app.state::<Desktop>();
    let _guard = desktop
        .updates
        .gate
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let mut inner = desktop.inner.lock().await;
    let mut config = inner.config.clone();
    config.automatic_updates = preferences.automatic;
    config.update_prereleases = Some(preferences.prereleases);
    config
        .save(&desktop.config_file)
        .map_err(|e| e.to_string())?;
    inner.config = config;
    *desktop.updates.pending.lock().await = None;
    publish(&app, Status::default());
    Ok(())
}
#[tauri::command]
pub async fn updates_check(app: tauri::AppHandle) -> Result<(), String> {
    check(&app)
        .await
        .map_err(|_| "Could not check for updates. Try again later.".into())
}
#[tauri::command]
pub async fn updates_install(app: tauri::AppHandle) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let desktop = app.state::<Desktop>();
    let _guard = desktop
        .updates
        .gate
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let update = desktop
        .updates
        .pending
        .lock()
        .await
        .clone()
        .ok_or("Check for an update first")?;
    ensure_running(&desktop).map_err(|e| e.to_string())?;
    publish(
        &app,
        Status {
            phase: "downloading".into(),
            version: Some(update.version.clone()),
            ..Default::default()
        },
    );
    let download = tokio::time::timeout(
        Duration::from_secs(15 * 60),
        update.download(
            |length, total| {
                let mut status = desktop.updates.status.lock().unwrap();
                status.downloaded = status.downloaded.saturating_add(length as u64);
                status.total = total;
            },
            || {},
        ),
    )
    .await;
    let bytes = match download {
        Ok(Ok(bytes)) => bytes,
        _ => {
            publish(
                &app,
                Status {
                    phase: "available".into(),
                    version: Some(update.version.clone()),
                    error: Some(
                        "Download or signature verification failed. Your app was not changed."
                            .into(),
                    ),
                    ..Default::default()
                },
            );
            return Err("Download or signature verification failed".into());
        }
    };
    desktop
        .quit
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "The app is already stopping")?;
    publish(
        &app,
        Status {
            phase: "installing".into(),
            version: Some(update.version.clone()),
            ..Default::default()
        },
    );
    let result: Result<()> = async {
        desktop.stop_hub().await?;
        desktop.quit.store(2, Ordering::Release);
        tokio::task::spawn_blocking(move || update.install(bytes)).await??;
        Ok(())
    }
    .await;
    if result.is_ok() {
        app.restart();
    }
    desktop.quit.store(0, Ordering::Release);
    let _ = desktop_bootstrap(app.state()).await;
    publish(
        &app,
        Status {
            phase: "error".into(),
            error: Some(
                "Could not install the update. Check your system permissions and try again.".into(),
            ),
            ..Default::default()
        },
    );
    Err("Could not install the update".into())
}
fn ensure_running(desktop: &Desktop) -> Result<()> {
    ensure!(
        desktop.quit.load(std::sync::atomic::Ordering::Acquire) == 0,
        "The app is already stopping"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn release_selection_obeys_channel_and_never_downgrades_or_uses_incomplete_assets() {
        let release = |tag, prerelease, assets| json!({"tag_name":tag,"draft":false,"prerelease":prerelease,"assets":assets});
        let asset = json!([{"name":"desktop-updater.json","size":1024}]);
        let releases = json!([
            release("v1.0.1", false, asset.clone()),
            release("v1.1.0-beta.1", true, asset),
            release("v2.0.0", false, json!([])),
            release("not-a-version", false, json!([]))
        ]);
        let current = Version::parse("1.0.0").unwrap();
        assert_eq!(
            select_release(&releases, &current, false)
                .unwrap()
                .to_string(),
            "1.0.1"
        );
        assert_eq!(
            select_release(&releases, &current, true)
                .unwrap()
                .to_string(),
            "1.1.0-beta.1"
        );
        assert!(select_release(&releases, &Version::parse("2.0.0").unwrap(), true).is_none());
        assert_eq!(feed(&current).unwrap().as_str(),"https://github.com/thisnick/agent-wechat/releases/download/v1.0.0/desktop-updater.json");
    }
}
