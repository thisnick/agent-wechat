//! Bounded owned uploads and account-scoped media jobs and bytes.
use super::*;
use axum::extract::Multipart;
use wechat_core::{MediaSendRequest, MediaTransferRequest, MAX_SOURCE_BYTES};

fn validate_upload_chunk(current: usize, next: usize) -> ApiResult<()> {
    if current
        .checked_add(next)
        .is_none_or(|n| n > MAX_SOURCE_BYTES)
    {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "upload_too_large"));
    }
    Ok(())
}
/// One message from the request task: a body chunk, the end of the body
/// (`Ok(None)`), or why it stopped.
type UploadPiece = std::io::Result<Option<axum::body::Bytes>>;
/// Blocking reader over upload chunks sent from the request task, so the
/// encrypting writer consumes the body as it arrives. Only an explicit end
/// marker ends the body: a request dropped mid-upload closes the channel
/// without one and fails the upload instead of storing a truncated file.
struct ChunkReader {
    chunks: tokio::sync::mpsc::Receiver<UploadPiece>,
    current: axum::body::Bytes,
    ended: bool,
}
impl std::io::Read for ChunkReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            if self.ended {
                return Ok(0);
            }
            match self.chunks.blocking_recv() {
                Some(Ok(Some(chunk))) => self.current = chunk,
                Some(Ok(None)) => self.ended = true,
                Some(Err(error)) => return Err(error),
                None => return Err(std::io::Error::other("upload_interrupted")),
            }
        }
        let count = buffer.len().min(self.current.len());
        buffer[..count].copy_from_slice(&self.current[..count]);
        self.current = self.current.slice(count..);
        Ok(count)
    }
}

pub(super) fn routes() -> Router<App> {
    Router::new()
        .api_route(
            "/v1/accounts/{a}/uploads",
            post(upload).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES + 64 * 1024)),
        )
        .api_route(
            "/v1/accounts/{a}/uploads/{id}",
            axum::routing::delete(release_upload),
        )
        .api_route("/v1/accounts/{a}/messages/{id}/media", get(assets))
        .api_route(
            "/v1/accounts/{a}/media/lookup",
            post(assets_batch).layer(DefaultBodyLimit::max(384 * 1024)),
        )
        .api_route(
            "/v1/accounts/{a}/messages/{id}/media-transfers",
            post(transfer),
        )
        .api_route("/v1/accounts/{a}/media/{id}", get(asset))
        .api_route("/v1/accounts/{a}/media/{id}/content", get(content))
        .api_route("/v1/accounts/{a}/jobs/{id}", get(job))
        .api_route("/v1/accounts/{a}/jobs/{id}/cancel", post(cancel_job))
        .api_route("/v1/accounts/{a}/jobs/{id}/retry", post(retry_job))
        .api_route(
            "/v1/accounts/{a}/jobs/{id}/retry-completed-upload",
            post(retry_completed_upload),
        )
        .api_route(
            "/v1/accounts/{a}/media-writeback/repair",
            post(repair_writeback),
        )
}
async fn repair_writeback(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<contracts::NativeMediaWritebackRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "media")?;
    if !(1..=100).contains(&input.limit) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_media_repair_limit",
        ));
    }
    let operation = app.core.repair_media_writeback_operation(&a, input.limit)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(operation).map_err(anyhow::Error::from)?),
    ))
}
async fn upload(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    mut form: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "send")?;
    app.core.store.account(&a)?;
    let mut file: Option<wechat_store::OwnedMediaSource> = None;
    while let Some(mut field) = form
        .next_field()
        .await
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_upload"))?
    {
        if file.is_some() || field.name() != Some("file") {
            if let Some(stored) = file.take() {
                let core = app.core.clone();
                let account = a.clone();
                let id: String = stored.id;
                let _ = work(move || core.store.delete_media_source(&account, &id)).await;
            }
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_upload"));
        }
        let name = field.file_name().unwrap_or("attachment").to_owned();
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_owned();
        // The body is encrypted to disk as it arrives; it is never buffered whole.
        let (sender, chunks) = tokio::sync::mpsc::channel(4);
        let core = app.core.clone();
        let account = a.clone();
        let store = work(move || {
            let mut reader = ChunkReader {
                chunks,
                current: axum::body::Bytes::new(),
                ended: false,
            };
            core.upload_source_stream(&account, &name, &mime, &mut reader)
        });
        let pump = async move {
            let mut total = 0usize;
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if let Err(error) = validate_upload_chunk(total, chunk.len()) {
                            let _ = sender
                                .send(Err(std::io::Error::other("upload_too_large")))
                                .await;
                            return Err(error);
                        }
                        total += chunk.len();
                        // A closed channel means the writer stopped; its error is reported.
                        if sender.send(Ok(Some(chunk))).await.is_err() {
                            return Ok(());
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Ok(None)).await;
                        return Ok(());
                    }
                    Err(_) => {
                        let _ = sender
                            .send(Err(std::io::Error::other("upload_interrupted")))
                            .await;
                        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_upload"));
                    }
                }
            }
        };
        let (pumped, stored) = tokio::join!(pump, store);
        if let Err(error) = pumped {
            // The writer saw the failure too; never keep a stored body anyway.
            if let Ok(source) = stored {
                let core = app.core.clone();
                let account = a.clone();
                let _ = work(move || core.store.delete_media_source(&account, &source.id)).await;
            }
            return Err(error);
        }
        file = Some(stored?);
    }
    let source = file.ok_or(ApiError(StatusCode::BAD_REQUEST, "file_required"))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(source).map_err(anyhow::Error::from)?),
    ))
}
/// Delete an upload the caller has finished with, such as the source of a
/// sticker that has been added. Uploads a pending send or preparation still
/// needs are kept (409 `upload_in_use`).
async fn release_upload(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<crate::contracts::NativeUploadRelease>> {
    c.require(Some(&a), "send")?;
    let released = work(move || app.core.release_upload(&a, &id)).await?;
    Ok(Json(crate::contracts::NativeUploadRelease { released }))
}
async fn assets(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")?;
    let mut rows = app.core.media_assets_concurrent(&a, vec![id]).await?;
    let assets = rows.pop().context("media_result_missing")?.1?;
    Ok(Json(json!({"items": assets})))
}

async fn assets_batch(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<contracts::NativeMediaBatchRequest>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")?;
    if input.message_ids.is_empty()
        || input.message_ids.len() > 40
        || input
            .message_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 8192)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let rows = app
        .core
        .media_assets_concurrent(&a, input.message_ids)
        .await?;
    let items = rows
        .into_iter()
        .map(|(message_id, result)| match result {
            Ok(assets) => contracts::NativeMediaBatchItem {
                message_id,
                assets,
                error: None,
            },
            Err(error) => {
                let ApiError(_, code) = ApiError::from(error);
                contracts::NativeMediaBatchItem {
                    message_id,
                    assets: Vec::new(),
                    error: Some(code.into()),
                }
            }
        })
        .collect();
    Ok(Json(
        serde_json::to_value(contracts::NativeMediaBatch { items }).map_err(anyhow::Error::from)?,
    ))
}
async fn transfer(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Json(input): Json<MediaTransferRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "media")?;
    if id != input.message_id {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "message_identity_mismatch",
        ));
    }
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.start_media_transfer(&a, input)
    })
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(job).map_err(anyhow::Error::from)?),
    ))
}
async fn asset(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")?;
    Ok(Json(
        serde_json::to_value(
            app.core
                .read(&a.clone(), move |core| core.media_asset(&a, &id))
                .await
                .map_err(ApiError::from)?,
        )
        .map_err(anyhow::Error::from)?,
    ))
}
async fn job(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")
        .or_else(|_| c.require(Some(&a), "send"))?;
    let job = app.core.media_job(&a, &id)?;
    c.require(
        Some(&a),
        if matches!(
            job.kind.as_str(),
            "send" | "sticker_saved" | "sticker_pack" | "sticker_send"
        ) {
            "send"
        } else {
            "media"
        },
    )?;
    Ok(Json(
        serde_json::to_value(job).map_err(anyhow::Error::from)?,
    ))
}
async fn cancel_job(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")
        .or_else(|_| c.require(Some(&a), "send"))?;
    let job = app.core.media_job(&a, &id)?;
    c.require(
        Some(&a),
        if matches!(
            job.kind.as_str(),
            "send" | "sticker_saved" | "sticker_pack" | "sticker_send"
        ) {
            "send"
        } else {
            "media"
        },
    )?;
    Ok(Json(
        serde_json::to_value(app.core.cancel_media_job(&a, &id)?).map_err(anyhow::Error::from)?,
    ))
}
async fn retry_job(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    request: Option<Json<contracts::NativeMediaJobRetryRequest>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "media")
        .or_else(|_| c.require(Some(&a), "send"))?;
    let job = app.core.media_job(&a, &id)?;
    c.require(
        Some(&a),
        if matches!(
            job.kind.as_str(),
            "send" | "sticker_saved" | "sticker_pack" | "sticker_send"
        ) {
            "send"
        } else {
            "media"
        },
    )?;
    let reuse = request
        .and_then(|Json(request)| request.reuse_completed_upload)
        .unwrap_or(false);
    if reuse {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "completed_upload_retry_route_required",
        ));
    }
    let retried = app.core.retry_media_job(&a, &id)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(retried).map_err(anyhow::Error::from)?),
    ))
}
async fn retry_completed_upload(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Json(request): Json<contracts::NativeMediaJobRetryRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "send")?;
    if request.reuse_completed_upload != Some(true) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "completed_upload_retry_opt_in_required",
        ));
    }
    let retried = app
        .core
        .retry_media_job_reuse_completed_upload(&a, &id)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(retried).map_err(anyhow::Error::from)?),
    ))
}
async fn content(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Query(query): Query<ContentQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    if matches!(query.representation, ContentRepresentation::Preview) {
        // A still preview is decoded when asked for and is small.
        let (media, timing) = app
            .core
            .read_timed(&a.clone(), move |core| core.media_preview_content(&a, &id))
            .await
            .map_err(ApiError::from)?;
        return Ok(timed_response(content_response(media, &headers)?, timing));
    }
    let wanted = ContentRequest::new(&headers);
    let (media, timing) = app
        .core
        .read_timed(&a.clone(), move |core| {
            core.media_content_stream(&a, &id, |total, sha256| wanted.range(total, sha256))
        })
        .await
        .map_err(ApiError::from)?;
    Ok(timed_response(stream_response(media, &headers)?, timing))
}
/// The conditional and range headers of a content request, kept so the range
/// can be chosen once the attachment's length and digest are known.
struct ContentRequest {
    range: Option<String>,
    if_range: Option<String>,
    if_none_match: Option<String>,
}
impl ContentRequest {
    fn new(headers: &HeaderMap) -> Self {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        Self {
            range: text("range"),
            if_range: text("if-range"),
            if_none_match: text("if-none-match"),
        }
    }
    /// The inclusive byte range to read, or `None` for the whole attachment.
    /// Nothing is read for a request answered with 304 or 416.
    fn range(&self, total: u64, sha256: &str) -> Option<(u64, u64)> {
        let etag = format!("\"{sha256}\"");
        if self.if_none_match.as_deref() == Some(etag.as_str()) {
            return None;
        }
        if self.if_range.as_deref().is_some_and(|value| value != etag) {
            return None;
        }
        let (start, end) = byte_range(self.range.as_deref()?, usize::try_from(total).ok()?)?;
        Some((start as u64, end as u64))
    }
}
/// A content response whose body is read from `media` as it is sent. Headers
/// and status codes are the same as for a response built from bytes.
fn stream_response(
    media: wechat_core::media_types::MediaContentStream,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let total = media.byte_length;
    let etag = format!("\"{}\"", media.sha256);
    let response = Response::builder()
        .header("cache-control", "private, no-store")
        .header("accept-ranges", "bytes")
        .header("etag", &etag);
    if headers
        .get("if-none-match")
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return Ok(response
            .status(StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .expect("valid media response"));
    }
    let requested = headers.get("range").is_some()
        && headers
            .get("if-range")
            .is_none_or(|v| v.as_bytes() == etag.as_bytes());
    let (status, length, response) = match media.range {
        Some((start, end)) => (
            StatusCode::PARTIAL_CONTENT,
            end - start + 1,
            response.header("content-range", format!("bytes {start}-{end}/{total}")),
        ),
        None if requested => {
            return Ok(response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("content-range", format!("bytes */{total}"))
                .body(axum::body::Body::empty())
                .expect("valid range response"));
        }
        None => (StatusCode::OK, total, response),
    };
    // MIME is validated when bytes are cached, and never taken from a URL.
    response
        .header("content-type", media.content_type)
        .header("content-length", length)
        .status(status)
        .body(streaming_body(media.body))
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "invalid_media_response"))
}
/// A response body read from `body` on a blocking thread, at most a few steps
/// ahead of the client. A read error ends the body with an error, which cuts
/// the connection, so a client never gets a complete response with bytes that
/// failed a check.
fn streaming_body(mut body: Box<dyn std::io::Read + Send>) -> axum::body::Body {
    const STEP: usize = 256 * 1024;
    let (sender, mut receiver) =
        tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(4);
    tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0u8; STEP];
        loop {
            let piece = match body.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => Ok(axum::body::Bytes::copy_from_slice(&buffer[..read])),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => Err(error),
            };
            let failed = piece.is_err();
            // A closed channel means the client went away.
            if sender.blocking_send(piece).is_err() || failed {
                break;
            }
        }
    });
    axum::body::Body::from_stream(async_stream::stream! {
        while let Some(piece) = receiver.recv().await {
            yield piece;
        }
    })
}
fn content_response(
    media: wechat_core::media_types::MediaContent,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let bytes = axum::body::Bytes::from(media.bytes);
    let total = bytes.len();
    let etag = format!("\"{}\"", media.sha256);
    let mut response = Response::builder()
        .header("cache-control", "private, no-store")
        .header("accept-ranges", "bytes")
        .header("etag", &etag);
    if let Some(source_sha256) = media.source_sha256 {
        response = response.header("x-source-sha256", source_sha256);
    }
    if headers
        .get("if-none-match")
        .is_some_and(|v| v.as_bytes() == etag.as_bytes())
    {
        return Ok(response
            .status(StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .expect("valid media response"));
    }
    let range = headers.get("range").filter(|_| {
        headers
            .get("if-range")
            .is_none_or(|v| v.as_bytes() == etag.as_bytes())
    });
    let (status, body) = if let Some(value) = range {
        let selected = value.to_str().ok().and_then(|r| byte_range(r, total));
        let Some((start, end)) = selected else {
            return Ok(response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("content-range", format!("bytes */{total}"))
                .body(axum::body::Body::empty())
                .expect("valid range response"));
        };
        response = response.header("content-range", format!("bytes {start}-{end}/{total}"));
        (StatusCode::PARTIAL_CONTENT, bytes.slice(start..=end))
    } else {
        (StatusCode::OK, bytes)
    };
    // MIME is validated when bytes are cached, and never taken from a URL.
    response = response
        .header("content-type", media.content_type)
        .header("content-length", body.len());
    response
        .status(status)
        .body(axum::body::Body::from(body))
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "invalid_media_response"))
}
/// Which bytes of a media asset to return.
#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ContentRepresentation {
    /// The file itself.
    #[default]
    Original,
    /// The saved preview of the file.
    Preview,
}
/// Options for fetching media bytes.
#[derive(Deserialize, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ContentQuery {
    /// The file itself or its preview. Defaults to the original.
    #[serde(default)]
    representation: ContentRepresentation,
}
fn byte_range(value: &str, total: usize) -> Option<(usize, usize)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if total == 0 || end.contains(',') {
        return None;
    }
    if start.is_empty() {
        let suffix = end.parse::<usize>().ok()?;
        return (suffix > 0).then(|| (total.saturating_sub(suffix), total - 1));
    }
    let start = start.parse::<usize>().ok()?;
    let end = if end.is_empty() {
        total - 1
    } else {
        end.parse::<usize>().ok()?.min(total - 1)
    };
    (start < total && end >= start).then_some((start, end))
}
pub(super) async fn send(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "send")?;
    let key = super::idempotency_key(&headers)?;
    if input.get("quote").is_some() && input.get("kind").is_some_and(|v| v != "text") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    if input.get("kind").is_some_and(|v| v == "share") {
        let request: wechat_core::ShareSendRequest = serde_json::from_value(input)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
        request
            .validate()
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_share_request"))?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(
                serde_json::to_value(app.core.send_share(&a, key, request).await?)
                    .map_err(anyhow::Error::from)?,
            ),
        ));
    }
    if input.get("kind").is_some_and(|v| v == "sticker") {
        let request: wechat_core::StickerSendRequest = serde_json::from_value(input)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
        let core = app.core.clone();
        let key = key.to_string();
        let guard = core.lock_account(&a).await?;
        let job = super::work(move || {
            let _guard = guard;
            core.send_sticker(&a, &key, request)
        })
        .await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(job).map_err(anyhow::Error::from)?),
        ));
    }
    if input.get("kind").is_some_and(|v| v != "text") {
        let request: MediaSendRequest = serde_json::from_value(input)
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(
                serde_json::to_value(app.core.send_media(&a, key, request)?)
                    .map_err(anyhow::Error::from)?,
            ),
        ));
    }
    let mut input = input;
    if let Some(object) = input.as_object_mut() {
        object.remove("kind");
    }
    let request: TextSend = serde_json::from_value(input)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    if let Some(mentions) = &request.mentions {
        mentions
            .validate()
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_mentions"))?;
        if !request.thread_id.ends_with("@chatroom") {
            return Err(ApiError(StatusCode::BAD_REQUEST, "mentions_require_group"));
        }
    }
    let send = if let Some(reference) = request.quote {
        c.require(Some(&a), "read")?;
        app.core
            .send_quoted_text_with_mentions(
                &a,
                key,
                &request.thread_id,
                &request.text,
                reference,
                request.mentions,
            )
            .await?
    } else if let Some(mentions) = request.mentions {
        app.core
            .send_mentioned_text(&a, key, &request.thread_id, &request.text, mentions)
            .await?
    } else {
        app.core
            .send_text(&a, key, &request.thread_id, &request.text)
            .await?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(send).map_err(anyhow::Error::from)?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    #[test]
    fn upload_body_ends_only_at_the_end_marker() {
        use std::io::Read;
        let reader = |pieces: Vec<UploadPiece>| {
            let (sender, chunks) = tokio::sync::mpsc::channel(pieces.len().max(1));
            for piece in pieces {
                sender.try_send(piece).unwrap();
            }
            ChunkReader {
                chunks,
                current: axum::body::Bytes::new(),
                ended: false,
            }
        };
        let mut body = Vec::new();
        reader(vec![
            Ok(Some(axum::body::Bytes::from_static(b"GIF"))),
            Ok(Some(axum::body::Bytes::from_static(b"89a"))),
            Ok(None),
        ])
        .read_to_end(&mut body)
        .unwrap();
        assert_eq!(body, b"GIF89a");
        // A request dropped mid-body closes the channel without the marker.
        let error = reader(vec![Ok(Some(axum::body::Bytes::from_static(b"GIF")))])
            .read_to_end(&mut Vec::new())
            .expect_err("a truncated body must not read as complete");
        assert_eq!(error.to_string(), "upload_interrupted");
    }
    #[test]
    fn completed_upload_reuse_requires_the_named_boolean_opt_in() {
        use contracts::NativeMediaJobRetryRequest;
        let default: NativeMediaJobRetryRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(default.reuse_completed_upload, None);
        let requested: NativeMediaJobRetryRequest =
            serde_json::from_value(json!({"reuseCompletedUpload":true})).unwrap();
        assert_eq!(requested.reuse_completed_upload, Some(true));
        for invalid in [
            json!({"reuseCompletedUpload":"true"}),
            json!({"resume":true}),
            json!({"reuseCompletedUpload":true,"skipReadback":true}),
        ] {
            assert!(serde_json::from_value::<NativeMediaJobRetryRequest>(invalid).is_err());
        }
    }
    #[tokio::test]
    async fn range_body_reuses_the_verified_allocation() {
        let bytes = vec![1, 2, 3, 4, 5];
        let address = bytes.as_ptr() as usize;
        let media = wechat_core::media_types::MediaContent {
            bytes,
            content_type: "application/octet-stream".into(),
            sha256: "a".repeat(64),
            filename: None,
            source_sha256: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("range", "bytes=1-3".parse().unwrap());
        let response = content_response(media, &headers).unwrap();
        assert_eq!(response.headers()["content-length"], "3");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), &[2, 3, 4]);
        assert_eq!(bytes.as_ptr() as usize, address + 1);
    }
    #[test]
    fn upload_chunk_limit_accepts_exact_maximum_and_rejects_overflow_without_allocation() {
        assert!(validate_upload_chunk(MAX_SOURCE_BYTES - 1, 1).is_ok());
        assert!(validate_upload_chunk(MAX_SOURCE_BYTES, 0).is_ok());
        for (current, next) in [
            (MAX_SOURCE_BYTES, 1),
            (0, MAX_SOURCE_BYTES + 1),
            (usize::MAX, 1),
        ] {
            assert_eq!(
                validate_upload_chunk(current, next).unwrap_err().0,
                StatusCode::PAYLOAD_TOO_LARGE
            );
        }
    }
    #[tokio::test]
    async fn preview_response_binds_source_hash_and_ranges_to_preview_bytes() {
        let source = "a".repeat(64);
        let preview_hash = "b".repeat(64);
        let make = || wechat_core::media_types::MediaContent {
            bytes: vec![1, 2, 3, 4],
            content_type: "image/jpeg".into(),
            sha256: preview_hash.clone(),
            filename: Some("synthetic.still.jpg".into()),
            source_sha256: Some(source.clone()),
        };
        let mut headers = HeaderMap::new();
        headers.insert("range", "bytes=1-2".parse().unwrap());
        let response = content_response(make(), &headers).unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["content-range"], "bytes 1-2/4");
        assert_eq!(response.headers()["x-source-sha256"], source);
        assert_eq!(response.headers()["etag"], format!("\"{preview_hash}\""));
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            &[2, 3]
        );
        headers.insert(
            "if-none-match",
            format!("\"{preview_hash}\"").parse().unwrap(),
        );
        let response = content_response(make(), &headers).unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()["x-source-sha256"], source);
    }
    #[test]
    fn ranges_are_single_bounded_and_support_suffixes() {
        assert_eq!(byte_range("bytes=2-5", 10), Some((2, 5)));
        assert_eq!(byte_range("bytes=2-50", 10), Some((2, 9)));
        assert_eq!(byte_range("bytes=7-", 10), Some((7, 9)));
        assert_eq!(byte_range("bytes=-3", 10), Some((7, 9)));
        for bad in [
            "bytes=10-",
            "bytes=3-1",
            "bytes=-0",
            "bytes=1-2,4-5",
            "bytes=184467440737095516160-",
            "invalid",
        ] {
            assert_eq!(byte_range(bad, 10), None);
        }
        assert_eq!(byte_range("bytes=0-", 0), None);
    }
}
