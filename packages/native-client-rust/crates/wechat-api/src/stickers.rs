//! Resource-oriented local sticker views and owned-media preparation.
use super::*;
use wechat_core::StickerPreparationRequest;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .api_route(
            "/v1/accounts/{a}/sticker-packs/{id}/membership",
            axum::routing::put(add_pack).delete(remove_pack),
        )
        .api_route("/v1/accounts/{a}/sticker-collections", get(collections))
        .api_route("/v1/accounts/{a}/sticker-packs", get(packs))
        .api_route("/v1/accounts/{a}/sticker-packs/{id}", get(pack))
        .api_route(
            "/v1/accounts/{a}/sticker-packs/{id}/stickers",
            get(pack_items),
        )
        .api_route(
            "/v1/accounts/{a}/sticker-packs/{id}/stickers/{stickerId}",
            get(pack_item),
        )
        .api_route("/v1/accounts/{a}/sticker-syncs", post(sync))
        .api_route("/v1/accounts/{a}/sticker-downloads", post(download))
        .api_route("/v1/accounts/{a}/sticker-preparations", post(prepare))
        .api_route(
            "/v1/accounts/{a}/sticker-preparations/{id}",
            get(preparation),
        )
        .api_route("/v1/accounts/{a}/custom-stickers", get(custom).post(add))
        .api_route(
            "/v1/accounts/{a}/custom-stickers/{id}",
            get(custom_item).patch(move_item).delete(remove),
        )
}
async fn prepare(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<StickerPreparationRequest>,
) -> ApiResult<impl IntoResponse> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.prepare_sticker(&a, &key, input)
    })
    .await?;
    let location = format!(
        "/v1/accounts/{}/sticker-preparations/{}",
        job.account_id, job.id
    );
    Ok((StatusCode::ACCEPTED, [("location", location)], Json(job)))
}
async fn preparation(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<wechat_core::MediaJob>> {
    c.require(Some(&a), "media")?;
    Ok(Json(app.core.sticker_preparation(&a, &id)?))
}
async fn custom(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<impl IntoResponse> {
    c.require(Some(&a), "media")?;
    let saved = app
        .core
        .read(&a.clone(), move |core| core.saved_stickers(&a))
        .await?;
    Ok(([("etag", format!("\"{}\"", saved.revision))], Json(saved)))
}
async fn custom_item(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<wechat_core::CustomSticker>> {
    c.require(Some(&a), "media")?;
    // Indexing existing cache bytes writes operational media references, so
    // use the same account gate as the ordinary attachment-list endpoint.
    let guard = app.core.lock_account(&a).await?;
    let item = work(move || {
        let _guard = guard;
        app.core.custom_sticker(&a, &id)
    })
    .await?;
    Ok(Json(item))
}

fn collection_match(headers: &HeaderMap) -> ApiResult<String> {
    let value = headers
        .get("if-match")
        .ok_or(ApiError(
            StatusCode::PRECONDITION_REQUIRED,
            "sticker_revision_required",
        ))?
        .to_str()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_sticker_revision"))?;
    let revision = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_sticker_revision",
        ))?;
    Ok(revision.to_owned())
}
fn submitted_job(job: wechat_core::MediaJob) -> Response {
    let location = format!("/v1/accounts/{}/jobs/{}", job.account_id, job.id);
    (StatusCode::ACCEPTED, [("location", location)], Json(job)).into_response()
}
async fn add(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<wechat_core::AddCustomStickerRequest>,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.add_custom_sticker(&a, &key, input)
    })
    .await?;
    Ok(submitted_job(job))
}
async fn move_item(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<wechat_core::MoveCustomStickerRequest>,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let revision = collection_match(&headers)?;
    let guard = app.core.lock_account(&a).await?;
    let core = app.core.clone();
    let account = a.clone();
    let result = work(move || {
        let _guard = guard;
        core.move_custom_sticker(&account, &key, &id, &revision, input)
    })
    .await;
    mutation_result(app, a, result).await
}
async fn remove(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let revision = collection_match(&headers)?;
    let guard = app.core.lock_account(&a).await?;
    let core = app.core.clone();
    let account = a.clone();
    let result = work(move || {
        let _guard = guard;
        core.remove_custom_sticker(&account, &key, &id, &revision)
    })
    .await;
    mutation_result(app, a, result).await
}
async fn mutation_result(
    app: App,
    account: String,
    result: ApiResult<wechat_core::MediaJob>,
) -> ApiResult<Response> {
    match result {
        Ok(job) => Ok(submitted_job(job)),
        Err(ApiError(StatusCode::PRECONDITION_FAILED, _)) => {
            let saved = app
                .core
                .read(&account.clone(), move |core| core.saved_stickers(&account))
                .await?;
            Ok((StatusCode::PRECONDITION_FAILED, [("etag",format!("\"{}\"",saved.revision))],
                Json(json!({"error":{"code":"sticker_revision_mismatch", "currentRevision":saved.revision}}))).into_response())
        }
        Err(error) => Err(error),
    }
}

async fn sync(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<wechat_core::StickerSyncRequest>,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.sync_stickers(&a, &key, input.scope)
    })
    .await?;
    Ok(submitted_job(job))
}
async fn download(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<wechat_core::StickerDownloadRequest>,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        match (input.pack_id, input.sticker_id, input.asset_id) {
            (Some(pack), item, None) => {
                app.core
                    .download_sticker_pack(&a, &key, &pack, item.as_deref())
            }
            (None, Some(item), None) => app.core.download_custom_sticker(&a, &key, &item),
            (None, None, Some(asset)) => app.core.download_sticker_preview(&a, &key, &asset),
            _ => anyhow::bail!("invalid_sticker_download"),
        }
    })
    .await?;
    Ok(submitted_job(job))
}

/// Filters for account-added membership or a page of the available feed.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StickerPackQuery {
    /// Select `added` for the complete canonical account list; omission browses the feed.
    membership: Option<StickerMembershipFilter>,
    /// Opaque available-feed continuation returned by the preceding page.
    cursor: Option<String>,
}
/// Supported membership list filter.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum StickerMembershipFilter {
    /// Every pack in the account's canonical added list.
    Added,
}
/// Locale used to choose optional official captions.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StickerLanguageQuery {
    /// Exact official locale; literal `default` is the fallback. Defaults to `en`.
    language: Option<String>,
}
async fn collections(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<wechat_core::StickerCollections>> {
    c.require(Some(&a), "media")?;
    Ok(Json(
        app.core
            .read(&a.clone(), move |core| core.sticker_collections(&a))
            .await?,
    ))
}
async fn packs(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(query): Query<StickerPackQuery>,
) -> ApiResult<Json<wechat_core::StickerPackList>> {
    c.require(Some(&a), "media")?;
    if query.membership.is_some() {
        if query.cursor.is_some() {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "invalid_sticker_pack_query",
            ));
        }
        let guard = app.core.lock_account(&a).await?;
        return Ok(Json(
            work(move || {
                let _guard = guard;
                app.core.added_sticker_packs(&a)
            })
            .await?,
        ));
    }
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || {
            let _guard = guard;
            app.core.browse_sticker_packs(&a, query.cursor.as_deref())
        })
        .await?,
    ))
}
async fn pack(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<wechat_core::StickerPack>> {
    c.require(Some(&a), "media")?;
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || {
            let _guard = guard;
            app.core.sticker_pack_detail(&a, &id)
        })
        .await?,
    ))
}
async fn pack_items(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Query(query): Query<StickerLanguageQuery>,
) -> ApiResult<Json<wechat_core::StickerPackStickers>> {
    c.require(Some(&a), "media")?;
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || {
            let _guard = guard;
            app.core
                .sticker_pack_stickers(&a, &id, query.language.as_deref().unwrap_or("en"))
        })
        .await?,
    ))
}
async fn pack_item(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id, sticker)): Path<(String, String, String)>,
    Query(query): Query<StickerLanguageQuery>,
) -> ApiResult<Json<wechat_core::StickerPackSticker>> {
    c.require(Some(&a), "media")?;
    let guard = app.core.lock_account(&a).await?;
    let items = work(move || {
        let _guard = guard;
        app.core
            .sticker_pack_stickers(&a, &id, query.language.as_deref().unwrap_or("en"))
    })
    .await?;
    Ok(Json(
        items
            .items
            .into_iter()
            .find(|item| item.id == sticker)
            .ok_or(ApiError(
                StatusCode::NOT_FOUND,
                "sticker_pack_item_not_found",
            ))?,
    ))
}

/// Empty membership representation. Adding a pack does not purchase it or acquire media.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StickerPackMembershipRequest {}
async fn add_pack(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_input): Json<StickerPackMembershipRequest>,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.set_sticker_pack_membership(&a, &key, &id, true)
    })
    .await?;
    Ok(submitted_job(job))
}
async fn remove_pack(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    c.require(Some(&a), "send")?;
    let key = idempotency_key(&headers)?.to_owned();
    let guard = app.core.lock_account(&a).await?;
    let job = work(move || {
        let _guard = guard;
        app.core.set_sticker_pack_membership(&a, &key, &id, false)
    })
    .await?;
    Ok(submitted_job(job))
}
