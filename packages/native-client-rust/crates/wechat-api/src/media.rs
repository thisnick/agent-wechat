//! Bounded owned uploads and account-scoped media jobs and bytes.
use super::*;
use axum::extract::Multipart;
use wechat_core::{MediaSendRequest, MediaTransferRequest, MAX_SOURCE_BYTES};

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route(
            "/v1/accounts/{a}/uploads",
            post(upload).layer(DefaultBodyLimit::max(MAX_SOURCE_BYTES + 64 * 1024)),
        )
        .route("/v1/accounts/{a}/messages/{id}/media", get(assets))
        .route(
            "/v1/accounts/{a}/messages/{id}/media-transfers",
            post(transfer),
        )
        .route("/v1/accounts/{a}/media/{id}", get(asset))
        .route("/v1/accounts/{a}/media/{id}/content", get(content))
        .route("/v1/accounts/{a}/jobs/{id}", get(job))
        .route("/v1/accounts/{a}/jobs/{id}/cancel", post(cancel_job))
        .route("/v1/accounts/{a}/jobs/{id}/retry", post(retry_job))
        .route(
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
    let mut file = None;
    while let Some(mut field) = form
        .next_field()
        .await
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_upload"))?
    {
        if file.is_some() || field.name() != Some("file") {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_upload"));
        }
        let name = field.file_name().unwrap_or("attachment").to_owned();
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_upload"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_SOURCE_BYTES {
                return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "upload_too_large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        file = Some((name, mime, bytes));
    }
    let (name, mime, bytes) = file.ok_or(ApiError(StatusCode::BAD_REQUEST, "file_required"))?;
    let source = work(move || app.core.upload_source(&a, &name, &mime, &bytes)).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(source).map_err(anyhow::Error::from)?),
    ))
}
async fn assets(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "media")?;
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        json!({"items":work(move||{let _guard=guard;app.core.media_assets(&a,&id)}).await?}),
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
    let job = app.core.start_media_transfer(&a, input)?;
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
        serde_json::to_value(work(move || app.core.media_asset(&a, &id)).await?)
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
    c.require(Some(&a), if job.kind == "send" { "send" } else { "media" })?;
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
    c.require(Some(&a), if job.kind == "send" { "send" } else { "media" })?;
    Ok(Json(
        serde_json::to_value(app.core.cancel_media_job(&a, &id)?).map_err(anyhow::Error::from)?,
    ))
}
async fn retry_job(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "media")
        .or_else(|_| c.require(Some(&a), "send"))?;
    let job = app.core.media_job(&a, &id)?;
    c.require(Some(&a), if job.kind == "send" { "send" } else { "media" })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(app.core.retry_media_job(&a, &id)?)
                .map_err(anyhow::Error::from)?,
        ),
    ))
}
async fn content(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    let media = work(move || app.core.media_content(&a, &id)).await?;
    let total = media.bytes.len();
    let etag = format!("\"{}\"", media.sha256);
    let mut response = Response::builder()
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
        (
            StatusCode::PARTIAL_CONTENT,
            media.bytes[start..=end].to_vec(),
        )
    } else {
        (StatusCode::OK, media.bytes)
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
    Ok((
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(
                app.core
                    .send_text(&a, key, &request.thread_id, &request.text)
                    .await?,
            )
            .map_err(anyhow::Error::from)?,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
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
