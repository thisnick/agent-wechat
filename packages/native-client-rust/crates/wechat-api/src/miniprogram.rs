//! Account-scoped Mini Program launch and package endpoints.

use super::*;
use axum::{
    extract::DefaultBodyLimit,
    http::header::{CACHE_CONTROL, CONTENT_TYPE},
};
use contracts::{
    NativeMiniProgram, NativeMiniProgramCapabilities, NativeMiniProgramEntry,
    NativeMiniProgramIpcRequest, NativeMiniProgramLaunch, NativeMiniProgramLaunchRequest,
    NativeMiniProgramLogin, NativeMiniProgramNetworkPolicy, NativeMiniProgramPackage,
    NativeMiniPrograms,
};
use uuid::Uuid;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .api_route("/v1/accounts/{a}/mini-programs", get(list))
        .api_route(
            "/v1/accounts/{a}/mini-programs/{app}/launches",
            post(launch),
        )
        .api_route(
            "/v1/accounts/{a}/mini-programs/{app}/packages",
            post(fetch_package),
        )
        .api_route(
            "/v1/accounts/{a}/mini-programs/{app}/packages/{version}/{package}",
            get(package),
        )
        .api_route(
            "/v1/accounts/{a}/mini-programs/{app}/launches/{id}/ipc",
            post(ipc).layer(DefaultBodyLimit::max(1024)),
        )
}

pub(super) fn error(error: &anyhow::Error) -> Option<ApiError> {
    error.chain().find_map(|cause| {
        let (status, code) = match cause.to_string().as_str() {
            "invalid_mini_program_app" => (StatusCode::BAD_REQUEST, "invalid_mini_program_app"),
            "mini_program_package_unavailable" => {
                (StatusCode::NOT_FOUND, "mini_program_package_unavailable")
            }
            "mini_program_launch_unavailable" => {
                (StatusCode::NOT_FOUND, "mini_program_launch_unavailable")
            }
            "mini_program_cache_invalid" => (StatusCode::CONFLICT, "mini_program_cache_invalid"),
            "mini_program_page_unsupported" => {
                (StatusCode::CONFLICT, "mini_program_page_unsupported")
            }
            "mini_program_source_unavailable" => {
                (StatusCode::NOT_FOUND, "mini_program_source_unavailable")
            }
            "mini_program_cache_limit" => (StatusCode::CONFLICT, "mini_program_cache_limit"),
            "mini_program_launch_limit" => {
                (StatusCode::TOO_MANY_REQUESTS, "mini_program_launch_limit")
            }
            "mini_program_network_unavailable" => {
                (StatusCode::BAD_GATEWAY, "mini_program_network_unavailable")
            }
            "mini_program_download_forbidden" => {
                (StatusCode::FORBIDDEN, "mini_program_download_forbidden")
            }
            "mini_program_download_offline" => {
                (StatusCode::CONFLICT, "mini_program_download_offline")
            }
            "mini_program_download_failed" => {
                (StatusCode::BAD_GATEWAY, "mini_program_download_failed")
            }
            "mini_program_login_forbidden" => {
                (StatusCode::FORBIDDEN, "mini_program_login_forbidden")
            }
            "mini_program_upstream_login_failed" => (
                StatusCode::BAD_GATEWAY,
                "mini_program_upstream_login_failed",
            ),
            _ => return None,
        };
        Some(ApiError(status, code))
    })
}

fn principal(claims: &Claims) -> &str {
    claims
        .grant
        .as_ref()
        .map_or("owner", |grant| grant.id.as_str())
}

async fn list(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path(account): Path<String>,
) -> ApiResult<Json<NativeMiniPrograms>> {
    claims.require(Some(&account), "read")?;
    let items = work(move || app.core.mini_programs(&account)).await?;
    Ok(Json(NativeMiniPrograms {
        items: items
            .into_iter()
            .map(|item| NativeMiniProgram {
                app_id: item.app_id,
                title: item.title,
                available: item.available,
            })
            .collect(),
    }))
}

async fn fetch_package(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path((account, app_id)): Path<(String, String)>,
) -> ApiResult<Json<NativeMiniProgramPackage>> {
    claims.require(Some(&account), "admin")?;
    let (package, size) = work(move || {
        app.core
            .fetch_mini_program(&account, &app_id, || claims.valid(&app.core.store))
    })
    .await?;
    Ok(Json(NativeMiniProgramPackage {
        id: package.package_id,
        version: package.version,
        size_bytes: size as u32,
        sha256: package.sha256,
        root: None,
    }))
}

async fn launch(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path((account, app_id)): Path<(String, String)>,
    input: Option<Json<NativeMiniProgramLaunchRequest>>,
) -> ApiResult<impl IntoResponse> {
    claims.require(Some(&account), "read")?;
    let can_login = claims.require(Some(&account), "mini-program-login").is_ok();
    let input = input.map(|Json(input)| input).unwrap_or_default();
    let (selected, network_policy) = work(move || {
        let selected = app.core.prepare_mini_program(
            &account,
            &app_id,
            principal(&claims),
            can_login,
            input.source_message_id.as_deref(),
        )?;
        let network = app
            .core
            .mini_program_network_policy(&account, &app_id, || claims.valid(&app.core.store))?;
        Ok((selected, network))
    })
    .await?;
    let metadata = selected.package;
    let mut packages = vec![NativeMiniProgramPackage {
        id: metadata.package_id,
        version: metadata.version.clone(),
        size_bytes: selected.size_bytes as u32,
        sha256: metadata.sha256,
        root: None,
    }];
    packages.extend(
        selected
            .subpackages
            .into_iter()
            .map(|subpackage| NativeMiniProgramPackage {
                id: subpackage.id,
                version: metadata.version.clone(),
                size_bytes: subpackage.size_bytes as u32,
                sha256: subpackage.sha256,
                root: Some(subpackage.root),
            }),
    );
    Ok((
        StatusCode::CREATED,
        [(CACHE_CONTROL, "no-store")],
        Json(NativeMiniProgramLaunch {
            launch_id: selected.id.to_string(),
            network_policy: network_policy.map(|policy| NativeMiniProgramNetworkPolicy {
                request_domains: policy.request_domains,
            }),
            entry: NativeMiniProgramEntry {
                path: metadata.entry_page,
                service_path: metadata.service_path,
                query: selected.query,
                framework: selected.framework.name().into(),
                renderer: "webview".into(),
            },
            packages,
            capabilities: NativeMiniProgramCapabilities {
                login: selected.login,
                share_to_chat: false,
            },
        }),
    ))
}

async fn package(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path((account, app_id, version, id)): Path<(String, String, String, String)>,
) -> ApiResult<impl IntoResponse> {
    claims.require(Some(&account), "read")?;
    let bytes = work(move || {
        app.core
            .mini_program_package(&account, &app_id, &version, &id)
    })
    .await?;
    Ok((
        [
            (CONTENT_TYPE, "application/octet-stream"),
            (CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    ))
}

async fn ipc(
    State(app): State<App>,
    Extension(claims): Extension<Claims>,
    Path((account, app_id, id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(NativeMiniProgramIpcRequest::Login {}): Json<NativeMiniProgramIpcRequest>,
) -> ApiResult<impl IntoResponse> {
    claims.require(Some(&account), "read")?;
    claims.require(Some(&account), "mini-program-login")?;
    let id = Uuid::parse_str(&id)
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "mini_program_launch_unavailable"))?;
    let invocation_id = headers
        .get("idempotency-key")
        .and_then(|key| key.to_str().ok())
        .and_then(|key| Uuid::parse_str(key).ok())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_idempotency_key"))?;
    let code = work(move || {
        app.core.mini_program_login(
            &account,
            &app_id,
            principal(&claims),
            id,
            invocation_id,
            || claims.valid(&app.core.store),
        )
    })
    .await?;
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(NativeMiniProgramLogin {
            code: code.to_string(),
        }),
    ))
}
