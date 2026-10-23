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
        ResourceAction::Retry {
            id,
            reuse_completed_upload,
        } => {
            if kind != "jobs" {
                bail!("unsupported_capability: an operation cannot be implicitly restarted");
            }
            let body = if reuse_completed_upload {
                json!({"reuseCompletedUpload":true})
            } else {
                json!({})
            };
            let action = if reuse_completed_upload {
                "retry-completed-upload"
            } else {
                "retry"
            };
            r.post(&r.path(&format!("jobs/{}/{action}", encode(&id)))?, body)
                .await
        }
    }
}
pub async fn wait_resource(r: &Remote, mut value: Value, kind: &str, wait: u64) -> Result<Value> {
    let max_wait = if kind == "jobs" { 3600 } else { 900 };
    if wait > max_wait {
        bail!("wait must be at most{max_wait}seconds");
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
    let mut output = wechat_platform::private_tempfile_in(parent, ".wx-receipt-")?;
    output.write_all(&serde_json::to_vec(value)?)?;
    output.as_file().sync_all()?;
    output.persist(path)?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
fn receipt(
    config: &settings::File,
    r: &Remote,
    target: &str,
    kind: &str,
    hash: &str,
    key: &str,
    server_id: Option<&str>,
) -> Result<(PathBuf, Value)> {
    receipt_with_video_quality(config, r, target, kind, hash, key, server_id, None)
}
#[allow(clippy::too_many_arguments)]
fn receipt_with_video_quality(
    config: &settings::File,
    r: &Remote,
    target: &str,
    kind: &str,
    hash: &str,
    key: &str,
    server_id: Option<&str>,
    video_quality: Option<&str>,
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
    let target_field = if kind == "message_recall" {
        "messageId"
    } else {
        "threadId"
    };
    let mut value = json!({"schemaVersion":1,"endpoint":r.base.as_str(),"accountId":r.account()?,"kind":kind,"sourceSha256":hash,"idempotencyKey":key,"state":"prepared"});
    value[target_field] = json!(target);
    if let Some(quality) = video_quality {
        value["videoQuality"] = json!(quality);
    }
    if let Some(server_id) = server_id {
        value["serverId"] = json!(server_id);
    }
    if path.exists() {
        let existing: Value = serde_json::from_slice(&private_input(&path, 65536)?)?;
        for field in [
            "endpoint",
            "accountId",
            target_field,
            "kind",
            "sourceSha256",
            "idempotencyKey",
            "serverId",
            "videoQuality",
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
pub async fn recall(
    r: &Remote,
    config: &settings::File,
    message: String,
    server: String,
    key: Option<String>,
    detach: bool,
    wait: u64,
) -> Result<Value> {
    if message.is_empty()
        || message.len() > 8192
        || server
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .is_none_or(|value| value.to_string() != server)
    {
        bail!("invalid recall message or exact server ID");
    }
    if wait > 900 {
        bail!("wait must be at most900seconds");
    }
    let hash = format!("{:x}", Sha256::digest(server.as_bytes()));
    let key = key.unwrap_or_else(|| {
        format!(
            "recall-{:x}",
            Sha256::digest(format!("{message}\0{server}").as_bytes())
        )
    });
    let (path, mut saved) = receipt(
        config,
        r,
        &message,
        "message_recall",
        &hash,
        &key,
        Some(&server),
    )?;
    eprintln!("Recall receipt: {}", path.display());
    let op = if let Some(id) = saved["operationId"].as_str() {
        r.get(&r.path(&format!("operations/{}", encode(id)))?)
            .await?
    } else {
        saved["state"] = json!("submitting");
        write_replace(&path, &saved)?;
        let op = r.request(reqwest::Method::POST,
            &r.path(&format!("messages/{}/recall", encode(&message)))?,
            Some(json!({"serverId":server})), Some(&key)).await
            .with_context(|| format!("recall response unavailable; keep the same target and key from receipt {} and inspect the daemon before another action", path.display()))?;
        validate_recall_operation(r, &op, &message, &server, None)?;
        saved["operationId"] = op["id"].clone();
        saved["state"] = json!("accepted_by_daemon");
        write_replace(&path, &saved)?;
        op
    };
    let operation_id = saved["operationId"]
        .as_str()
        .context("invalid recall operation receipt")?;
    let mut result = wait_operation_checked(r, op, if detach { 0 } else { wait }, |value| {
        validate_recall_operation(r, value, &message, &server, Some(operation_id))
    })
    .await?;
    result["receiptPath"] = json!(path);
    Ok(result)
}
fn validate_recall_operation(
    r: &Remote,
    operation: &Value,
    message: &str,
    server: &str,
    expected_id: Option<&str>,
) -> Result<()> {
    if operation["accountId"].as_str() != Some(r.account()?)
        || operation["kind"] != "message_recall"
        || operation["id"].as_str().is_none_or(str::is_empty)
        || expected_id.is_some_and(|id| operation["id"].as_str() != Some(id))
        || operation["result"]["messageId"].as_str() != Some(message)
        || operation["result"]["serverId"].as_str() != Some(server)
    {
        return Err(failure(8, "recall_result_unverified", "Recall operation identity could not be verified; inspect the retained receipt before another action"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Mirrors one command's mutually exclusive payload flags.
pub async fn send(
    r: &Remote,
    config: &settings::File,
    thread: String,
    share: Option<String>,
    title: Option<String>,
    description: Option<String>,
    thumbnail_url: Option<String>,
    text: Option<String>,
    image: Option<PathBuf>,
    file: Option<PathBuf>,
    video: Option<PathBuf>,
    video_quality: Option<String>,
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
    if inputs.len() + usize::from(text.is_some()) + usize::from(share.is_some()) != 1 {
        bail!("choose exactly one of --text/--share/--image/--file/--video/--voice");
    }
    let (kind, bytes, name) = if let Some(url) = share {
        let request = wechat_core::ShareSendRequest::Share {
            thread_id: thread.clone(),
            title: title.context("--share requires --title")?,
            url,
            description,
            thumbnail_url,
        };
        request.validate()?;
        ("share", serde_json::to_vec(&request)?, None)
    } else if let Some(text) = text {
        if text.is_empty() {
            bail!("message text cannot be empty");
        }
        ("text", text.into_bytes(), None)
    } else {
        let (kind, path) = &inputs[0];
        let max_source_bytes = send_kind_source_limit(kind)?;
        let mut file = wechat_platform::OpenOptions::new()
            .read(true)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_source_bytes as u64 {
            bail!("source must be a nonempty regular file within the native source limit");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&mut file)
            .take(max_source_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > max_source_bytes {
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
    let video_quality = (kind == "video").then(|| video_quality.unwrap_or_else(|| "normal".into()));
    let (path, mut receipt) = receipt_with_video_quality(
        config,
        r,
        &thread,
        kind,
        &hash,
        &key,
        None,
        video_quality.as_deref(),
    )?;
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
    let (mut result, resource) = if kind == "text" || kind == "share" {
        (
            r.request(
                reqwest::Method::POST,
                &r.path("sends")?,
                Some(if kind == "share" {
                    serde_json::from_slice(&bytes)?
                } else {
                    json!({"threadId":thread,"text":std::str::from_utf8(&bytes)?})
                }),
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
                .upload(bytes, name.as_deref().context("source name missing")?)
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
                Some({
                    let mut request = json!({"threadId":thread,"kind":kind,"sourceId":source_id});
                    if let Some(quality) = video_quality {
                        request["videoQuality"] = quality.into();
                    }
                    request
                }),
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
fn send_kind_source_limit(kind: &str) -> Result<usize> {
    use wechat_core::MediaKind;
    let kind = match kind {
        "file" => MediaKind::File,
        "image" => MediaKind::Image,
        "video" => MediaKind::Video,
        "voice" => MediaKind::Voice,
        _ => bail!("unsupported media send kind"),
    };
    Ok(kind.max_source_bytes())
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
            .response_with_timeout(
                reqwest::Method::GET,
                &r.path(&format!("media/{}/content", encode(id)))?,
                None,
                None,
                Duration::from_secs(3600),
            )
            .await?;
        let max_bytes = if asset["kind"] == "file" {
            wechat_core::MAX_SOURCE_BYTES
        } else {
            128 * 1024 * 1024
        };
        super::private_media_output(
            &path,
            response,
            max_bytes,
            asset["byteLength"].as_u64(),
            asset["sha256"].as_str(),
        )
        .await?;
        let mut asset = asset;
        asset["output"] = json!(path);
        return Ok(asset);
    }
    Ok(asset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streamed_media_output_requires_complete_digest_and_preserves_existing_files() {
        use axum::{body::Body, routing::get, Router};
        let payload = b"synthetic streamed attachment";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/",
                    get(|| async {
                        Body::from_stream(futures_util::stream::iter([
                            Ok::<_, std::io::Error>(b"synthetic streamed ".as_slice()),
                            Ok(b"attachment".as_slice()),
                        ]))
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("saved.bin");
        let hash = format!("{:x}", Sha256::digest(payload));
        for (size, digest) in [
            (payload.len() as u64 + 1, hash.as_str()),
            (
                payload.len() as u64,
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        ] {
            let response = client
                .get(format!("http://{address}/"))
                .send()
                .await
                .unwrap();
            assert!(
                crate::private_media_output(&output, response, 1024, Some(size), Some(digest))
                    .await
                    .is_err()
            );
            assert!(!output.exists());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
        for succeeds in [true, false] {
            let response = client
                .get(format!("http://{address}/"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                crate::private_media_output(
                    &output,
                    response,
                    1024,
                    Some(payload.len() as u64),
                    Some(&hash)
                )
                .await
                .is_ok(),
                succeeds
            );
            assert_eq!(std::fs::read(&output).unwrap(), payload);
        }
        server.abort();
    }

    #[test]
    fn video_receipt_binds_quality_before_any_network_request() {
        let temp = tempfile::tempdir().unwrap();
        let remote = Remote {
            http: reqwest::Client::new(),
            base: reqwest::Url::parse("http://127.0.0.1:1/").unwrap(),
            token: "synthetic-token".into(),
            account: Some("synthetic".into()),
        };
        let config = settings::File {
            path: temp.path().join("client.toml"),
            settings: Default::default(),
        };
        let make = |quality| {
            receipt_with_video_quality(
                &config,
                &remote,
                "filehelper",
                "video",
                "source-sha",
                "quality-key",
                None,
                quality,
            )
        };
        let (_, original) = make(Some("normal")).unwrap();
        assert_eq!(original["sourceSha256"], "source-sha");
        assert!(make(Some("normal")).is_ok());
        assert!(make(Some("original"))
            .unwrap_err()
            .to_string()
            .contains("idempotency_conflict"));
        assert!(make(None).is_err());
    }

    #[test]
    fn selected_payload_flag_controls_source_read_bound() {
        assert_eq!(send_kind_source_limit("file").unwrap(), 536_870_912);
        assert_eq!(send_kind_source_limit("video").unwrap(), 52_428_800);
        for kind in ["image", "voice"] {
            assert_eq!(send_kind_source_limit(kind).unwrap(), 26_214_383);
        }
        for unsupported in ["sticker", "text", "share", "unknown"] {
            assert!(send_kind_source_limit(unsupported).is_err());
        }
    }

    #[tokio::test]
    async fn oversized_ordinary_kinds_are_rejected_before_daemon_submission() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("synthetic.bin");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(wechat_core::MAX_ORDINARY_SOURCE_BYTES as u64 + 1)
            .unwrap();
        let remote = Remote {
            http: reqwest::Client::new(),
            base: reqwest::Url::parse("http://127.0.0.1:1/").unwrap(),
            token: "synthetic-test-token".into(),
            account: Some("synthetic".into()),
        };
        let config = settings::File {
            path: temp.path().join("client.toml"),
            settings: Default::default(),
        };
        for kind in ["image", "video", "voice"] {
            std::fs::File::create(&path)
                .unwrap()
                .set_len(send_kind_source_limit(kind).unwrap() as u64 + 1)
                .unwrap();
            let error = send(
                &remote,
                &config,
                "filehelper".into(),
                None,
                None,
                None,
                None,
                None,
                (kind == "image").then(|| path.clone()),
                None,
                (kind == "video").then(|| path.clone()),
                None,
                (kind == "voice").then(|| path.clone()),
                Some("synthetic-key".into()),
                false,
                0,
            )
            .await
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("source must be a nonempty regular file"));
        }
    }
}
