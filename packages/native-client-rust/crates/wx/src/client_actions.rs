use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub async fn catalog(r: &Remote, kind: &str, command: CatalogCommand) -> Result<Value> {
    match command {
        CatalogCommand::Avatar {
            id,
            output,
            refresh,
            detach,
            wait,
        } => {
            if kind != "contacts" {
                bail!("unsupported_capability: use contacts avatar for an exact username");
            }
            if refresh {
                let op = r
                    .post(
                        &r.path(&format!("avatars/{}/refresh", encode(&id)))?,
                        json!({}),
                    )
                    .await?;
                return wait_operation(r, op, if detach { 0 } else { wait }).await;
            }
            let path = output.context("--output is required for cached avatar bytes")?;
            let response = r
                .response(
                    reqwest::Method::GET,
                    &r.path(&format!("avatars/{}", encode(&id)))?,
                    None,
                    None,
                )
                .await?;
            let bytes = super::bounded_bytes(response, 16 * 1024 * 1024).await?;
            private_output(&path, &bytes)?;
            Ok(
                json!({"state":"available","output":path,"byteLength":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
            )
        }
        CatalogCommand::List { limit, cursor } => {
            r.get(&query(
                r.path(kind)?,
                &[("limit", limit.map(|v| v.to_string())), ("cursor", cursor)],
            ))
            .await
        }
        CatalogCommand::Get { id } => r.get(&r.path(&format!("{kind}/{}", encode(&id)))?).await,
        CatalogCommand::Find { query: search } => {
            r.get(&query(r.path(kind)?, &[("query", Some(search))]))
                .await
        }
        CatalogCommand::Refresh { id, detach, wait } => {
            let op = r
                .post(
                    &r.path(&format!("{kind}/{}/refresh", encode(&id)))?,
                    json!({}),
                )
                .await?;
            wait_operation(r, op, if detach { 0 } else { wait }).await
        }
        CatalogCommand::Members { id, cursor } => {
            if kind != "groups" {
                bail!("unsupported_capability: only groups have members");
            }
            r.get(&query(
                r.path(&format!("groups/{}/members", encode(&id)))?,
                &[("cursor", cursor)],
            ))
            .await
        }
        CatalogCommand::Sync { detach, wait } => {
            let op = r
                .post(
                    &r.path(if kind == "groups" {
                        "group-sync"
                    } else {
                        "contact-sync"
                    })?,
                    json!({}),
                )
                .await?;
            wait_operation(r, op, if detach { 0 } else { wait }).await
        }
    }
}
pub async fn jobs(r: &Remote, command: JobsAction) -> Result<Value> {
    match command {
        JobsAction::Resource(action) => resource(r, "jobs", action).await,
        JobsAction::RepairWriteback {
            limit,
            detach,
            wait,
        } => {
            if !(1..=100).contains(&limit) {
                bail!("media repair limit must be 1..100");
            }
            let operation = r
                .post(&r.path("media-writeback/repair")?, json!({"limit":limit}))
                .await?;
            wait_operation(r, operation, if detach { 0 } else { wait }).await
        }
    }
}
pub async fn resource(r: &Remote, kind: &str, command: ResourceAction) -> Result<Value> {
    match command {
        ResourceAction::Get { id } => r.get(&r.path(&format!("{kind}/{}", encode(&id)))?).await,
        ResourceAction::Cancel { id } => {
            if kind == "operations" {
                r.request(
                    reqwest::Method::DELETE,
                    &r.path(&format!("operations/{}", encode(&id)))?,
                    None,
                    None,
                )
                .await
            } else {
                r.post(
                    &r.path(&format!("{kind}/{}/cancel", encode(&id)))?,
                    json!({}),
                )
                .await
            }
        }
        ResourceAction::Retry { id } => {
            if kind != "jobs" {
                bail!("unsupported_capability: an operation cannot be implicitly restarted");
            }
            r.post(&r.path(&format!("jobs/{}/retry", encode(&id)))?, json!({}))
                .await
        }
    }
}
pub async fn wait_resource(r: &Remote, mut value: Value, kind: &str, wait: u64) -> Result<Value> {
    if wait > 900 {
        bail!("wait must be at most900seconds");
    }
    if wait == 0 {
        return Ok(value);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(wait);
    while matches!(
        value["state"].as_str(),
        Some("queued" | "running" | "preparing" | "uploading" | "submitting")
    ) {
        if std::time::Instant::now() >= deadline {
            value["clientWaitExpired"] = json!(true);
            return Ok(value);
        }
        tokio::select! {_=tokio::signal::ctrl_c()=>{set_exit(130);value["clientInterrupted"]=json!(true);return Ok(value);},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
        let id = value["id"].as_str().context("resource identity missing")?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let request_path = r.path(&format!("{kind}/{}", encode(id)))?;
        let next = tokio::select! {_=tokio::signal::ctrl_c()=>{set_exit(130);value["clientInterrupted"]=json!(true);return Ok(value);},next=tokio::time::timeout(remaining,r.get(&request_path))=>next};
        match next {
            Ok(result) => value = result?,
            Err(_) => {
                value["clientWaitExpired"] = json!(true);
                return Ok(value);
            }
        }
    }
    Ok(value)
}
pub(super) fn write_replace(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("invalid receipt path")?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    wechat_platform::protect(output.as_file())?;
    output.write_all(&serde_json::to_vec(value)?)?;
    output.as_file().sync_all()?;
    output.persist(path)?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
fn receipt(
    config: &settings::File,
    r: &Remote,
    thread: &str,
    kind: &str,
    hash: &str,
    key: &str,
) -> Result<(PathBuf, Value)> {
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        bail!("invalid idempotency key");
    }
    let directory = config
        .path
        .parent()
        .context("client config location missing")?
        .join("receipts");
    std::fs::create_dir_all(&directory)?;
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || !wechat_platform::owned_path(&directory)? {
        bail!("receipt directory must be owned by this user");
    }
    wechat_platform::protect_path(&directory, true)?;
    let path = directory.join(format!(
        "{:x}.json",
        Sha256::digest(format!("{}\0{}\0{key}", r.base, r.account()?).as_bytes())
    ));
    let value = json!({"schemaVersion":1,"endpoint":r.base.as_str(),"accountId":r.account()?,"threadId":thread,"kind":kind,"sourceSha256":hash,"idempotencyKey":key,"state":"prepared"});
    if path.exists() {
        let existing: Value = serde_json::from_slice(&private_input(&path, 65536)?)?;
        for field in [
            "endpoint",
            "accountId",
            "threadId",
            "kind",
            "sourceSha256",
            "idempotencyKey",
        ] {
            if existing[field] != value[field] {
                bail!("idempotency_conflict: receipt input changed");
            }
        }
        return Ok((path, existing));
    }
    private_output(&path, &serde_json::to_vec(&value)?)?;
    Ok((path, value))
}
#[allow(clippy::too_many_arguments)] // Mirrors one command's mutually exclusive payload flags.
pub async fn send(
    r: &Remote,
    config: &settings::File,
    thread: String,
    text: Option<String>,
    image: Option<PathBuf>,
    file: Option<PathBuf>,
    video: Option<PathBuf>,
    voice: Option<PathBuf>,
    key: Option<String>,
    detach: bool,
    wait: u64,
) -> Result<Value> {
    let inputs = [
        image.map(|p| ("image", p)),
        file.map(|p| ("file", p)),
        video.map(|p| ("video", p)),
        voice.map(|p| ("voice", p)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if inputs.len() + usize::from(text.is_some()) != 1 {
        bail!("choose exactly one of --text/--image/--file/--video/--voice");
    }
    let (kind, bytes, name) = if let Some(text) = text {
        if text.is_empty() {
            bail!("message text cannot be empty");
        }
        ("text", text.into_bytes(), None)
    } else {
        let (kind, path) = &inputs[0];
        let mut file = wechat_platform::OpenOptions::new()
            .read(true)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > wechat_core::MAX_SOURCE_BYTES as u64
        {
            bail!("source must be a nonempty regular file within the native source limit");
        }
        let mut bytes = vec![];
        (&mut file)
            .take(wechat_core::MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > wechat_core::MAX_SOURCE_BYTES {
            bail!("source exceeds native source limit");
        }
        (
            *kind,
            bytes,
            Some(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .context("invalid source filename")?
                    .to_string(),
            ),
        )
    };
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let key = key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (path, mut receipt) = receipt(config, r, &thread, kind, &hash, &key)?;
    eprintln!("Submission receipt: {}", path.display());
    if receipt["resourceId"].is_string() {
        let value = r
            .get(&query(
                r.path("sends/by-idempotency-key")?,
                &[("key", Some(key))],
            ))
            .await?;
        let mut value = value;
        value["receiptPath"] = json!(path);
        return Ok(value);
    }
    let (mut result, resource) = if kind == "text" {
        (
            r.request(
                reqwest::Method::POST,
                &r.path("sends")?,
                Some(json!({"threadId":thread,"text":std::str::from_utf8(&bytes)?})),
                Some(&key),
            )
            .await
            .with_context(|| {
                format!(
                    "submission response unavailable; inspect original receipt {}",
                    path.display()
                )
            })?,
            "sends",
        )
    } else {
        let source_id = if let Some(id) = receipt["sourceId"].as_str() {
            id.to_string()
        } else {
            let source = r
                .upload(&bytes, name.as_deref().context("source name missing")?)
                .await?;
            let id = source["id"]
                .as_str()
                .context("upload identity missing")?
                .to_string();
            receipt["sourceId"] = json!(id);
            write_replace(&path, &receipt)?;
            id
        };
        (
            r.request(
                reqwest::Method::POST,
                &r.path("sends")?,
                Some(json!({"threadId":thread,"kind":kind,"sourceId":source_id})),
                Some(&key),
            )
            .await
            .with_context(|| {
                format!(
                    "submission response unavailable; inspect original receipt {}",
                    path.display()
                )
            })?,
            "jobs",
        )
    };
    receipt["resourceId"] = result["id"].clone();
    receipt["state"] = json!("accepted_by_daemon");
    write_replace(&path, &receipt)?;
    result = wait_resource(r, result, resource, if detach { 0 } else { wait }).await?;
    result["receiptPath"] = json!(path);
    Ok(result)
}
#[allow(clippy::too_many_arguments)] // One command's identity, quality, output and wait policy.
pub async fn media(
    r: &Remote,
    thread: &str,
    message: &str,
    quality: &str,
    output: Option<PathBuf>,
    cached_only: bool,
    detach: bool,
    wait: u64,
) -> Result<Value> {
    let record = r
        .get(&r.path(&format!("messages/{}", encode(message)))?)
        .await?;
    if record["threadId"] != thread {
        bail!("message does not belong to selected thread");
    }
    let quality = if quality == "full" {
        "original"
    } else {
        quality
    };
    let list = r
        .get(&r.path(&format!("messages/{}/media", encode(message)))?)
        .await?;
    let items = list["items"].as_array().context("invalid media catalog")?;
    let mut candidates = items
        .iter()
        .filter(|a| {
            a["state"] == "available" && (quality == "best" || a["actualVariant"] == quality)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|a| match a["actualVariant"].as_str() {
        Some("original") => 0,
        Some("standard" | "normal") => 1,
        Some("thumbnail") => 2,
        _ => 3,
    });
    let found = candidates.first().copied().cloned();
    let asset = if let Some(asset) = found {
        asset
    } else if cached_only {
        return Ok(json!({"state":"not_cached","quality":quality,"messageId":message}));
    } else {
        let job = r
            .post(
                &r.path(&format!("messages/{}/media-transfers", encode(message)))?,
                json!({"messageId":message,"quality":quality}),
            )
            .await?;
        let job = wait_resource(r, job, "jobs", if detach { 0 } else { wait }).await?;
        if !matches!(
            job["state"].as_str(),
            Some("succeeded" | "available" | "completed")
        ) {
            return Ok(job);
        }
        let id = job["assetId"]
            .as_str()
            .context("completed transfer asset missing")?;
        r.get(&r.path(&format!("media/{}", encode(id)))?).await?
    };
    if let Some(path) = output {
        if asset["state"] != "available" {
            return Ok(asset);
        }
        let id = asset["id"].as_str().context("media identity missing")?;
        let response = r
            .response(
                reqwest::Method::GET,
                &r.path(&format!("media/{}/content", encode(id)))?,
                None,
                None,
            )
            .await?;
        let bytes = super::bounded_bytes(response, 128 * 1024 * 1024).await?;
        if asset["sha256"].as_str() != Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
            || asset["byteLength"].as_u64() != Some(bytes.len() as u64)
        {
            bail!("media integrity check failed");
        }
        private_output(&path, &bytes)?;
        let mut asset = asset;
        asset["output"] = json!(path);
        return Ok(asset);
    }
    Ok(asset)
}
