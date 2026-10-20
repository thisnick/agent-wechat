//! A reusable authenticated router for capabilities and registered profile inventory.
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
};
use subtle::ConstantTimeEq;
use wechat_store::Store;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    product: &'static str,
    backend: &'static str,
    version: &'static str,
    stage: &'static str,
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
        stage: "foundation",
        api_version: 1,
        capabilities: vec!["serve", "profile-attach", "profile-copy", "config"],
        native_login: false,
        official_database_writes: false,
    }
}

#[derive(Clone)]
struct App {
    store: Arc<Store>,
    token_hash: [u8; 32],
}

async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":{"code":"unauthorized"}})),
        )
            .into_response();
    };
    let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if !bool::from(candidate.ct_eq(&app.token_hash)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":{"code":"unauthorized"}})),
        )
            .into_response();
    }
    next.run(request).await
}
async fn status(State(app): State<App>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"serverId":app.store.server_id,"version":version(),"state":"ready","nativeConnection":"not_implemented"}),
    )
}
async fn accounts(State(app): State<App>) -> Response {
    // Registry work is small and bounded here; protocol/large profile work must not
    // enter these handlers when those capabilities are added.
    match app.store.accounts() {
        Ok(items) => Json(serde_json::json!({"items":items})).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":{"code":"registry_unavailable"}})),
        )
            .into_response(),
    }
}
pub fn router(store: Arc<Store>, token: &str) -> Router {
    let state = App {
        store,
        token_hash: Sha256::digest(token.as_bytes()).into(),
    };
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/accounts", get(accounts))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

pub fn read_token(path: &Path) -> Result<String> {
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
pub fn provision_token(path: &Path, allow_create: bool) -> Result<String> {
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
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn authorization_and_capabilities_are_real_and_explicit() {
        let t = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&t.path().join("data")).unwrap());
        let app = router(store, "0123456789abcdef0123456789abcdef");
        for token in [None, Some("wrong")] {
            let mut req = Request::builder().uri("/v1/accounts");
            if let Some(token) = token {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            let response = app
                .clone()
                .oneshot(req.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header("authorization", "Bearer 0123456789abcdef0123456789abcdef")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data = response.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(v["version"]["nativeLogin"], false);
        assert_eq!(v["version"]["officialDatabaseWrites"], false);
    }
    #[test]
    fn token_is_private_persistent_and_never_replaced_on_failure() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("private/token");
        let first = provision_token(&p, true).unwrap();
        assert_eq!(first, provision_token(&p, true).unwrap());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(provision_token(&p, true).is_err());
        assert_eq!(fs::read_to_string(p).unwrap().trim(), first);
    }
}
