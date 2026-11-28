use crate::Desktop;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap};
use tauri::{http::HeaderValue, ipc::Channel, State};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

const STREAM_CHUNK_BYTES: usize = 128 * 1024;
const STREAM_CREDITS: usize = 8;
const MAX_ACTIVE_REQUESTS: usize = 64;
const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
const FRAME_DATA: u8 = 0;
const FRAME_END: u8 = 1;
const FRAME_ERROR: u8 = 2;

fn data_frame(part: &[u8]) -> tauri::ipc::InvokeResponseBody {
    let mut frame = Vec::with_capacity(1 + part.len());
    frame.push(FRAME_DATA);
    frame.extend_from_slice(part);
    tauri::ipc::InvokeResponseBody::Raw(frame)
}

fn control_frame(tag: u8) -> tauri::ipc::InvokeResponseBody {
    tauri::ipc::InvokeResponseBody::Raw(vec![tag])
}

pub struct Control {
    pub cancel: CancellationToken,
    pub credits: std::sync::Arc<tokio::sync::Semaphore>,
}
pub struct Upload {
    pub temp: tempfile::NamedTempFile,
    pub file: tokio::fs::File,
    length: usize,
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
fn forward_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &BTreeMap<String, String>,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if [
            "content-type",
            "range",
            "if-match",
            "if-none-match",
            "idempotency-key",
            "accept",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
        {
            builder = builder.header(name, value);
        }
    }
    builder
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
fn parse_upload_chunk<'a>(
    id_header: Option<&'a HeaderValue>,
    body: &'a tauri::ipc::InvokeBody,
) -> Result<(&'a str, Cow<'a, [u8]>), String> {
    let id = id_header
        .and_then(|value| value.to_str().ok())
        .ok_or("Invalid request identity")?;
    request_id(id).map_err(|_| "Invalid request identity")?;
    let bytes = match body {
        tauri::ipc::InvokeBody::Raw(bytes) => {
            if bytes.len() > UPLOAD_CHUNK_BYTES {
                return Err("Upload chunk is too large".into());
            }
            Cow::Borrowed(bytes.as_slice())
        }
        // Tauri's postMessage fallback JSON-encodes a Uint8Array payload as a byte array.
        tauri::ipc::InvokeBody::Json(serde_json::Value::Array(items)) => {
            if items.len() > UPLOAD_CHUNK_BYTES {
                return Err("Upload chunk is too large".into());
            }
            let mut decoded = Vec::with_capacity(items.len());
            for item in items {
                let byte = item
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or("Binary upload chunk is required")?;
                decoded.push(byte);
            }
            Cow::Owned(decoded)
        }
        tauri::ipc::InvokeBody::Json(_) => return Err("Binary upload chunk is required".into()),
    };
    Ok((id, bytes))
}
#[tauri::command]
pub async fn http_body_chunk(
    state: State<'_, Desktop>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let (id, bytes) = parse_upload_chunk(request.headers().get("x-upload-id"), request.body())?;
    let mut uploads = state.uploads.lock().await;
    let upload = uploads.get_mut(id).ok_or("Upload was cancelled")?;
    if upload.length + bytes.len() > 513 * 1024 * 1024 {
        return Err("Upload is too large".into());
    }
    upload
        .file
        .write_all(bytes.as_ref())
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
        if control.credits.available_permits() < STREAM_CREDITS {
            control.credits.add_permits(1);
        }
    }
    Ok(())
}
#[tauri::command]
pub async fn http_request(
    state: State<'_, Desktop>,
    request: Request,
    channel: Channel<tauri::ipc::InvokeResponseBody>,
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
    let builder = active
        .http
        .request(
            reqwest::Method::from_bytes(request.method.as_bytes()).unwrap(),
            url,
        )
        .bearer_auth(active.key.as_str());
    let mut builder = forward_headers(builder, &request.headers);
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
    let credits = std::sync::Arc::new(tokio::sync::Semaphore::new(STREAM_CREDITS));
    {
        let mut requests = state.requests.lock().await;
        if requests.len() >= MAX_ACTIVE_REQUESTS || requests.contains_key(&request.id) {
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
        _ = cancel.cancelled() => Err("Request cancelled"),
        response = builder.send() => {
            response.map_err(|_| "Cannot reach the hub; check its connection and certificate")
        }
    };
    let mut response = match response {
        Ok(value) => value,
        Err(error) => {
            state.requests.lock().await.remove(&request.id);
            return Err(error.into());
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
                    for part in bytes.chunks(STREAM_CHUNK_BYTES) {
                        let permit = tokio::select! { _ = cancel.cancelled() => break, permit = credits.acquire() => permit };
                        let Ok(permit) = permit else {
                            break;
                        };
                        permit.forget();
                        if channel.send(data_frame(part)).is_err() {
                            cancel.cancel();
                            break;
                        }
                    }
                }
                Ok(None) => {
                    let _ = channel.send(control_frame(FRAME_END));
                    break;
                }
                Err(_) => {
                    let _ = channel.send(control_frame(FRAME_ERROR));
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

    const UPLOAD_ID: &str = "c4bea3cb-06bc-4e78-b4ec-608ece103b2a";

    #[test]
    fn data_frames_prefix_the_binary_payload() {
        let tauri::ipc::InvokeResponseBody::Raw(bytes) = data_frame(&[0, 128, 255]) else {
            panic!("expected a raw data frame");
        };
        assert_eq!(bytes, [0, 0, 128, 255]);
    }

    #[test]
    fn control_frames_contain_only_their_tag() {
        for (tag, expected) in [(FRAME_END, [1]), (FRAME_ERROR, [2])] {
            let tauri::ipc::InvokeResponseBody::Raw(bytes) = control_frame(tag) else {
                panic!("expected a raw control frame");
            };
            assert_eq!(bytes, expected);
        }
    }

    #[test]
    fn upload_chunk_accepts_raw_bytes() {
        let header = HeaderValue::from_static(UPLOAD_ID);
        let body = tauri::ipc::InvokeBody::Raw(vec![0, 128, 255]);
        let (id, bytes) = parse_upload_chunk(Some(&header), &body).unwrap();
        assert_eq!(id, UPLOAD_ID);
        assert_eq!(bytes.as_ref(), &[0, 128, 255][..]);
    }

    #[test]
    fn upload_chunk_rejects_missing_or_invalid_upload_ids() {
        let invalid_uuid = HeaderValue::from_static("not-a-uuid");
        let invalid_utf8 = HeaderValue::from_bytes(&[0xff]).unwrap();
        let body = tauri::ipc::InvokeBody::Raw(vec![0]);
        for header in [None, Some(&invalid_uuid), Some(&invalid_utf8)] {
            assert_eq!(
                parse_upload_chunk(header, &body).unwrap_err(),
                "Invalid request identity"
            );
        }
    }

    #[test]
    fn upload_chunk_accepts_postmessage_fallback_json_byte_array() {
        let header = HeaderValue::from_static(UPLOAD_ID);
        let body = tauri::ipc::InvokeBody::Json(serde_json::json!([0, 128, 255]));
        let (id, bytes) = parse_upload_chunk(Some(&header), &body).unwrap();
        assert_eq!(id, UPLOAD_ID);
        assert_eq!(bytes.as_ref(), &[0, 128, 255][..]);

        let oversized = tauri::ipc::InvokeBody::Json(serde_json::Value::Array(vec![
            serde_json::Value::from(0);
            UPLOAD_CHUNK_BYTES + 1
        ]));
        assert_eq!(
            parse_upload_chunk(Some(&header), &oversized).unwrap_err(),
            "Upload chunk is too large"
        );
    }

    #[test]
    fn upload_chunk_rejects_invalid_json() {
        let header = HeaderValue::from_static(UPLOAD_ID);
        for body in [
            tauri::ipc::InvokeBody::Json(serde_json::json!({ "bytes": [0, 1] })),
            tauri::ipc::InvokeBody::Json(serde_json::json!([256])),
            tauri::ipc::InvokeBody::Json(serde_json::json!([-1])),
            tauri::ipc::InvokeBody::Json(serde_json::json!(["0"])),
        ] {
            assert_eq!(
                parse_upload_chunk(Some(&header), &body).unwrap_err(),
                "Binary upload chunk is required"
            );
        }
    }

    #[test]
    fn upload_chunk_accepts_empty_and_max_sized_raw_bytes() {
        let header = HeaderValue::from_static(UPLOAD_ID);
        let empty = tauri::ipc::InvokeBody::Raw(Vec::new());
        let (id, bytes) = parse_upload_chunk(Some(&header), &empty).unwrap();
        assert_eq!(id, UPLOAD_ID);
        assert!(bytes.is_empty());
        let max = vec![0x5a; UPLOAD_CHUNK_BYTES];
        let body = tauri::ipc::InvokeBody::Raw(max.clone());
        let (id, bytes) = parse_upload_chunk(Some(&header), &body).unwrap();
        assert_eq!(id, UPLOAD_ID);
        assert_eq!(bytes.as_ref(), max.as_slice());
    }

    #[test]
    fn upload_chunk_rejects_oversized_raw_bytes() {
        let header = HeaderValue::from_static(UPLOAD_ID);
        let body = tauri::ipc::InvokeBody::Raw(vec![0; UPLOAD_CHUNK_BYTES + 1]);
        assert_eq!(
            parse_upload_chunk(Some(&header), &body).unwrap_err(),
            "Upload chunk is too large"
        );
    }

    #[test]
    fn conditional_sticker_mutations_keep_revision_and_native_credentials() {
        for method in [reqwest::Method::PATCH, reqwest::Method::DELETE] {
            for name in ["if-match", "If-Match"] {
                let revision = format!("\"{}\"", "a".repeat(64));
                let headers = BTreeMap::from([
                    (name.into(), revision.clone()),
                    ("Idempotency-Key".into(), "saved-intent".into()),
                    ("Authorization".into(), "Bearer webview-placeholder".into()),
                    ("Host".into(), "untrusted.example".into()),
                ]);
                let request = forward_headers(
                    reqwest::Client::new()
                        .request(
                            method.clone(),
                            "http://127.0.0.1/v1/accounts/a/custom-stickers/s",
                        )
                        .bearer_auth("native-key"),
                    &headers,
                )
                .build()
                .unwrap();
                assert_eq!(request.headers()["if-match"], revision);
                assert_eq!(request.headers()["idempotency-key"], "saved-intent");
                assert_eq!(request.headers()["authorization"], "Bearer native-key");
                assert!(!request.headers().contains_key("host"));
            }
        }
    }
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
