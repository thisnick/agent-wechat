//! Own the bundled Mini Program host for the lifetime of its window.
use crate::{hub, Desktop};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tauri::{webview::NewWindowResponse, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    connection_id: String,
    account_id: String,
    app_id: String,
    source_message_id: Option<String>,
    title: String,
    account_name: String,
}

#[derive(Deserialize)]
struct Ready {
    version: u8,
    state: String,
    url: reqwest::Url,
}

struct MiniProgramHost {
    child: Child,
    control: Option<ChildStdin>,
    url: reqwest::Url,
}

impl MiniProgramHost {
    async fn launch(binary: &Path, startup: &[u8]) -> Result<Self> {
        let mut command = Command::new(binary);
        command
            .arg("__mini-program-host-v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_remove("AGENT_WECHAT_TOKEN")
            .env_remove("AGENT_WECHAT_TOKEN_FILE")
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW.
        let mut child = command
            .spawn()
            .context("Cannot start the bundled Mini Program host")?;
        // Child::wait closes any stdin handle still owned by the child.
        // The window must retain the control pipe while also watching for exit.
        let mut control = child.stdin.take().context("Missing host input")?;
        let ready = tokio::time::timeout(Duration::from_secs(30), async {
            control.write_all(startup).await?;
            control.write_all(b"\n").await?;
            control.flush().await?;
            let output = child.stdout.take().context("Missing host output")?;
            let mut line = Vec::new();
            BufReader::new(output)
                .take(4096)
                .read_until(b'\n', &mut line)
                .await?;
            let ready: Ready = serde_json::from_slice(&line)
                .context("The Mini Program host could not open this page")?;
            ensure!(
                ready.version == 1
                    && ready.state == "ready"
                    && ready.url.scheme() == "http"
                    && ready.url.host_str() == Some("127.0.0.1")
                    && ready.url.port().is_some()
                    && ready.url.username().is_empty()
                    && ready.url.password().is_none()
                    && ready.url.path() == "/"
                    && ready.url.query().is_none(),
                "Invalid Mini Program host response"
            );
            Ok::<_, anyhow::Error>(ready)
        })
        .await
        .context("The Mini Program host did not start in time")??;
        Ok(Self {
            child,
            control: Some(control),
            url: ready.url,
        })
    }

    async fn close(&mut self) {
        drop(self.control.take());
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

#[tauri::command]
pub async fn desktop_open_mini_program(
    caller: WebviewWindow,
    state: tauri::State<'_, Desktop>,
    request: OpenRequest,
) -> Result<(), String> {
    open(caller, &state, request)
        .await
        .map_err(|error| error.to_string())
}

async fn open(caller: WebviewWindow, state: &Desktop, request: OpenRequest) -> Result<()> {
    use std::sync::atomic::Ordering;
    use tauri::Manager;
    ensure!(
        caller.label() == "main",
        "Mini Programs can only be opened from the main window"
    );
    ensure!(
        state.quit.load(Ordering::Acquire) == 0,
        "AgentWechat is shutting down"
    );
    ensure!(
        !request.title.is_empty()
            && request.title.len() <= 1024
            && !request.title.chars().any(char::is_control)
            && request.account_name.len() <= 1024
            && !request.account_name.chars().any(char::is_control),
        "Invalid Mini Program title"
    );
    let active = state
        .inner
        .lock()
        .await
        .active
        .clone()
        .filter(|active| active.connection.id == request.connection_id)
        .context("This hub is no longer selected")?;
    let startup = Zeroizing::new(serde_json::to_vec(&json!({
        "version": 1,
        "endpoint": active.connection.endpoint,
        "certificate": active.connection.certificate,
        "token": active.key.as_str(),
        "accountId": request.account_id,
        "appId": request.app_id,
        "sourceMessageId": request.source_message_id,
    }))?);
    let host = MiniProgramHost::launch(&hub::executable(&state.binaries, "wx")?, &startup).await?;
    {
        let inner = state.inner.lock().await;
        ensure!(
            state.quit.load(Ordering::Acquire) == 0
                && inner
                    .active
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &active)),
            "This hub is no longer selected"
        );
    }
    let origin = host.url.origin();
    let label = format!("mini-program-{}", uuid::Uuid::new_v4());
    let window = WebviewWindowBuilder::new(
        caller.app_handle(),
        label,
        WebviewUrl::External(host.url.clone()),
    )
    .title(format!(
        "{} · {} · {}",
        request.title,
        request.account_name,
        crate::i18n::text("miniProgramExperimental")
    ))
    .inner_size(410.0, 776.0)
    .min_inner_size(309.0, 560.0)
    .incognito(true)
    .disable_drag_drop_handler()
    .on_download(|_, _| false)
    .on_navigation(move |url| url.origin() == origin)
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .build()
    .context("Cannot open the Mini Program window")?;
    let closed = CancellationToken::new();
    let signal = closed.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            signal.cancel();
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut host = host;
        tokio::select! {
            _ = closed.cancelled() => host.close().await,
            _ = host.child.wait() => { let _ = window.destroy(); }
        }
    });
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn watching_the_child_keeps_the_host_control_pipe_open() {
        let scratch = tempfile::tempdir().unwrap();
        let executable = scratch.path().join("mini-host-fixture");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
IFS= read -r startup || exit 1
printf '%s\n' '{"version":1,"state":"ready","url":"http://127.0.0.1:12345/"}'
IFS= read -r control
exit 0
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut host = MiniProgramHost::launch(&executable, b"{}").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), host.child.wait())
                .await
                .is_err()
        );
        host.close().await;
        assert!(host.child.try_wait().unwrap().unwrap().success());
    }
}
