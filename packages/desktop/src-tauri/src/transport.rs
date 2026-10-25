use crate::Desktop;
use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tauri::{ipc::Channel, State};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

pub struct Control {
    pub cancel: CancellationToken,
    pub credits: std::sync::Arc<tokio::sync::Semaphore>,
}
pub struct Upload {
    pub temp: tempfile::NamedTempFile,
    pub file: tokio::fs::File,
    length: usize,
}
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Packet {
    Data { bytes: String },
    End,
    Error,
}
#[derive(Serialize)]
pub struct Head {
    status: u16,
    headers: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    pub connection_id: String,
    pub path: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: bool,
}
fn request_id(id: &str) -> Result<()> {
    uuid::Uuid::parse_str(id)?;
    Ok(())
}
fn request_path(path: &str) -> Result<()> {
    if !path.starts_with("/v1/") && path != "/health" {
        bail!("Only hub API requests are allowed");
    }
    if path.contains(['\\', '#', '\r', '\n']) || path.len() > 16384 {
        bail!("Invalid API path");
    }
    let decoded = path
        .split('?')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if decoded.split('/').any(|part| part == "." || part == "..")
        || decoded.contains("%2e")
        || decoded.contains("%2f")
        || decoded.contains("%5c")
    {
        bail!("Invalid API path");
    }
    Ok(())
}
#[tauri::command]
pub async fn http_body_begin(state: State<'_, Desktop>, id: String) -> Result<(), String> {
    request_id(&id).map_err(|_| "Invalid request identity")?;
    let mut uploads = state.uploads.lock().await;
    if uploads.len() >= 8 || uploads.contains_key(&id) {
        return Err("Too many active uploads".into());
    }
    let directory = state.data.parent().unwrap().join("transfers");
    wechat_platform::private_dir(&directory).map_err(|_| "Cannot create the transfer folder")?;
    let temp = wechat_platform::private_tempfile_in(&directory, ".upload-")
        .map_err(|_| "Cannot prepare upload")?;
    let file = tokio::fs::File::from_std(
        temp.as_file()
            .try_clone()
            .map_err(|_| "Cannot prepare upload")?,
    );
    uploads.insert(
        id,
        Upload {
            temp,
            file,
            length: 0,
        },
    );
    Ok(())
}
#[tauri::command]
pub async fn http_body_chunk(
    state: State<'_, Desktop>,
    id: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    if bytes.len() > 64 * 1024 {
        return Err("Upload chunk is too large".into());
    }
    let mut uploads = state.uploads.lock().await;
    let upload = uploads.get_mut(&id).ok_or("Upload was cancelled")?;
    if upload.length + bytes.len() > 513 * 1024 * 1024 {
        return Err("Upload is too large".into());
    }
    upload
        .file
        .write_all(&bytes)
        .await
        .map_err(|_| "Cannot store upload")?;
    upload.length += bytes.len();
    Ok(())
}
#[tauri::command]
pub async fn http_cancel(state: State<'_, Desktop>, id: String) -> Result<(), String> {
    state.uploads.lock().await.remove(&id);
    if let Some(control) = state.requests.lock().await.remove(&id) {
        control.cancel.cancel();
    }
    Ok(())
}
#[tauri::command]
pub async fn http_ack(state: State<'_, Desktop>, id: String) -> Result<(), String> {
    if let Some(control) = state.requests.lock().await.get(&id) {
        if control.credits.available_permits() < 8 {
            control.credits.add_permits(1);
        }
    }
    Ok(())
}
#[tauri::command]
pub async fn http_request(
    state: State<'_, Desktop>,
    request: Request,
    channel: Channel<Packet>,
) -> Result<Head, String> {
    request_id(&request.id).map_err(|_| "Invalid request identity")?;
    request_path(&request.path).map_err(|_| "Invalid API path")?;
    if !matches!(
        request.method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"
    ) {
        return Err("Invalid API method".into());
    }
    let active = {
        let inner = state.inner.lock().await;
        inner
            .active
            .clone()
            .filter(|active| active.connection.id == request.connection_id)
            .ok_or("Reconnect to the selected hub")?
    };
    let base =
        crate::config::endpoint(&active.connection.endpoint).map_err(|_| "Invalid hub address")?;
    let url = base.join(&request.path).map_err(|_| "Invalid API path")?;
    if url.origin() != base.origin() {
        return Err("Invalid API origin".into());
    }
    let mut builder = active
        .http
        .request(
            reqwest::Method::from_bytes(request.method.as_bytes()).unwrap(),
            url,
        )
        .bearer_auth(active.key.as_str());
    for (name, value) in &request.headers {
        if [
            "content-type",
            "range",
            "if-none-match",
            "idempotency-key",
            "accept",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
        {
            builder = builder.header(name, value);
        }
    }
    let mut uploaded = state.uploads.lock().await.remove(&request.id);
    if request.body {
        let upload = uploaded.as_mut().ok_or("Upload was cancelled")?;
        upload
            .file
            .flush()
            .await
            .map_err(|_| "Cannot prepare upload")?;
        upload
            .file
            .rewind()
            .await
            .map_err(|_| "Cannot prepare upload")?;
        let file = upload
            .file
            .try_clone()
            .await
            .map_err(|_| "Cannot prepare upload")?;
        builder = builder.body(reqwest::Body::wrap_stream(ReaderStream::new(file)));
    } else if uploaded.is_some() {
        return Err("Unexpected request body".into());
    }
    let cancel = CancellationToken::new();
    let credits = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    {
        let mut requests = state.requests.lock().await;
        if requests.len() >= 64 || requests.contains_key(&request.id) {
            return Err("Too many active requests".into());
        }
        requests.insert(
            request.id.clone(),
            Control {
                cancel: cancel.clone(),
                credits: credits.clone(),
            },
        );
    }
    let response = tokio::select! {
        _ = cancel.cancelled() => Err("Request cancelled".to_string()),
        response = builder.send() => response.map_err(|_| "Cannot reach the hub; check its connection and certificate".to_string()),
    };
    let mut response = match response {
        Ok(value) => value,
        Err(error) => {
            state.requests.lock().await.remove(&request.id);
            return Err(error);
        }
    };
    let head = Head {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect(),
    };
    let requests = state.requests.clone();
    tokio::spawn(async move {
        // Keep the upload file until the HTTP request finishes on every platform.
        let _upload_guard = uploaded.take().map(|upload| upload.temp);
        loop {
            let next = tokio::select! { _ = cancel.cancelled() => break, chunk = response.chunk() => chunk };
            match next {
                Ok(Some(bytes)) => {
                    for part in bytes.chunks(64 * 1024) {
                        let permit = tokio::select! { _ = cancel.cancelled() => break, permit = credits.acquire() => permit };
                        let Ok(permit) = permit else {
                            break;
                        };
                        permit.forget();
                        if channel
                            .send(Packet::Data {
                                bytes: STANDARD.encode(part),
                            })
                            .is_err()
                        {
                            cancel.cancel();
                            break;
                        }
                    }
                }
                Ok(None) => {
                    let _ = channel.send(Packet::End);
                    break;
                }
                Err(_) => {
                    let _ = channel.send(Packet::Error);
                    break;
                }
            }
        }
        requests.lock().await.remove(&request.id);
    });
    Ok(head)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_cannot_escape_to_another_origin_or_resource() {
        for value in [
            "https://other/v1/status",
            "//other/v1/status",
            "/v1/../secret",
            "/v1/%2e%2e/secret",
            "/v1/../../health",
            "/v1/a\\b",
            "/secrets",
        ] {
            assert!(request_path(value).is_err());
        }
        for value in [
            "/v1/status",
            "/v1/accounts/a/events/stream?after=0",
            "/health",
        ] {
            assert!(request_path(value).is_ok());
        }
    }
}
