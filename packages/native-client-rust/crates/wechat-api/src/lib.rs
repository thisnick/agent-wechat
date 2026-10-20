//! Authenticated, account-scoped native daemon API.
use anyhow::{bail, Context, Result};
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path as FsPath,
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use wechat_core::{Core, LoginRequest};
use wechat_store::{AccessToken, Store};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    product: &'static str,
    backend: &'static str,
    version: &'static str,
    stage: &'static str,
    target: &'static str,
    api_version: u32,
    capabilities: Vec<&'static str>,
    native_login: bool,
    official_database_writes: bool,
}
pub fn version() -> Version {
    Version {
        product: "agent-wechat",
        backend: "native",
        version: env!("CARGO_PKG_VERSION"),
        stage: "experimental",
        target: env!("WX_TARGET"),
        api_version: 1,
        capabilities: vec![
            "serve",
            "profile-attach",
            "profile-copy",
            "profile-read",
            "native-login",
            "native-sync",
            "text-send",
            "events",
            "scoped-tokens",
            "config",
        ],
        native_login: true,
        official_database_writes: true,
    }
}
#[derive(Clone)]
struct App {
    core: Arc<Core>,
    token_hash: [u8; 32],
}
#[derive(Clone)]
struct Claims {
    grant: Option<AccessToken>,
}
impl Claims {
    fn require(&self, account: Option<&str>, scope: &str) -> ApiResult<()> {
        if let Some(grant) = &self.grant {
            if !grant.scopes.iter().any(|s| s == scope || s == "admin")
                || account.is_some_and(|a| !grant.accounts.iter().any(|v| v == "*" || v == a))
            {
                return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
            }
        }
        Ok(())
    }
    fn valid(&self, store: &Store) -> bool {
        match &self.grant {
            None => true,
            Some(g) => store.access_tokens().is_ok_and(|grants| {
                grants.iter().any(|t| {
                    t.id == g.id
                        && !t.revoked
                        && t.expires_at
                            .is_none_or(|e| e > wechat_store::unix_seconds())
                })
            }),
        }
    }
}
struct ApiError(StatusCode, &'static str);
type ApiResult<T> = std::result::Result<T, ApiError>;
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":{"code":self.1}}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(profile) = error.downcast_ref::<wechat_profile::ProfileError>() {
            use wechat_profile::ProfileError::*;
            return match profile {
                NotCached => Self(StatusCode::NOT_FOUND, "media_not_cached"),
                CursorExpired => Self(StatusCode::CONFLICT, "cursor_expired"),
                ImageQualityUnavailable => Self(StatusCode::CONFLICT, "image_quality_unavailable"),
                InvalidQuery | InvalidCursor => Self(StatusCode::BAD_REQUEST, "invalid_request"),
                _ => Self(StatusCode::CONFLICT, "profile_unavailable"),
            };
        }
        let message = error.to_string();
        for (needle, status, code) in [
            (
                "account not found",
                StatusCode::NOT_FOUND,
                "account_not_found",
            ),
            ("no rows", StatusCode::NOT_FOUND, "not_found"),
            ("auth_required", StatusCode::CONFLICT, "auth_required"),
            (
                "profile_keys_missing",
                StatusCode::CONFLICT,
                "profile_keys_missing",
            ),
            (
                "idempotency_conflict",
                StatusCode::CONFLICT,
                "idempotency_conflict",
            ),
            (
                "read_state_conflict",
                StatusCode::CONFLICT,
                "read_state_conflict",
            ),
            (
                "event_cursor_expired",
                StatusCode::GONE,
                "event_cursor_expired",
            ),
            (
                "operation_in_progress",
                StatusCode::CONFLICT,
                "operation_in_progress",
            ),
            (
                "login_challenge_unavailable",
                StatusCode::NOT_FOUND,
                "login_challenge_unavailable",
            ),
            ("unsupported", StatusCode::CONFLICT, "unsupported_profile"),
            ("not supported", StatusCode::CONFLICT, "unsupported_profile"),
            ("invalid", StatusCode::BAD_REQUEST, "invalid_request"),
        ] {
            if message.to_lowercase().contains(needle) {
                return Self(status, code);
            }
        }
        Self(StatusCode::INTERNAL_SERVER_ERROR, "operation_failed")
    }
}
async fn authorize(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    else {
        return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    if token.len() > 1024 {
        return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let claims = if bool::from(digest.ct_eq(&app.token_hash)) {
        Some(Claims { grant: None })
    } else {
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        app.core
            .store
            .access_tokens()
            .ok()
            .and_then(|tokens| {
                tokens.into_iter().find(|t| {
                    !t.revoked
                        && t.expires_at
                            .is_none_or(|e| e > wechat_store::unix_seconds())
                        && bool::from(t.digest.as_bytes().ct_eq(hash.as_bytes()))
                })
            })
            .map(|grant| Claims { grant: Some(grant) })
    };
    let Some(claims) = claims else {
        return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    request.extensions_mut().insert(claims);
    next.run(request).await
}
async fn work<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "worker_failed"))?
        .map_err(Into::into)
}
async fn status(State(app): State<App>, Extension(c): Extension<Claims>) -> ApiResult<Json<Value>> {
    c.require(None, "read")?;
    Ok(Json(
        json!({"serverId":app.core.store.server_id,"dataDir":app.core.store.root,"pid":std::process::id(),"ownershipGeneration":app.core.ownership_generation,"version":version(),"state":"ready"}),
    ))
}
async fn accounts(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    c.require(None, "read")?;
    let records = app.core.store.accounts()?;
    let mut items = vec![];
    for account in records {
        if c.require(Some(&account.id), "read").is_ok() {
            items.push(app.core.account_status(&account.id)?);
        }
    }
    Ok(Json(json!({"items":items})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAccount {
    id: String,
}
async fn create_account(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Json(input): Json<CreateAccount>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(None, "admin")?;
    if c.grant
        .as_ref()
        .is_some_and(|g| !g.accounts.iter().any(|s| s == "*"))
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    let account = app.core.create_account(&input.id)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(account).map_err(anyhow::Error::from)?),
    ))
}
async fn account(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    Ok(Json(app.core.account_status(&a)?))
}
async fn login(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<LoginRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "login")?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(app.core.login(&a, input).await?).map_err(anyhow::Error::from)?),
    ))
}
async fn operation(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "login")?;
    Ok(Json(
        serde_json::to_value(app.core.store.operation(&a, &id)?).map_err(anyhow::Error::from)?,
    ))
}
async fn cancel(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "login")?;
    Ok(Json(
        serde_json::to_value(app.core.cancel_operation(&a, &id)?).map_err(anyhow::Error::from)?,
    ))
}
async fn qr(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    c.require(Some(&a), "login")?;
    Ok((
        [("content-type", "image/png"), ("cache-control", "no-store")],
        app.core.qr(&a, &id)?,
    )
        .into_response())
}
async fn disconnect(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "login")?;
    app.core.disconnect(&a)?;
    Ok(Json(json!({"state":"disconnected","trustRetained":true})))
}
async fn local_logout(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "login")?;
    app.core.logout_local(&a)?;
    Ok(Json(
        json!({"state":"logged_out","serverRevocation":"not_requested"}),
    ))
}
async fn sync(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "login")?;
    Ok(Json(app.core.sync(&a).await?))
}
#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    search: Option<String>,
}
impl PageQuery {
    fn profile(&self) -> ApiResult<wechat_profile::ListQuery> {
        if self.limit.is_some_and(|v| v == 0 || v > 200)
            || self.cursor.as_ref().is_some_and(|v| v.len() > 8192)
            || self.search.as_ref().is_some_and(|v| v.len() > 256)
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        Ok(wechat_profile::ListQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
            search: self.search.clone(),
        })
    }
}
macro_rules! listing {
    ($name:ident,$method:ident) => {
        async fn $name(
            State(app): State<App>,
            Extension(c): Extension<Claims>,
            Path(a): Path<String>,
            Query(q): Query<PageQuery>,
        ) -> ApiResult<Json<Value>> {
            c.require(Some(&a), "read")?;
            let q = q.profile()?;
            let _guard = app.core.lock_account(&a).await?;
            Ok(Json(work(move || app.core.$method(&a, &q)).await?))
        }
    };
}
listing!(threads, threads);
listing!(contacts, contacts);
listing!(groups, groups);
async fn schema(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let _guard = app.core.lock_account(&a).await?;
    Ok(Json(work(move || app.core.profile_schema(&a)).await?))
}
async fn members(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, room)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let q = q.profile()?;
    let _guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || app.core.group_members(&a, &room, &q)).await?,
    ))
}
async fn messages(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, thread)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    q.profile()?;
    let _guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || app.core.messages(&a, &thread, q.limit, q.cursor)).await?,
    ))
}
async fn message(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let _guard = app.core.lock_account(&a).await?;
    Ok(Json(work(move || app.core.message(&a, &id)).await?))
}
async fn avatar(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    let _guard = app.core.lock_account(&a).await?;
    let media = work(move || app.core.avatar(&a, &id)).await?;
    Ok((
        [
            ("content-type", media.media_type),
            ("cache-control", "private, no-store".into()),
        ],
        media.bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaQuery {
    quality: Option<String>,
}
async fn image(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Query(q): Query<MediaQuery>,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    let quality = match q.quality.as_deref().unwrap_or("best") {
        "best" => wechat_profile::ImageQuality::Best,
        "original" => wechat_profile::ImageQuality::Original,
        "standard" => wechat_profile::ImageQuality::Standard,
        "thumbnail" => wechat_profile::ImageQuality::Thumbnail,
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_quality")),
    };
    let _guard = app.core.lock_account(&a).await?;
    let media = work(move || app.core.image(&a, &id, quality)).await?;
    Ok((
        [
            ("content-type", media.media.media_type),
            ("cache-control", "private, no-store".into()),
            (
                "x-wechat-image-quality",
                serde_json::to_value(media.quality)
                    .map_err(anyhow::Error::from)?
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
            ),
        ],
        media.media.bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupRequest {
    ids: Vec<String>,
}
async fn lookup_contacts(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<LookupRequest>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    Ok(Json(app.core.lookup_contacts(&a, input.ids).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomQuery {
    #[serde(default)]
    version: u32,
}
async fn lookup_room(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, room)): Path<(String, String)>,
    Query(q): Query<RoomQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    Ok(Json(app.core.lookup_room(&a, &room, q.version).await?))
}
async fn recover_projection(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "admin")?;
    let _guard = app.core.lock_account(&a).await?;
    Ok(Json(work(move || app.core.recover_projection(&a)).await?))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextSend {
    thread_id: String,
    text: String,
}
async fn send_text(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<TextSend>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "send")?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
        ))?;
    let intent = app
        .core
        .send_text(&a, key, &input.thread_id, &input.text)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(intent).map_err(anyhow::Error::from)?),
    ))
}
async fn send_status(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "send")?;
    Ok(Json(
        serde_json::to_value(app.core.store.send(&a, &id)?).map_err(anyhow::Error::from)?,
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadState {
    message_ids: Vec<String>,
    expected_version: String,
}
async fn read_state(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, thread)): Path<(String, String)>,
    Json(input): Json<ReadState>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read-state")?;
    Ok(Json(
        app.core
            .mark_read(&a, &thread, input.message_ids, &input.expected_version)
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventQuery {
    #[serde(default)]
    after: Option<String>,
    limit: Option<usize>,
}
fn decimal(value: Option<&str>) -> ApiResult<i64> {
    let v = value.unwrap_or("0");
    if v.is_empty() || v.len() > 19 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_cursor"));
    }
    v.parse()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_cursor"))
}
async fn events(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let after = decimal(q.after.as_deref())?;
    let items = app.core.store.events(&a, after, q.limit.unwrap_or(100))?;
    Ok(Json(
        json!({"items":items,"nextCursor":items.last().map(|v|v.cursor.clone()).unwrap_or_else(||after.to_string())}),
    ))
}
async fn stream(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
) -> ApiResult<Response> {
    c.require(Some(&a), "read")?;
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let mut after = decimal(q.after.as_deref().or(last))?;
    app.core.store.events(&a, after, 1)?;
    let events = async_stream::stream! {loop {
        if app.core.stopping.load(std::sync::atomic::Ordering::SeqCst)||!c.valid(&app.core.store){break;}
        match app.core.store.events(&a,after,100){Ok(items)=>for item in items {after=item.cursor.parse().unwrap_or(after);yield Ok::<_,std::convert::Infallible>(Event::default().id(&item.cursor).json_data(&item).expect("serializable event"));},Err(_)=>{yield Ok(Event::default().event("error").data("{\"error\":{\"code\":\"event_cursor_expired\"}}"));break;}}
        tokio::time::sleep(Duration::from_millis(500)).await;
    }};
    Ok(Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    cursor: String,
}
async fn ack(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, consumer)): Path<(String, String)>,
    Json(input): Json<Ack>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    app.core
        .store
        .acknowledge(&a, &consumer, decimal(Some(&input.cursor))?)?;
    Ok(Json(
        json!({"cursor":input.cursor,"messageReadStateChanged":false}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TokenRequest {
    name: String,
    accounts: Vec<String>,
    scopes: Vec<String>,
    expires_at: Option<i64>,
}
async fn issue_token(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Json(input): Json<TokenRequest>,
) -> ApiResult<Json<Value>> {
    if c.grant.is_some() {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_token_required"));
    }
    use rand::RngCore;
    let mut bytes = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    let grant = app.core.store.issue_token(
        &input.name,
        &hash,
        &input.accounts,
        &input.scopes,
        input.expires_at,
    )?;
    Ok(Json(json!({"token":token,"grant":grant})))
}
async fn tokens(State(app): State<App>, Extension(c): Extension<Claims>) -> ApiResult<Json<Value>> {
    if c.grant.is_some() {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_token_required"));
    }
    Ok(Json(json!({"items":app.core.store.access_tokens()?})))
}
async fn revoke_token(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    if c.grant.is_some() {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_token_required"));
    }
    Ok(Json(json!({"revoked":app.core.store.revoke_token(&id)?})))
}
pub fn router(core: Arc<Core>, token: &str) -> Router {
    let app = App {
        core,
        token_hash: Sha256::digest(token.as_bytes()).into(),
    };
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/tokens", get(tokens).post(issue_token))
        .route("/v1/tokens/{id}", axum::routing::delete(revoke_token))
        .route("/v1/accounts", get(accounts).post(create_account))
        .route("/v1/accounts/{a}", get(account))
        .route("/v1/accounts/{a}/schema", get(schema))
        .route(
            "/v1/accounts/{a}/projection/recover",
            post(recover_projection),
        )
        .route("/v1/accounts/{a}/contacts/lookup", post(lookup_contacts))
        .route("/v1/accounts/{a}/groups/{room}/lookup", get(lookup_room))
        .route("/v1/accounts/{a}/auth/login", post(login))
        .route("/v1/accounts/{a}/auth/disconnect", post(disconnect))
        .route("/v1/accounts/{a}/auth/logout-local", post(local_logout))
        .route(
            "/v1/accounts/{a}/operations/{id}",
            get(operation).delete(cancel),
        )
        .route("/v1/accounts/{a}/operations/{id}/qr", get(qr))
        .route("/v1/accounts/{a}/sync", post(sync))
        .route("/v1/accounts/{a}/threads", get(threads))
        .route("/v1/accounts/{a}/contacts", get(contacts))
        .route("/v1/accounts/{a}/groups", get(groups))
        .route("/v1/accounts/{a}/groups/{room}/members", get(members))
        .route("/v1/accounts/{a}/threads/{thread}/messages", get(messages))
        .route("/v1/accounts/{a}/messages/{id}", get(message))
        .route("/v1/accounts/{a}/avatars/{id}", get(avatar))
        .route("/v1/accounts/{a}/messages/{id}/image", get(image))
        .route("/v1/accounts/{a}/sends", post(send_text))
        .route("/v1/accounts/{a}/sends/{id}", get(send_status))
        .route(
            "/v1/accounts/{a}/threads/{thread}/read-state",
            post(read_state),
        )
        .route("/v1/accounts/{a}/events", get(events))
        .route("/v1/accounts/{a}/events/stream", get(stream))
        .route("/v1/accounts/{a}/consumers/{consumer}/ack", post(ack))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), authorize))
        .with_state(app)
}
pub fn read_token(path: &FsPath) -> Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("cannot open API token file")?;
    let meta = file.metadata().context("cannot read API token metadata")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
        || meta.len() > 1024
    {
        bail!("API token must be an owner-only regular file of at most 1024 bytes");
    }
    let mut token = String::new();
    file.take(1025)
        .read_to_string(&mut token)
        .context("cannot read API token file")?;
    if token.len() > 1024 {
        bail!("API token file is too large");
    }
    let token = token.trim();
    if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("invalid API token file");
    }
    Ok(token.into())
}
pub fn provision_token(path: &FsPath, allow_create: bool) -> Result<String> {
    if fs::symlink_metadata(path).is_ok() {
        return read_token(path);
    }
    if !allow_create {
        bail!("explicit API token file is missing");
    }
    let parent = path.parent().context("token file needs a parent")?;
    wechat_store::private_dir(parent)?;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            writeln!(file, "{token}")?;
            file.sync_all()?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_token(path),
        Err(e) => Err(e).context("cannot provision API token"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    const OWNER: &str = "0123456789abcdef0123456789abcdef";
    fn setup() -> (tempfile::TempDir, Arc<Core>, Router) {
        let t = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let core = Core::new(store, None).unwrap();
        let app = router(core.clone(), OWNER);
        (t, core, app)
    }
    async fn call(
        app: Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let response = app
            .oneshot(
                request
                    .body(
                        body.map(|v| Body::from(serde_json::to_vec(&v).unwrap()))
                            .unwrap_or_else(Body::empty),
                    )
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn authorization_is_required_and_status_identifies_owned_process() {
        let (_t, core, app) = setup();
        for token in [None, Some("wrong")] {
            assert_eq!(
                call(app.clone(), "GET", "/v1/status", token, None).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        let (status, v) = call(app, "GET", "/v1/status", Some(OWNER), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["pid"], std::process::id());
        assert_eq!(v["ownershipGeneration"], core.ownership_generation);
        assert_eq!(v["version"]["backend"], "native");
        assert!(v["dataDir"].as_str().unwrap().ends_with("/data"));
    }
    #[tokio::test]
    async fn grants_are_scoped_before_profile_io_and_revocation_is_immediate() {
        let (_t, core, app) = setup();
        let token = "another-long-synthetic-token-123456";
        let hash = format!("{:x}", Sha256::digest(token));
        let grant = core
            .store
            .issue_token("read one", &hash, &["one".into()], &["read".into()], None)
            .unwrap();
        let (status, v) = call(app.clone(), "GET", "/v1/accounts", Some(token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][0]["id"], "one");
        for (method, path, body) in [
            ("GET", "/v1/accounts/two/threads", None),
            (
                "POST",
                "/v1/accounts/one/sends",
                Some(json!({"threadId":"filehelper","text":"synthetic"})),
            ),
            ("POST", "/v1/accounts/one/auth/login", Some(json!({}))),
        ] {
            assert_eq!(
                call(app.clone(), method, path, Some(token), body).await.0,
                StatusCode::FORBIDDEN
            );
        }
        core.store.revoke_token(&grant.id).unwrap();
        assert_eq!(
            call(app, "GET", "/v1/status", Some(token), None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn event_ack_does_not_read_messages_or_change_read_generation() {
        let (_t, core, app) = setup();
        core.store
            .append_event("one", "sync.received", &json!({"projection":"pending"}))
            .unwrap();
        let (_, page) = call(
            app.clone(),
            "GET",
            "/v1/accounts/one/events?after=0",
            Some(OWNER),
            None,
        )
        .await;
        assert_eq!(page["items"][0]["cursor"], "1");
        let (status, ack) = call(
            app.clone(),
            "POST",
            "/v1/accounts/one/consumers/test/ack",
            Some(OWNER),
            Some(json!({"cursor":"1"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ack["messageReadStateChanged"], false);
        assert_eq!(core.store.read_version("one", "filehelper").unwrap(), "0");
        assert_eq!(
            call(
                app,
                "GET",
                "/v1/accounts/one/events?after=1e3",
                Some(OWNER),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn public_errors_do_not_return_keys_or_filesystem_paths() {
        let (_t, _core, app) = setup();
        let (status, v) = call(app, "GET", "/v1/accounts/one/threads", Some(OWNER), None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v, json!({"error":{"code":"profile_keys_missing"}}));
    }
    #[test]
    fn token_file_is_private_persistent_and_never_replaced_on_failure() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("private/token");
        let first = provision_token(&p, true).unwrap();
        assert_eq!(first, provision_token(&p, true).unwrap());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(provision_token(&p, true).is_err());
        assert_eq!(fs::read_to_string(p).unwrap().trim(), first);
    }
}
