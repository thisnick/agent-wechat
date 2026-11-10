//! Loopback host for a Mini Program prepared by the selected hub.

mod request;

use super::{bounded_bytes, Remote};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, io::Write, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::OnceCell;
use uuid::Uuid;
use wechat_api::contracts::{
    NativeMiniProgramIpcRequest, NativeMiniProgramLaunch, NativeMiniProgramLogin,
    NativeMiniProgramPackage,
};
use wechat_miniprogram::{runner, valid_app_id, Archive, MAX_PACKAGE_BYTES};

const JS: &str = "text/javascript; charset=utf-8";

#[derive(Subcommand)]
pub(super) enum MiniProgramCommand {
    /// Fetch a missing main package from WeChat into the hub's cache (experimental).
    Fetch {
        /// Public AppID of the Mini Program to download.
        app: String,
    },
    /// Print a local URL and serve a verified Mini Program until interrupted (experimental).
    Serve {
        /// Public AppID of the Mini Program in the hub's cache.
        app: String,
        /// Saved Mini Program card to open instead of the default page.
        #[arg(long)]
        message: Option<String>,
        /// Restrict server-approved requests and display images to exact HTTPS URLs. Repeat and omit queries.
        #[arg(long = "allow-request-url", value_name = "HTTPS_URL")]
        allowed_requests: Vec<String>,
    },
}

struct Login {
    remote: Arc<Remote>,
    path: String,
}

struct Runner {
    host: String,
    origin: String,
    page: String,
    modules: String,
    archive: Archive,
    service: String,
    allow_wasm: bool,
    login: Option<Login>,
    network: Option<request::Network>,
    key: Option<String>,
    remote: Arc<Remote>,
    app: String,
    root: String,
    subpackages: BTreeMap<String, Subpackage>,
}

struct Subpackage {
    package: NativeMiniProgramPackage,
    modules: OnceCell<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostStart {
    version: u8,
    endpoint: String,
    token: String,
    certificate: Option<String>,
    account_id: String,
    app_id: String,
    source_message_id: Option<String>,
}

pub(super) async fn machine_host() -> Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut input = Vec::new();
    (&mut stdin)
        .take(65536)
        .read_until(b'\n', &mut input)
        .await?;
    ensure!(
        input.ends_with(b"\n"),
        "Invalid Mini Program host configuration"
    );
    let start: HostStart =
        serde_json::from_slice(&input).context("Invalid Mini Program host configuration")?;
    ensure!(start.version == 1, "Unsupported Mini Program host protocol");
    let remote = Remote::with_connection(
        wechat_config::ClientConfig {
            schema_version: 1,
            endpoint: start.endpoint,
            token_file: Default::default(),
            account: Some(start.account_id),
        },
        Some(start.token),
        start.certificate.as_deref(),
        true,
    )?;
    // The startup record is the only input. Any further byte or EOF closes the host.
    tokio::select! {
        outcome = serve(remote, start.app_id, start.source_message_id, Vec::new(), true) => outcome,
        _ = stdin.read_u8() => Ok(()),
    }
}

pub(super) async fn run(remote: Remote, command: MiniProgramCommand, json: bool) -> Result<()> {
    match command {
        MiniProgramCommand::Fetch { app } => {
            ensure!(valid_app_id(&app), "Expected a Mini Program AppID");
            let path = remote.path(&format!("mini-programs/{app}/packages"))?;
            let reply = remote.request(Method::POST, &path, None, None).await?;
            if json {
                println!("{}", serde_json::to_string(&reply)?);
            } else {
                let package: wechat_api::contracts::NativeMiniProgramPackage =
                    serde_json::from_value(reply)?;
                println!(
                    "Mini Program {app} version {} ready in the hub cache.",
                    package.version
                );
            }
            Ok(())
        }
        MiniProgramCommand::Serve {
            app,
            message,
            allowed_requests,
        } => tokio::select! {
            outcome = serve(remote, app, message, allowed_requests, json) => outcome,
            signal = tokio::signal::ctrl_c() => Ok(signal?),
        },
    }
}

async fn serve(
    remote: Remote,
    app: String,
    message: Option<String>,
    allowed_requests: Vec<String>,
    json: bool,
) -> Result<()> {
    ensure!(valid_app_id(&app), "Expected a Mini Program AppID");
    let root = remote.path(&format!("mini-programs/{app}"))?;
    let launch_path = format!("{root}/launches");
    let launch = remote
        .response(
            Method::POST,
            &launch_path,
            Some(json!({ "sourceMessageId": message })),
            None,
        )
        .await?;
    let launch: NativeMiniProgramLaunch =
        serde_json::from_slice(&bounded_bytes(launch, 1024 * 1024).await?)?;
    let network = request::Network::new(
        launch
            .network_policy
            .as_ref()
            .map(|policy| policy.request_domains.clone()),
        allowed_requests,
    )?;
    ensure!(
        launch.packages.len() <= 65,
        "Too many Mini Program packages"
    );
    let mut packages = launch.packages.into_iter();
    let package = packages
        .next()
        .context("Missing Mini Program main archive")?;
    ensure!(package.root.is_none(), "Missing Mini Program main archive");
    let mut subpackages = BTreeMap::new();
    let mut roots = BTreeMap::new();
    for subpackage in packages {
        validate_package(&subpackage)?;
        let subroot = subpackage
            .root
            .as_deref()
            .context("Missing Mini Program subpackage root")?;
        ensure!(
            subpackage.id != package.id
                && subpackage.version == package.version
                && wechat_miniprogram::split_page_path(subroot) == Some((subroot, ""))
                && !subpackages.contains_key(&subpackage.id)
                && !roots.contains_key(subroot),
            "Invalid Mini Program subpackage selection"
        );
        roots.insert(subroot.to_owned(), subpackage.id.clone());
        subpackages.insert(
            subpackage.id.clone(),
            Subpackage {
                package: subpackage,
                modules: OnceCell::new(),
            },
        );
    }
    let archive = selected_archive(&remote, &root, &app, &package).await?;
    let mut page = runner::page_module(
        &archive,
        &launch.entry.path,
        &launch.entry.service_path,
        &launch.entry.query,
        launch.capabilities.login,
    )?;
    page.push_str(&format!(
        "export const appId = {};\nexport const subpackages = new Map({});\n",
        serde_json::to_string(&app)?,
        serde_json::to_string(&roots.into_iter().collect::<Vec<_>>())?
    ));
    if network
        .as_ref()
        .is_some_and(request::Network::requests_enabled)
    {
        page.push_str("export const allowRequest = true;\n");
    }
    page.push_str("export const allowImages = true;\n");
    let allow_wasm = runner::has_wasm(&archive);
    if allow_wasm {
        page.push_str("export const allowWasm = true;\n");
    }
    let modules = runner::module_registry(&archive, &launch.entry.service_path)?;
    let remote = Arc::new(remote);
    let login = if launch.capabilities.login {
        let id = Uuid::parse_str(&launch.launch_id).context("Invalid Mini Program launch ID")?;
        Some(Login {
            remote: remote.clone(),
            path: format!("{launch_path}/{id}/ipc"),
        })
    } else {
        None
    };
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let host = listener.local_addr()?.to_string();
    let origin = format!("http://{host}");
    let key = (login.is_some() || network.is_some()).then(|| Uuid::new_v4().simple().to_string());
    let url = match &key {
        Some(key) => format!("{origin}/#{key}"),
        None => format!("{origin}/"),
    };
    let runner = Router::new().fallback(handle).with_state(Arc::new(Runner {
        host,
        origin,
        page,
        modules,
        archive,
        service: launch.entry.service_path,
        allow_wasm,
        login,
        network,
        key,
        remote,
        app: app.clone(),
        root,
        subpackages,
    }));
    if json {
        println!(
            "{}",
            json!({"version": 1, "state": "ready", "appId": app, "url": url, "sessionId": launch.launch_id, "capabilities": launch.capabilities})
        );
    } else {
        println!("Mini Program ready at {url}\nPress Ctrl-C to stop.");
    }
    std::io::stdout().flush()?;
    axum::serve(listener, runner).await?;
    Ok(())
}

fn validate_package(package: &NativeMiniProgramPackage) -> Result<()> {
    ensure!(
        (1..=32).contains(&package.id.len())
            && package
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && (1..=20).contains(&package.version.len())
            && package.version.bytes().all(|b| b.is_ascii_digit())
            && (18..=MAX_PACKAGE_BYTES).contains(&(package.size_bytes as usize)),
        "Invalid Mini Program package selection"
    );
    Ok(())
}

async fn selected_archive(
    remote: &Remote,
    root: &str,
    app: &str,
    package: &NativeMiniProgramPackage,
) -> Result<Archive> {
    validate_package(package)?;
    let path = format!("{root}/packages/{}/{}", package.version, package.id);
    let response = remote.response(Method::GET, &path, None, None).await?;
    let size = package.size_bytes as usize;
    let bytes = bounded_bytes(response, size).await?;
    ensure!(bytes.len() == size, "Mini Program package length mismatch");
    Archive::open(app, bytes, &package.sha256)
}

async fn subpackage_module(runner: &Runner, id: &str) -> Response {
    let Some(subpackage) = runner.subpackages.get(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match subpackage
        .modules
        .get_or_try_init(|| async {
            let archive = selected_archive(
                &runner.remote,
                &runner.root,
                &runner.app,
                &subpackage.package,
            )
            .await?;
            let root = subpackage
                .package
                .root
                .clone()
                .context("Missing Mini Program subpackage root")?;
            tokio::task::spawn_blocking(move || runner::subpackage_module_registry(&archive, &root))
                .await?
        })
        .await
    {
        Ok(modules) => asset(JS, modules),
        Err(_) => StatusCode::BAD_GATEWAY.into_response(),
    }
}

async fn handle(State(runner): State<Arc<Runner>>, request: Request) -> Response {
    let headers = request.headers();
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(&runner.host) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let path = request.uri().path();
    let response = match (request.method(), path) {
        (&Method::GET, "/page.js") => asset(JS, &runner.page),
        (&Method::GET, "/modules.js") => asset(JS, &runner.modules),
        (&Method::GET, path) if path.starts_with("/_wx/subpackages/") => {
            match path["/_wx/subpackages/".len()..].strip_suffix(".js") {
                Some(id) => subpackage_module(&runner, id).await,
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        (&Method::GET, path) if path.starts_with("/_wx/pages/") => {
            let entry = &path["/_wx/pages/".len()..];
            match runner::page_module(
                &runner.archive,
                entry,
                &runner.service,
                "",
                runner.login.is_some(),
            ) {
                Ok(page) => asset(JS, &page),
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
        (&Method::POST, "/_wx/ipc") => {
            let Some(login) = runner.login.as_ref() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Err(status) = authorize_post(&runner, headers) {
                return status.into_response();
            }
            let Some(invocation) = headers
                .get("idempotency-key")
                .and_then(|key| key.to_str().ok())
                .and_then(|key| Uuid::parse_str(key).ok())
            else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Ok(bytes) = axum::body::to_bytes(request.into_body(), 1024).await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let Ok(NativeMiniProgramIpcRequest::Login {}) = serde_json::from_slice(&bytes) else {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response();
            };
            match request_code(login, invocation).await {
                Ok(code) => Json(code).into_response(),
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
        (&Method::POST, "/_wx/request") => {
            let Some(network) = &runner.network else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Err(status) = authorize_post(&runner, headers) {
                return status.into_response();
            }
            let Ok(bytes) =
                axum::body::to_bytes(request.into_body(), request::MAX_REQUEST_BYTES).await
            else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            let Ok(request) = serde_json::from_slice(&bytes) else {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response();
            };
            match network.send(request).await {
                Ok(reply) => Json(reply).into_response(),
                Err(status) => status.into_response(),
            }
        }
        (&Method::POST, "/_wx/image") => {
            let Some(network) = runner.network.as_ref() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if let Err(status) = authorize_post(&runner, headers) {
                return status.into_response();
            }
            let Ok(bytes) = axum::body::to_bytes(request.into_body(), 16384).await else {
                return StatusCode::PAYLOAD_TOO_LARGE.into_response();
            };
            let Ok(input) = serde_json::from_slice(&bytes) else {
                return StatusCode::UNPROCESSABLE_ENTITY.into_response();
            };
            match network.image(input).await {
                Ok(reply) => Json(reply).into_response(),
                Err(status) => status.into_response(),
            }
        }
        (&Method::GET, path) => match runner::asset(path) {
            Some((mime, content)) => asset(mime, content),
            None => runner::package_asset(&runner.archive, path.trim_start_matches('/'))
                .map_or_else(
                    || StatusCode::NOT_FOUND.into_response(),
                    |(mime, data)| ([(header::CONTENT_TYPE, mime)], data.to_vec()).into_response(),
                ),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = response;
    let policy = if runner.allow_wasm {
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
    } else if runner.key.is_some() {
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
    } else {
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(policy),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn authorize_post(runner: &Runner, headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(runner.origin.as_str())
        || !runner.key.as_deref().is_some_and(|key| {
            headers
                .get("x-wx-launch")
                .and_then(|value| value.to_str().ok())
                == Some(key)
        })
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some("application/json")
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    Ok(())
}

fn asset(content_type: &'static str, content: &str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], content.to_owned()).into_response()
}

async fn request_code(login: &Login, invocation: Uuid) -> Result<NativeMiniProgramLogin> {
    let reply = login
        .remote
        .response(
            Method::POST,
            &login.path,
            Some(json!(NativeMiniProgramIpcRequest::Login {})),
            Some(&invocation.to_string()),
        )
        .await?;
    let code: NativeMiniProgramLogin = serde_json::from_slice(&bounded_bytes(reply, 4096).await?)?;
    ensure!(
        (1..=1024).contains(&code.code.len()) && code.code.bytes().all(|b| b.is_ascii_graphic()),
        "Invalid Mini Program login reply"
    );
    Ok(code)
}
