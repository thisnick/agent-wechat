//! Authenticated, account-scoped native daemon API.
mod actions;
pub mod contracts;
mod media;
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
#[cfg(all(unix, test))]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs::{self},
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
        stage: "supported-profile",
        target: env!("WX_TARGET"),
        api_version: 1,
        capabilities: vec![
            "serve",
            "profile-attach",
            "profile-copy",
            "profile-read",
            "native-login",
            "official-profile-login",
            "native-sync",
            "native-push",
            "text-send",
            "events",
            "scoped-tokens",
            "config",
        ]
        .into_iter()
        .filter(|capability| *capability != "official-profile-login" || cfg!(target_os = "linux"))
        .collect(),
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
#[derive(Debug)]
struct ApiError(StatusCode, &'static str);
type ApiResult<T> = std::result::Result<T, ApiError>;
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":{"code":self.1}}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        let message = error.to_string();
        // Remote message metadata failures are media errors. Never diagnose
        // these as a local profile/schema mismatch through the broad fallback.
        for code in [
            "unsupported_application_media",
            "unsupported_attachment_encryption",
            "unsupported_attachment_transport",
            "media_metadata_invalid",
            "media_metadata_limit",
            "media_key_missing",
        ] {
            if error.chain().any(|cause| cause.to_string() == code) {
                return Self(StatusCode::CONFLICT, code);
            }
        }
        // Admission attaches a stable public code while retaining its typed
        // profile cause. Prefer that code over the general profile fallback.
        for code in [
            "account_database_password_mismatch",
            "profile_database_unavailable",
            "profile_changed_during_login",
            "profile_resource_limit",
            "profile_keys_unavailable",
        ] {
            if message == code {
                return Self(StatusCode::CONFLICT, code);
            }
        }
        for code in ["profile_cipher_unavailable", "profile_io_failed"] {
            if message == code {
                return Self(StatusCode::SERVICE_UNAVAILABLE, code);
            }
        }
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
        for code in [
            "official_profile_not_found",
            "official_profile_ambiguous",
            "official_profile_in_use",
            "official_profile_overlaps_store",
            "official_profile_inventory_limit",
            "account_profile_mismatch",
            "profile_database_password_required",
        ] {
            if message == code {
                return Self(StatusCode::CONFLICT, code);
            }
        }
        if message == "official_profile_path_must_be_absolute" {
            return Self(
                StatusCode::BAD_REQUEST,
                "official_profile_path_must_be_absolute",
            );
        }
        for (needle, status, code) in [
            (
                "media_identity_unavailable",
                StatusCode::CONFLICT,
                "media_identity_unavailable",
            ),
            (
                "media_source_changed",
                StatusCode::CONFLICT,
                "media_source_changed",
            ),
            ("message_recalled", StatusCode::GONE, "message_recalled"),
            (
                "recall_identity_mismatch",
                StatusCode::CONFLICT,
                "recall_identity_mismatch",
            ),
            (
                "invalid_recall_server_id",
                StatusCode::BAD_REQUEST,
                "invalid_recall_server_id",
            ),
            (
                "operation_cannot_cancel",
                StatusCode::CONFLICT,
                "operation_cannot_cancel",
            ),
            (
                "recall_target_changed",
                StatusCode::CONFLICT,
                "recall_target_changed",
            ),
            (
                "recall_target_unavailable",
                StatusCode::CONFLICT,
                "recall_target_unavailable",
            ),
            (
                "invalid_recall_request",
                StatusCode::BAD_REQUEST,
                "invalid_recall_request",
            ),
            (
                "unsupported_media_variant",
                StatusCode::CONFLICT,
                "media_variant_unavailable",
            ),
            (
                "media_asset_not_found",
                StatusCode::NOT_FOUND,
                "media_asset_not_found",
            ),
            (
                "media_not_cached",
                StatusCode::NOT_FOUND,
                "media_not_cached",
            ),
            (
                "media_preview_unavailable",
                StatusCode::NOT_FOUND,
                "media_preview_unavailable",
            ),
            (
                "media_not_available",
                StatusCode::CONFLICT,
                "media_not_available",
            ),
            (
                "media_encoder_unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
                "media_encoder_unavailable",
            ),
            (
                "media_metadata_unavailable",
                StatusCode::CONFLICT,
                "media_metadata_unavailable",
            ),
            (
                "unsupported_media",
                StatusCode::CONFLICT,
                "unsupported_media",
            ),
            (
                "account_alias_conflict",
                StatusCode::CONFLICT,
                "account_alias_conflict",
            ),
            (
                "account_identity_mismatch",
                StatusCode::CONFLICT,
                "account_identity_mismatch",
            ),
            (
                "account_database_password_mismatch",
                StatusCode::CONFLICT,
                "account_database_password_mismatch",
            ),
            (
                "account_device_mismatch",
                StatusCode::CONFLICT,
                "account_device_mismatch",
            ),
            (
                "profile_identity_unavailable",
                StatusCode::CONFLICT,
                "profile_identity_unavailable",
            ),
            (
                "pagination_cursor_expired",
                StatusCode::CONFLICT,
                "pagination_cursor_expired",
            ),
            ("invalid_cursor", StatusCode::BAD_REQUEST, "invalid_cursor"),
            (
                "pagination_capacity_exceeded",
                StatusCode::SERVICE_UNAVAILABLE,
                "pagination_capacity_exceeded",
            ),
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
    if request.method() == axum::http::Method::GET && request.uri().path() == "/health" {
        return next.run(request).await;
    }
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
    if let Some(rest) = request.uri().path().strip_prefix("/v1/accounts/") {
        let (name, tail) = rest
            .split_once('/')
            .map(|(name, tail)| (name, format!("/{tail}")))
            .unwrap_or((rest, String::new()));
        if let Ok(canonical) = app.core.store.resolve_account(name) {
            if canonical != name {
                let query = request
                    .uri()
                    .query()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default();
                if let Ok(uri) = format!("/v1/accounts/{canonical}{tail}{query}").parse() {
                    *request.uri_mut() = uri;
                }
            }
        }
    }
    next.run(request).await
}
async fn work<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "worker_failed"))?
        .map_err(Into::into)
}
fn checked<T: serde::de::DeserializeOwned>(value: Value) -> ApiResult<Json<Value>> {
    Ok(Json(contracts::checked::<T>(value).map_err(|_| {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "invalid_server_response")
    })?))
}
// Lifecycle requests require the owner token and bind to the current store generation.
async fn shutdown_daemon(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    if claims.grant.is_some() {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_token_required"));
    }
    if body["pid"].as_u64() != Some(std::process::id() as u64)
        || body["serverId"] != app.core.store.server_id
        || body["ownershipGeneration"] != app.core.ownership_generation
    {
        return Err(ApiError(StatusCode::CONFLICT, "daemon_ownership_mismatch"));
    }
    app.core.shutdown();
    Ok(Json(json!({"state":"stopping","logout":false})))
}
async fn status(State(app): State<App>, Extension(c): Extension<Claims>) -> ApiResult<Json<Value>> {
    c.require(None, "read")?;
    checked::<contracts::NativeStatus>(
        json!({"serverId":app.core.store.server_id,"dataDir":app.core.store.root,"pid":std::process::id(),"ownershipGeneration":app.core.ownership_generation,"version":version(),"state":"ready"}),
    )
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
    id: Option<String>,
    alias: Option<String>,
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
    let id = input
        .id
        .unwrap_or_else(|| format!("account-{}", rand::random::<u64>()));
    let account = app
        .core
        .create_account_with_alias(&id, input.alias.as_deref())?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(account).map_err(anyhow::Error::from)?),
    ))
}
async fn prepare_official_profile(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Json(input): Json<wechat_core::OfficialProfileRequest>,
) -> ApiResult<Json<wechat_core::OfficialProfileSelection>> {
    c.require(None, "admin")?;
    if c.grant
        .as_ref()
        .is_some_and(|g| !g.accounts.iter().any(|s| s == "*"))
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    Ok(Json(app.core.prepare_official_profile(input).await?))
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
    headers: HeaderMap,
    Json(input): Json<LoginRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "login")?;
    let key = if headers.contains_key("idempotency-key") {
        Some(idempotency_key(&headers)?)
    } else {
        None
    };
    Ok((
        StatusCode::ACCEPTED,
        checked::<contracts::NativeOperation>(
            serde_json::to_value(app.core.login_keyed(&a, input, key).await?)
                .map_err(anyhow::Error::from)?,
        )?,
    ))
}
fn require_operation_scope(claims: &Claims, account: &str, kind: &str) -> ApiResult<()> {
    claims.require(
        Some(account),
        match kind {
            "login" | "logout" | "forget_credentials" => "login",
            "media-writeback" => "media",
            "message_recall" => "send",
            "sync" | "metadata" => "read",
            _ => "admin",
        },
    )
}
async fn operation(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")
        .or_else(|_| c.require(Some(&a), "login"))
        .or_else(|_| c.require(Some(&a), "media"))
        .or_else(|_| c.require(Some(&a), "send"))?;
    let op = app.core.store.operation(&a, &id)?;
    require_operation_scope(&c, &a, &op.kind)?;
    checked::<contracts::NativeOperation>(serde_json::to_value(op).map_err(anyhow::Error::from)?)
}
async fn cancel(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")
        .or_else(|_| c.require(Some(&a), "login"))
        .or_else(|_| c.require(Some(&a), "media"))
        .or_else(|_| c.require(Some(&a), "send"))?;
    let current = app.core.store.operation(&a, &id)?;
    require_operation_scope(&c, &a, &current.kind)?;
    checked::<contracts::NativeOperation>(
        serde_json::to_value(app.core.cancel_operation(&a, &id)?).map_err(anyhow::Error::from)?,
    )
}
async fn qr(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    Query(q): Query<QrQuery>,
) -> ApiResult<Response> {
    c.require(Some(&a), "login")?;
    if q.generation.is_some_and(|expected| {
        app.core
            .store
            .operation(&a, &id)
            .ok()
            .and_then(|op| op.result["qrGeneration"].as_u64())
            .is_none_or(|current| current != expected)
    }) {
        return Err(ApiError(StatusCode::CONFLICT, "login_challenge_superseded"));
    }
    Ok((
        [("content-type", "image/png"), ("cache-control", "no-store")],
        app.core.qr(&a, &id)?,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QrQuery {
    generation: Option<u64>,
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
    Ok(Json(
        serde_json::to_value(app.core.sync_operation(&a)?).map_err(anyhow::Error::from)?,
    ))
}
#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    #[serde(alias = "query")]
    search: Option<String>,
    kind: Option<String>,
}
impl PageQuery {
    fn catalog(&self) -> ApiResult<wechat_core::CatalogQuery> {
        self.profile()?;
        if self
            .kind
            .as_ref()
            .is_some_and(|k| !matches!(k.as_str(), "direct" | "group" | "system"))
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        Ok(wechat_core::CatalogQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
            search: self.search.clone(),
            thread_kind: self.kind.clone(),
        })
    }
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
            let q = q.catalog()?;
            let guard = app.core.lock_account(&a).await?;
            let value = work(move || {
                let _guard = guard;
                app.core.catalog_page(&a, stringify!($method), None, &q)
            })
            .await?;
            match stringify!($method) {
                "threads" => checked::<contracts::NativePage<contracts::NativeThread>>(value),
                "contacts" => checked::<contracts::NativePage<contracts::NativeContact>>(value),
                "groups" => checked::<contracts::NativePage<contracts::NativeGroup>>(value),
                _ => Err(ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_server_response",
                )),
            }
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
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || {
            let _guard = guard;
            app.core.profile_schema(&a)
        })
        .await?,
    ))
}
async fn members(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, room)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let q = q.catalog()?;
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(contracts::checked::<
        contracts::NativePage<contracts::NativeGroupMember>,
    >(
        work(move || {
            let _guard = guard;
            app.core.catalog_page(&a, "members", Some(&room), &q)
        })
        .await?,
    )?))
}
async fn messages(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, thread)): Path<(String, String)>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let q = q.catalog()?;
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(contracts::checked::<
        contracts::NativePage<contracts::NativeMessage>,
    >(
        work(move || {
            let _guard = guard;
            app.core.catalog_page(&a, "messages", Some(&thread), &q)
        })
        .await?,
    )?))
}
async fn archive_messages(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let q = q.catalog()?;
    let guard = app.core.lock_account(&a).await?;
    checked::<contracts::NativePage<contracts::NativeMessage>>(
        work(move || {
            let _guard = guard;
            app.core.catalog_page(&a, "messages", None, &q)
        })
        .await?,
    )
}
async fn message(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let guard = app.core.lock_account(&a).await?;
    checked::<contracts::NativeMessage>(
        work(move || {
            let _guard = guard;
            app.core.message(&a, &id)
        })
        .await?,
    )
}
async fn quoted_message(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let guard = app.core.lock_account(&a).await?;
    checked::<contracts::NativeQuoteResolution>(
        work(move || {
            let _guard = guard;
            app.core.quoted_message(&a, &id)
        })
        .await?,
    )
}
async fn avatar(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    c.require(Some(&a), "media")?;
    let guard = app.core.lock_account(&a).await?;
    let media = work(move || {
        let _guard = guard;
        app.core.avatar(&a, &id)
    })
    .await?;
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
    let guard = app.core.lock_account(&a).await?;
    let media = work(move || {
        let _guard = guard;
        app.core.image(&a, &id, quality)
    })
    .await?;
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
    let guard = app.core.lock_account(&a).await?;
    Ok(Json(
        work(move || {
            let _guard = guard;
            app.core.recover_projection(&a)
        })
        .await?,
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextSend {
    thread_id: String,
    text: String,
}
fn idempotency_key(headers: &HeaderMap) -> ApiResult<&str> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
        ))?;
    if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_idempotency_key"));
    }
    Ok(key)
}
async fn send_status(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "send")?;
    let value = match app.core.store.send(&a, &id) {
        Ok(send) => serde_json::to_value(send),
        Err(_) => serde_json::to_value(app.core.media_job(&a, &id)?),
    };
    Ok(Json(value.map_err(anyhow::Error::from)?))
}

async fn read_state(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, thread)): Path<(String, String)>,
    Json(input): Json<contracts::NativeReadRequest>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read-state")?;
    let result = app
        .core
        .mark_thread_read(&a, &thread, &input.expected_version)
        .await?;
    Ok(Json(result))
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
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
) -> ApiResult<Response> {
    if headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|part| part.trim().split(';').next() == Some("text/event-stream"))
        })
    {
        return stream(State(app), Extension(c), Path(a), headers, Query(q)).await;
    }
    c.require(Some(&a), "read")?;
    let after = decimal(q.after.as_deref())?;
    let items = app.core.store.events(&a, after, q.limit.unwrap_or(100))?;
    Ok(Json(
        json!({"items":items,"nextCursor":items.last().map(|v|v.cursor.clone()).unwrap_or_else(||after.to_string())}),
    ).into_response())
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
    let routes = Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/status", get(status))
        .route("/v1/daemon/shutdown", post(shutdown_daemon))
        .route("/v1/tokens", get(tokens).post(issue_token))
        .route("/v1/access-tokens", get(tokens).post(issue_token))
        .route("/v1/tokens/{id}", axum::routing::delete(revoke_token))
        .route(
            "/v1/access-tokens/{id}",
            axum::routing::delete(revoke_token),
        )
        .route("/v1/accounts", get(accounts).post(create_account))
        .route("/v1/profiles/official", post(prepare_official_profile))
        .route(
            "/v1/accounts/{a}",
            get(account).patch(actions::update_account),
        )
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
        .route("/v1/accounts/{a}/messages", get(archive_messages))
        .route("/v1/accounts/{a}/messages/{id}", get(message))
        .route("/v1/accounts/{a}/messages/{id}/quote", get(quoted_message))
        .route("/v1/accounts/{a}/avatars/{id}", get(avatar))
        .route("/v1/accounts/{a}/messages/{id}/image", get(image))
        .route("/v1/accounts/{a}/sends", post(media::send))
        .route("/v1/accounts/{a}/sends/{id}", get(send_status))
        .route(
            "/v1/accounts/{a}/threads/{thread}/read-state",
            post(read_state),
        )
        .route("/v1/accounts/{a}/events", get(events))
        .route("/v1/accounts/{a}/event-log", get(events))
        .route("/v1/accounts/{a}/events/stream", get(stream))
        .route("/v1/accounts/{a}/consumers/{consumer}/ack", post(ack))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .merge(media::routes())
        .merge(actions::routes())
        .with_state(app.clone());
    // Resolve aliases before route matching so scope checks and vault AAD always
    // use the canonical account identifier.
    Router::new()
        .fallback_service(routes)
        .layer(middleware::from_fn_with_state(app, authorize))
}
pub fn read_token(path: &FsPath) -> Result<String> {
    let file = wechat_platform::OpenOptions::new()
        .read(true)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(path)
        .context("cannot open API token file")?;
    let meta = file.metadata().context("cannot read API token metadata")?;
    if !meta.is_file() || !wechat_platform::private_file(&file)? || meta.len() > 1024 {
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
    match wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            writeln!(file, "{token}")?;
            file.sync_all()?;
            wechat_platform::sync_dir(parent)?;
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
    #[test]
    fn admission_codes_survive_typed_profile_error_context() {
        for (cause, code, expected) in [
            (
                wechat_profile::ProfileError::KeyMismatch("contact.db".into()),
                "account_database_password_mismatch",
                StatusCode::CONFLICT,
            ),
            (
                wechat_profile::ProfileError::DatabaseOpen("contact.db".into()),
                "profile_database_unavailable",
                StatusCode::CONFLICT,
            ),
            (
                wechat_profile::ProfileError::SourceChanged,
                "profile_changed_during_login",
                StatusCode::CONFLICT,
            ),
            (
                wechat_profile::ProfileError::SqlcipherUnavailable,
                "profile_cipher_unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let ApiError(status, actual) = ApiError::from(anyhow::Error::new(cause).context(code));
            assert_eq!(status, expected);
            assert_eq!(actual, code);
        }
    }
    #[tokio::test]
    async fn shutdown_requires_owner_and_matching_process_store_generation() {
        let (_temp, core, app) = setup();
        let body = json!({"pid":std::process::id(),"serverId":core.store.server_id,"ownershipGeneration":core.ownership_generation});
        let scoped = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        core.store
            .issue_token(
                "scoped-admin",
                &format!("{:x}", Sha256::digest(scoped.as_bytes())),
                &["*".into()],
                &["admin".into()],
                None,
            )
            .unwrap();
        for (token, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(scoped), StatusCode::FORBIDDEN),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "POST",
                    "/v1/daemon/shutdown",
                    token,
                    Some(body.clone())
                )
                .await
                .0,
                expected
            );
            assert!(!core.stopping.load(std::sync::atomic::Ordering::SeqCst));
        }
        let mut wrong = body.clone();
        wrong["ownershipGeneration"] = json!("stale");
        assert_eq!(
            call(
                app.clone(),
                "POST",
                "/v1/daemon/shutdown",
                Some(OWNER),
                Some(wrong)
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert!(!core.stopping.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            call(app, "POST", "/v1/daemon/shutdown", Some(OWNER), Some(body))
                .await
                .0,
            StatusCode::OK
        );
        assert!(core.stopping.load(std::sync::atomic::Ordering::SeqCst));
    }
    const OWNER: &str = "0123456789abcdef0123456789abcdef";
    #[test]
    fn attachment_discovery_errors_keep_exact_media_codes() {
        for code in [
            "unsupported_application_media",
            "unsupported_attachment_encryption",
            "unsupported_attachment_transport",
            "media_metadata_invalid",
            "media_metadata_limit",
            "media_key_missing",
        ] {
            let ApiError(status, actual) =
                ApiError::from(anyhow::anyhow!("{code}").context("bounded media discovery"));
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(actual, code);
        }
        let ApiError(_, actual) = ApiError::from(anyhow::anyhow!(
            "unsupported_attachment_transport https://private.invalid/file"
        ));
        assert_eq!(actual, "unsupported_profile");
    }
    fn setup() -> (tempfile::TempDir, Arc<Core>, Router) {
        let t = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("two").unwrap();
        let core = Core::new(store, None).unwrap();
        let app = router(core.clone(), OWNER);
        (t, core, app)
    }
    #[tokio::test]
    async fn recall_requires_account_send_scope_and_an_exact_string_server_id() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let send = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(read, "read"), (send, "send")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        for (account, token, expected) in [
            ("one", None, StatusCode::UNAUTHORIZED),
            ("one", Some(read), StatusCode::FORBIDDEN),
            ("two", Some(send), StatusCode::FORBIDDEN),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "POST",
                    &format!("/v1/accounts/{account}/messages/opaque/recall"),
                    token,
                    Some(json!({"serverId":"18446744073709551615"}))
                )
                .await
                .0,
                expected
            );
        }
        let missing_key = call(
            app.clone(),
            "POST",
            "/v1/accounts/one/messages/opaque/recall",
            Some(send),
            Some(json!({"serverId":"18446744073709551615"})),
        )
        .await;
        assert_eq!(missing_key.0, StatusCode::BAD_REQUEST);
        for value in [
            "0",
            "-1",
            "01",
            "18446744073709551616",
            " 23",
            "23 ",
            "2.3",
            "",
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/accounts/one/messages/opaque/recall")
                .header("authorization", format!("Bearer {send}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "synthetic-recall")
                .body(Body::from(
                    serde_json::to_vec(&json!({"serverId":value})).unwrap(),
                ))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{value:?}");
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["error"]["code"], "invalid_recall_server_id");
        }
        for body in [
            json!({}),
            json!({"serverId":9007199254740993u64}),
            json!({"serverId":"23", "threadId":"caller-selected"}),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "POST",
                    "/v1/accounts/one/messages/opaque/recall",
                    Some(send),
                    Some(body)
                )
                .await
                .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    }
    #[tokio::test]
    async fn recall_operation_inspection_requires_send_scope_in_the_same_account() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let send = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(read, "read"), (send, "send")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        core.store
            .save_operation(&wechat_store::Operation {
                id: "recall-fixture".into(),
                account_id: "one".into(),
                kind: "message_recall".into(),
                state: "needs_review".into(),
                result: json!({"status":"needs_review"}),
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            })
            .unwrap();
        for (account, token, expected) in [
            ("one", Some(read), StatusCode::FORBIDDEN),
            ("two", Some(send), StatusCode::FORBIDDEN),
            ("one", Some(send), StatusCode::OK),
            ("one", Some(OWNER), StatusCode::OK),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "GET",
                    &format!("/v1/accounts/{account}/operations/recall-fixture"),
                    token,
                    None
                )
                .await
                .0,
                expected
            );
        }
        let cancelled = call(
            app.clone(),
            "DELETE",
            "/v1/accounts/one/operations/recall-fixture",
            Some(send),
            None,
        )
        .await;
        assert_eq!(cancelled.0, StatusCode::CONFLICT);
        assert_eq!(cancelled.1["error"]["code"], "operation_cannot_cancel");
        assert_eq!(
            core.store.operation("one", "recall-fixture").unwrap().state,
            "needs_review"
        );
    }
    #[test]
    fn recalled_media_has_a_terminal_public_error() {
        let ApiError(status, code) = ApiError::from(anyhow::anyhow!("message_recalled"));
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(code, "message_recalled");
        for expected in ["media_identity_unavailable", "media_source_changed"] {
            let ApiError(status, code) = ApiError::from(anyhow::anyhow!(expected));
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(code, expected);
        }
    }
    #[tokio::test]
    async fn account_message_search_requires_account_read_scope_and_bounded_query() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let media = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(read, "read"), (media, "media")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        for (account, token, expected) in [
            ("one", None, StatusCode::UNAUTHORIZED),
            ("one", Some(media), StatusCode::FORBIDDEN),
            ("two", Some(read), StatusCode::FORBIDDEN),
            ("one", Some(read), StatusCode::BAD_REQUEST),
            ("one", Some(OWNER), StatusCode::BAD_REQUEST),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "GET",
                    &format!("/v1/accounts/{account}/messages?query=match&limit=0"),
                    token,
                    None
                )
                .await
                .0,
                expected
            );
        }
    }
    #[tokio::test]
    async fn share_send_validates_scope_and_metadata_before_account_or_network_access() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        core.store.issue_token("read", &format!("{:x}", Sha256::digest(read.as_bytes())), &["one".into()], &["read".into()], None).unwrap();
        let share = json!({"kind":"share","threadId":"peer","title":"Shared title","url":"https://example.invalid/article"});
        for (token, expected) in [(None, StatusCode::UNAUTHORIZED), (Some(read), StatusCode::FORBIDDEN)] {
            assert_eq!(call(app.clone(), "POST", "/v1/accounts/one/sends", token, Some(share.clone())).await.0, expected);
        }
        for update in [json!({"title":""}), json!({"url":"file:///private"}), json!({"thumbnailUrl":"http://127.0.0.1/private"}), json!({"sourceId":"must-not-be-a-file"}), json!({"description":12})] {
            let mut body = share.clone();
            body.as_object_mut().unwrap().extend(update.as_object().unwrap().clone());
            let response = app.clone().oneshot(Request::builder().method("POST").uri("/v1/accounts/one/sends")
                .header("authorization", format!("Bearer {OWNER}"))
                .header("content-type", "application/json").header("idempotency-key", "share-fixture")
                .body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        assert!(core.store.queued_sends("one").unwrap().is_empty());
    }
    #[tokio::test]
    async fn quote_lookup_checks_account_read_scope_before_profile_io() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let media = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(read, "read"), (media, "media")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        for (account, token, expected) in [
            ("one", None, StatusCode::UNAUTHORIZED),
            ("one", Some(media), StatusCode::FORBIDDEN),
            ("two", Some(read), StatusCode::FORBIDDEN),
        ] {
            assert_eq!(
                call(
                    app.clone(),
                    "GET",
                    &format!("/v1/accounts/{account}/messages/parent/quote"),
                    token,
                    None
                )
                .await
                .0,
                expected
            );
        }
    }
    #[tokio::test]
    async fn official_profile_preparation_requires_global_admin_before_reading_files() {
        let (_temp, core, app) = setup();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let admin = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(read, "read"), (admin, "admin")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        for token in [None, Some(read), Some(admin)] {
            let expected = if token.is_none() {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(
                call(
                    app.clone(),
                    "POST",
                    "/v1/profiles/official",
                    token,
                    Some(json!({"path":"relative","accountId":null}))
                )
                .await
                .0,
                expected
            );
        }
        let (status, error) = call(
            app.clone(),
            "POST",
            "/v1/profiles/official",
            Some(OWNER),
            Some(json!({"path":"relative","accountId":null})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["error"]["code"],
            "official_profile_path_must_be_absolute"
        );
        assert_eq!(core.store.accounts().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn cached_media_writeback_repair_is_bounded_durable_and_media_scoped() {
        let (_temp, core, app) = setup();
        let media = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let read = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for (token, scope) in [(media, "media"), (read, "read")] {
            core.store
                .issue_token(
                    scope,
                    &format!("{:x}", Sha256::digest(token.as_bytes())),
                    &["one".into()],
                    &[scope.into()],
                    None,
                )
                .unwrap();
        }
        let path = "/v1/accounts/one/media-writeback/repair";
        for limit in [0, 101] {
            assert_eq!(
                call(
                    app.clone(),
                    "POST",
                    path,
                    Some(media),
                    Some(json!({"limit":limit}))
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            call(
                app.clone(),
                "POST",
                path,
                Some(read),
                Some(json!({"limit":25}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, operation) = call(
            app.clone(),
            "POST",
            path,
            Some(media),
            Some(json!({"limit":25})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(operation["kind"], "media-writeback");
        let lookup = format!(
            "/v1/accounts/one/operations/{}",
            operation["id"].as_str().unwrap()
        );
        assert_eq!(
            call(app.clone(), "GET", &lookup, Some(media), None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(app.clone(), "GET", &lookup, Some(read), None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                app,
                "POST",
                "/v1/accounts/two/media-writeback/repair",
                Some(media),
                Some(json!({"limit":25}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert!(core.store.queued_sends("one").unwrap().is_empty());
    }
    #[tokio::test]
    async fn aliases_resolve_before_account_grant_checks() {
        let (_temp, core, app) = setup();
        core.store.set_account_alias("one", "personal").unwrap();
        let (_, account) = call(
            app.clone(),
            "GET",
            "/v1/accounts/personal",
            Some(OWNER),
            None,
        )
        .await;
        assert_eq!(account["id"], "one");
        let token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        core.store
            .issue_token(
                "other-account",
                &format!("{:x}", Sha256::digest(token.as_bytes())),
                &["two".into()],
                &["read".into()],
                None,
            )
            .unwrap();
        assert_eq!(
            call(app, "GET", "/v1/accounts/personal", Some(token), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    #[tokio::test]
    async fn multipart_uploads_are_owned_bounded_and_never_return_original_bytes() {
        let (_temp, _core, app) = setup();
        let content = "private synthetic file";
        let body=format!("--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n--test-boundary--\r\n");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/accounts/one/uploads")
                    .header("authorization", format!("Bearer {OWNER}"))
                    .header(
                        "content-type",
                        "multipart/form-data; boundary=test-boundary",
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let source: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(source["byteLength"], content.len());
        assert_eq!(
            source["sha256"],
            format!("{:x}", Sha256::digest(content.as_bytes()))
        );
        assert!(!String::from_utf8_lossy(&body).contains(content));
        let oversized = "x".repeat(wechat_core::MAX_SOURCE_BYTES + 1);
        let body=format!("--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"large\"\r\n\r\n{oversized}\r\n--test-boundary--\r\n");
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/accounts/one/uploads")
                    .header("authorization", format!("Bearer {OWNER}"))
                    .header(
                        "content-type",
                        "multipart/form-data; boundary=test-boundary",
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
    #[tokio::test]
    async fn unauthenticated_logout_still_has_a_durable_receipt_and_fence() {
        let (_temp, core, app) = setup();
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/v1/accounts/one/logout")
                .header("authorization", format!("Bearer {OWNER}"))
                .header("idempotency-key", "logout-test")
                .body(Body::empty())
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let first: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(core.store.runtime("one", "logged-out").unwrap().is_some());
        assert!(core.store.runtime("two", "logged-out").unwrap().is_none());
        let response = app.oneshot(request()).await.unwrap();
        let repeat: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(first["id"], repeat["id"]);
    }
    #[tokio::test]
    async fn saved_only_login_retries_return_one_operation_without_a_network_login() {
        let (_temp, core, app) = setup();
        let body = json!({"strategy":"saved-device","allowQr":false});
        let request = |body: &Value| {
            Request::builder()
                .method("POST")
                .uri("/v1/accounts/one/login-attempts")
                .header("authorization", format!("Bearer {OWNER}"))
                .header("idempotency-key", "login-retry-test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap()
        };
        let first = app.clone().oneshot(request(&body)).await.unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let repeated = app.clone().oneshot(request(&body)).await.unwrap();
        assert_eq!(repeated.status(), StatusCode::ACCEPTED);
        let repeated: Value =
            serde_json::from_slice(&repeated.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(first["id"], repeated["id"]);
        let mut different = body;
        different["computerName"] = json!("other-name");
        assert_eq!(
            app.oneshot(request(&different)).await.unwrap().status(),
            StatusCode::CONFLICT
        );
        for _ in 0..20 {
            if core
                .store
                .operation("one", first["id"].as_str().unwrap())
                .unwrap()
                .state
                == "needs_login"
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("saved-only empty account did not reach needs_login");
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
