use super::*;
pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/v1/accounts/{a}/capabilities", get(capabilities))
        .route("/v1/accounts/{a}/connect", post(connect))
        .route("/v1/accounts/{a}/disconnect", post(disconnect))
        .route("/v1/accounts/{a}/logout", post(logout))
        .route("/v1/accounts/{a}/forget-credentials", post(forget))
        .route("/v1/accounts/{a}/login-attempts", post(login))
        .route(
            "/v1/accounts/{a}/login-attempts/{id}",
            get(operation).delete(cancel),
        )
        .route("/v1/accounts/{a}/login-attempts/{id}/qr", get(qr))
        .route("/v1/accounts/{a}/login-attempts/{id}/cancel", post(cancel))
        .route(
            "/v1/accounts/{a}/sends/by-idempotency-key",
            get(send_by_key),
        )
        .route("/v1/accounts/{a}/sends/{id}/cancel", post(cancel_send))
        .route("/v1/accounts/{a}/threads/{id}", get(thread))
        .route("/v1/accounts/{a}/contacts/{id}", get(contact))
        .route("/v1/accounts/{a}/groups/{id}", get(group))
        .route(
            "/v1/accounts/{a}/contacts/{id}/refresh",
            post(refresh_contact),
        )
        .route("/v1/accounts/{a}/groups/{id}/refresh", post(refresh_group))
        .route(
            "/v1/accounts/{a}/groups/{id}/members/refresh",
            post(refresh_members),
        )
        .route(
            "/v1/accounts/{a}/avatars/{id}/refresh",
            post(refresh_avatar),
        )
        .route("/v1/accounts/{a}/contact-sync", post(sync_contacts))
        .route("/v1/accounts/{a}/group-sync", post(sync_groups))
}
pub(super) async fn update_account(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<UpdateAccount>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "admin")?;
    if let Some(alias) = input.alias {
        app.core.store.set_account_alias(&a, &alias)?;
    }
    if let Some(enabled) = input.auto_connect {
        app.core
            .store
            .put_runtime(&a, "auto-connect", if enabled { b"1" } else { b"0" })?;
    }
    Ok(Json(app.core.account_status(&a)?))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateAccount {
    alias: Option<String>,
    auto_connect: Option<bool>,
}
async fn capabilities(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    app.core.store.account(&a)?;
    Ok(Json(
        json!({"nativeRuntime":true,"officialBinaryRequired":false,"mediaUploadMaxBytes":wechat_core::MAX_SOURCE_BYTES,"mediaKinds":["image","file","video","voice"],"downloadQualities":["best","thumbnail","standard","original","normal","encoded"],"logout":true,"savedDeviceLogin":true,"profileKeysConfigured":app.core.store.keys_file(&a)?.is_some()}),
    ))
}
async fn connect(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "login")?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(app.core.connect(&a).await?).map_err(anyhow::Error::from)?),
    ))
}
async fn logout(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "login")?;
    let key = idempotency_key(&headers)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(app.core.logout(&a, key).await?).map_err(anyhow::Error::from)?),
    ))
}
async fn forget(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeForgetRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "login")?;
    if !input.confirm {
        return Err(ApiError(StatusCode::BAD_REQUEST, "confirmation_required"));
    }
    let key = idempotency_key(&headers)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(app.core.forget_credentials(&a, key).await?)
                .map_err(anyhow::Error::from)?,
        ),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendKey {
    key: String,
}
async fn send_by_key(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<SendKey>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "send")?;
    if q.key.is_empty() || q.key.len() > 200 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_idempotency_key"));
    }
    let result = match app.core.store.send_by_key(&a, &q.key) {
        Ok(send) => serde_json::to_value(send),
        Err(_) => serde_json::to_value(app.core.media_job_by_key(&a, &q.key)?),
    };
    Ok(Json(result.map_err(anyhow::Error::from)?))
}
async fn cancel_send(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "send")?;
    let result = match app.core.store.send(&a, &id) {
        Ok(_) => serde_json::to_value(app.core.store.cancel_send(&a, &id)?),
        Err(_) => serde_json::to_value(app.core.cancel_media_job(&a, &id)?),
    };
    Ok(Json(result.map_err(anyhow::Error::from)?))
}
async fn exact(
    app: App,
    c: Claims,
    a: String,
    id: String,
    kind: &'static str,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let guard = app.core.lock_account(&a).await?;
    let value = work(move || {
        let _guard = guard;
        app.core.catalog_item(&a, kind, &id)
    })
    .await?;
    match kind {
        "threads" => checked::<contracts::NativeThread>(value),
        "contacts" => checked::<contracts::NativeContact>(value),
        "groups" => checked::<contracts::NativeGroup>(value),
        _ => Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request")),
    }
}
async fn thread(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    exact(app, c, a, id, "threads").await
}
async fn contact(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    exact(app, c, a, id, "contacts").await
}
async fn group(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    exact(app, c, a, id, "groups").await
}
fn metadata(
    app: App,
    c: Claims,
    a: String,
    action: wechat_core::MetadataAction,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "read")?;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(app.core.start_metadata(&a, action)?)
                .map_err(anyhow::Error::from)?,
        ),
    ))
}
async fn refresh_contact(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::Contact { contact_id: id },
    )
}
async fn refresh_group(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::Group { group_id: id },
    )
}
async fn refresh_members(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::Members {
            group_id: id,
            version: 0,
        },
    )
}
async fn refresh_avatar(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "media")?;
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::Avatar { contact_id: id },
    )
}
async fn sync_contacts(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::ContactSync { max_pages: 10 },
    )
}
async fn sync_groups(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    metadata(
        app,
        c,
        a,
        wechat_core::MetadataAction::GroupSync { max_pages: 10 },
    )
}
