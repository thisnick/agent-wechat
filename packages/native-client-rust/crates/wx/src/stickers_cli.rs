//! Saved collection reads and owned image/GIF preparation.
use super::*;
use anyhow::ensure;

#[derive(Subcommand)]
pub enum StickerCommand {
    /// Browse, inspect, export and change membership of official packs.
    Packs {
        /// Official-pack operation.
        #[command(subcommand)]
        command: StickerPackCommand,
    },
    /// Send an existing Saved or added-pack sticker using its stable identity.
    Send {
        /// Stable sticker content identity.
        id: String,
        /// Official pack identity; omit to select Saved.
        #[arg(long)]
        pack: Option<String>,
        /// Intended contact or chat identity.
        #[arg(long)]
        to: String,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// List Saved stickers in official picker order.
    List,
    /// Show one Saved sticker by stable content identity.
    Show {
        /// Sticker identity returned by stickers list.
        id: String,
    },
    /// Save a local source, a prepared result, or one received sticker.
    Add {
        /// Local source to prepare and review through its returned previews.
        #[arg(conflicts_with_all=["prepared","message","asset"])]
        file: Option<PathBuf>,
        /// Completed preparation to save, without preparing it again.
        #[arg(long, conflicts_with_all=["file","message","asset"])]
        prepared: Option<String>,
        /// Received message containing the original sticker attachment.
        #[arg(long, requires="asset", conflicts_with_all=["file","prepared"])]
        message: Option<String>,
        /// Verified original sticker asset belonging to --message.
        #[arg(long, requires = "message")]
        asset: Option<String>,
        /// Permit proportional resizing for a local source.
        #[arg(long, requires = "file")]
        fit: bool,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Move within Saved using a fresh list, excluding the moving item for --last.
    Move {
        /// Stable Saved sticker identity.
        id: String,
        /// Place immediately after this Saved identity.
        #[arg(long, conflicts_with_all=["first","last"])]
        after: Option<String>,
        /// Place at the beginning of Saved.
        #[arg(long, conflicts_with = "last")]
        first: bool,
        /// Place at the end of a freshly read Saved list.
        #[arg(long)]
        last: bool,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Remove Saved membership, preserving messages and official packs.
    Remove {
        /// Stable Saved sticker identity.
        id: String,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Reconcile Saved with the server after reconnecting or an event gap.
    Sync {
        /// Collections to refresh: saved, added or all.
        #[arg(long, default_value="all", value_parser=["saved","added","all"])]
        scope: String,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Download one Saved original to this machine, preserving its real format.
    Download {
        /// Stable Saved sticker identity.
        id: String,
        /// New local output with an extension matching the original format.
        #[arg(long)]
        output: PathBuf,
        /// Resume a matching partial original, or verify an already complete output.
        #[arg(long)]
        resume: bool,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Prepare a local image, GIF, or MOV/MP4 video without saving or sending it.
    Prepare {
        /// Local input file; the CLI uploads its bytes rather than a daemon path.
        file: PathBuf,
        /// Permit proportional resizing to the sticker edge limit.
        #[arg(long)]
        fit: bool,
        /// Start of an explicit segment in seconds; requires --duration.
        #[arg(long, requires = "duration")]
        start: Option<f64>,
        /// Retained segment length in seconds, at most ten.
        #[arg(long)]
        duration: Option<f64>,
        /// Stable key for this preparation; reuse it after an uncertain response.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Return immediately after the preparation is queued.
        #[arg(long, conflicts_with = "wait")]
        detach: bool,
        /// Seconds to wait; work continues after the client stops waiting.
        #[arg(long, default_value_t = 30, conflicts_with = "detach")]
        wait: u64,
    },
    /// Save a prepared visual PNG without saving a sticker to the account.
    Preview {
        /// Saved sticker to preview; choose this or --prepared.
        #[arg(conflicts_with = "prepared")]
        id: Option<String>,
        /// Preparation identity returned by stickers prepare.
        #[arg(long)]
        prepared: Option<String>,
        /// New local PNG file. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
        /// Export the sampled-frame contact sheet instead of the first frame.
        #[arg(long)]
        contact_sheet: bool,
    },
}

#[derive(Subcommand)]
pub enum StickerPackCommand {
    /// List every account-added official pack, including those outside the catalog.
    List,
    /// Read one page of the available public feed; it is not a global inventory.
    Browse {
        /// Opaque continuation from an earlier browse result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read one pack's name, membership and content readiness.
    Show {
        /// Official product identity.
        id: String,
    },
    /// List the complete supplied sticker identities, captions and media references.
    Items {
        /// Official product identity.
        id: String,
        /// Exact official caption locale, with default fallback.
        #[arg(long, default_value = "en")]
        language: String,
    },
    /// Export one selected pack as numbered GIFs plus index.json; does not add it.
    Download {
        /// Official product identity, never the whole catalog.
        id: String,
        /// Local export directory.
        #[arg(long)]
        output: PathBuf,
        /// Verify an existing index and checksums, retaining completed files.
        #[arg(long)]
        resume: bool,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Establish eligible-free membership without acquiring original media.
    Add {
        /// Official product identity.
        id: String,
        #[command(flatten)]
        job: StickerJobOptions,
    },
    /// Remove account membership without deleting the official product or chat history.
    Remove {
        /// Official product identity.
        id: String,
        #[command(flatten)]
        job: StickerJobOptions,
    },
}

#[derive(clap::Args)]
pub struct StickerJobOptions {
    /// Stable submission key; reuse the key after an uncertain response.
    #[arg(long)]
    idempotency_key: Option<String>,
    /// Return the queued job without waiting.
    #[arg(long, conflicts_with = "wait")]
    detach: bool,
    /// Seconds to wait; work continues after the client stops waiting.
    #[arg(long, default_value_t = 30, conflicts_with = "detach")]
    wait: u64,
}
impl StickerJobOptions {
    fn key(&self) -> String {
        self.idempotency_key
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }
    async fn finish(&self, remote: &Remote, value: Value, key: &str) -> Result<Value> {
        let mut value = client_actions::wait_resource(
            remote,
            value,
            "jobs",
            if self.detach { 0 } else { self.wait },
        )
        .await?;
        value["idempotencyKey"] = key.into();
        Ok(value)
    }
}
async fn run_packs(remote: &Remote, command: StickerPackCommand) -> Result<Value> {
    match command {
        StickerPackCommand::List => {
            remote
                .get(&remote.path("sticker-packs?membership=added")?)
                .await
        }
        StickerPackCommand::Browse { cursor } => {
            remote
                .get(&remote.path(&format!(
                        "sticker-packs{}",
                        cursor
                            .map(|c| format!("?cursor={}", encode(&c)))
                            .unwrap_or_default()
                    ))?)
                .await
        }
        StickerPackCommand::Show { id } => {
            remote
                .get(&remote.path(&format!("sticker-packs/{}", encode(&id)))?)
                .await
        }
        StickerPackCommand::Items { id, language } => {
            remote
                .get(&remote.path(&format!(
                    "sticker-packs/{}/stickers?language={}",
                    encode(&id),
                    encode(&language)
                ))?)
                .await
        }
        StickerPackCommand::Add { id, job } => {
            let key = job.key();
            let value = remote
                .request(
                    reqwest::Method::PUT,
                    &remote.path(&format!("sticker-packs/{}/membership", encode(&id)))?,
                    Some(json!({})),
                    Some(&key),
                )
                .await?;
            job.finish(remote, value, &key).await
        }
        StickerPackCommand::Remove { id, job } => {
            let key = job.key();
            let value = remote
                .request(
                    reqwest::Method::DELETE,
                    &remote.path(&format!("sticker-packs/{}/membership", encode(&id)))?,
                    None,
                    Some(&key),
                )
                .await?;
            job.finish(remote, value, &key).await
        }
        StickerPackCommand::Download {
            id,
            output,
            resume,
            job,
        } => {
            let detail = remote
                .get(&remote.path(&format!("sticker-packs/{}", encode(&id)))?)
                .await?;
            let path = remote.path(&format!("sticker-packs/{}/stickers", encode(&id)))?;
            let mut contents = remote.get(&path).await?;
            let ready = contents["items"].as_array().is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item["original"]["state"] == "available")
            });
            if !ready {
                let key = job.key();
                let value = remote
                    .request(
                        reqwest::Method::POST,
                        &remote.path("sticker-downloads")?,
                        Some(json!({"packId":id})),
                        Some(&key),
                    )
                    .await?;
                let result = job.finish(remote, value, &key).await?;
                if result["state"] != "available" {
                    return Ok(result);
                }
                contents = remote.get(&path).await?;
            }
            let mut items = vec![];
            for item in contents["items"]
                .as_array()
                .context("pack contents missing")?
            {
                let bytes = match item["original"]["id"].as_str() {
                    Some(asset) => read_asset_bytes(remote, asset, &item["original"])
                        .await
                        .map_err(|_| "sticker_download_failed".into()),
                    None => Err("sticker_original_unavailable".into()),
                };
                items.push(wechat_media::StickerExportItem {
                    id: item["id"].as_str().context("item identity missing")?.into(),
                    ordinal: u32::try_from(
                        item["ordinal"].as_u64().context("item ordinal missing")?,
                    )?,
                    caption: item["caption"].as_str().map(str::to_string),
                    bytes,
                });
            }
            let pack = id.clone();
            let name = detail["name"].as_str().unwrap_or(&id).to_string();
            let index = tokio::task::spawn_blocking(move || {
                wechat_media::export_sticker_pack(&output, &pack, &name, items, resume)
            })
            .await??;
            Ok(serde_json::to_value(index)?)
        }
    }
}
async fn read_asset_bytes(remote: &Remote, id: &str, asset: &Value) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let response = remote
        .response(
            reqwest::Method::GET,
            &remote.path(&format!("media/{}/content", encode(id)))?,
            None,
            None,
        )
        .await?;
    let bytes = bounded_bytes(response, wechat_media::MAX_IMAGE_BYTES).await?;
    ensure!(
        asset["byteLength"].as_u64() == Some(bytes.len() as u64)
            && asset["sha256"].as_str() == Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
        "media_integrity_failed"
    );
    Ok(bytes.to_vec())
}
async fn resumable_original(remote: &Remote, id: &str, output: &Path, asset: &Value) -> Result<()> {
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    let total = asset["byteLength"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 16 * 1024 * 1024)
        .context("invalid original length")?;
    let sha = asset["sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .context("invalid original digest")?;
    if output.exists() {
        let mut file = wechat_platform::OpenOptions::new()
            .read(true)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(output)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() == total,
            "existing output does not match this original"
        );
        let mut hash = Sha256::new();
        std::io::copy(&mut file, &mut hash)?;
        ensure!(
            format!("{:x}", hash.finalize()) == sha,
            "existing output checksum mismatch"
        );
        return Ok(());
    }
    let mut partial_name = output.as_os_str().to_os_string();
    partial_name.push(".wx-part");
    let partial = PathBuf::from(partial_name);
    let mut metadata_name = partial.as_os_str().to_os_string();
    metadata_name.push(".json");
    let metadata_path = PathBuf::from(metadata_name);
    let expected = json!({"assetId":id,"byteLength":total,"sha256":sha});
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if metadata_path.exists() {
        let mut file = wechat_platform::OpenOptions::new()
            .read(true)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(&metadata_path)?;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.len() <= 4096,
            "invalid partial metadata"
        );
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        ensure!(
            serde_json::from_slice::<Value>(&bytes)? == expected,
            "partial original identity mismatch"
        );
    } else {
        ensure!(
            !partial.exists(),
            "partial file has no matching identity record"
        );
        let mut temp = wechat_platform::private_tempfile_in(parent, ".wx-sticker-resume-")?;
        serde_json::to_writer(&mut temp, &expected)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&metadata_path)
            .map_err(|e| e.error)?;
    }
    let mut options = wechat_platform::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW);
    if !partial.exists() {
        options.create_new(true);
    }
    let mut file = options.open(&partial)?;
    fs2::FileExt::try_lock_exclusive(&file)?;
    let info = file.metadata()?;
    ensure!(
        info.is_file() && info.len() <= total && wechat_platform::private_file(&file)?,
        "invalid partial original"
    );
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    let mut position = info.len();
    if position < total {
        let response = remote
            .http
            .get(
                remote
                    .base
                    .join(&remote.path(&format!("media/{}/content", encode(id)))?)?,
            )
            .bearer_auth(&remote.token)
            .header("Range", format!("bytes={position}-"))
            .header("If-Range", format!("\"{sha}\""))
            .timeout(Duration::from_secs(65))
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::PARTIAL_CONTENT
                && response
                    .headers()
                    .get("content-range")
                    .and_then(|h| h.to_str().ok())
                    == Some(format!("bytes {position}-{}/{total}", total - 1).as_str())
                && response.headers().get("etag").and_then(|h| h.to_str().ok())
                    == Some(format!("\"{sha}\"").as_str())
                && response
                    .content_length()
                    .is_none_or(|n| n == total - position),
            "original range or identity changed; partial retained"
        );
        file.seek(SeekFrom::End(0))?;
        let mut stream = response.bytes_stream();
        while let Some(bytes) = stream.next().await {
            let bytes = bytes?;
            ensure!(
                bytes.len() as u64 <= total - position,
                "original range exceeds its bound"
            );
            file.write_all(&bytes)?;
            file.sync_data()?;
            hash.update(&bytes);
            position += bytes.len() as u64;
        }
    }
    ensure!(
        position == total && format!("{:x}", hash.finalize()) == sha,
        "original integrity check failed; partial retained"
    );
    // Publish from the verified open handle. The existing partial remains intact
    // if a new output cannot be installed, including an unrelated-file conflict.
    file.seek(SeekFrom::Start(0))?;
    let mut temp = wechat_platform::private_tempfile_in(parent, ".wx-sticker-output-")?;
    ensure!(
        std::io::copy(&mut file, &mut temp)? == total,
        "partial original changed"
    );
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    if std::fs::symlink_metadata(&partial)
        .is_ok_and(|meta| wechat_platform::same_file(&info, &meta))
    {
        std::fs::remove_file(&partial)?;
    }
    if std::fs::read(&metadata_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .as_ref()
        == Some(&expected)
    {
        std::fs::remove_file(&metadata_path)?;
    }
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
pub async fn run(remote: &Remote, command: StickerCommand) -> Result<Value> {
    match command {
        StickerCommand::Packs { command } => run_packs(remote, command).await,
        StickerCommand::Send { id, pack, to, job } => {
            let source = if let Some(pack) = pack {
                json!({"kind":"pack","packId":pack,"stickerId":id})
            } else {
                json!({"kind":"saved","stickerId":id})
            };
            let key = job.key();
            let value = remote
                .request(
                    reqwest::Method::POST,
                    &remote.path("sends")?,
                    Some(json!({"kind":"sticker","threadId":to,"source":source})),
                    Some(&key),
                )
                .await?;
            job.finish(remote, value, &key).await
        }
        StickerCommand::List => remote.get(&remote.path("custom-stickers")?).await,
        StickerCommand::Show { id } => {
            remote
                .get(&remote.path(&format!("custom-stickers/{}", encode(&id)))?)
                .await
        }
        StickerCommand::Add {
            file,
            prepared,
            message,
            asset,
            fit,
            job,
        } => {
            let key = job.key();
            let source = if let Some(file) = file {
                use sha2::{Digest, Sha256};
                let preparation = Box::pin(run(
                    remote,
                    StickerCommand::Prepare {
                        file,
                        fit,
                        start: None,
                        duration: None,
                        idempotency_key: Some(format!(
                            "sticker-prepare:{:x}",
                            Sha256::digest(key.as_bytes())
                        )),
                        detach: false,
                        wait: job.wait,
                    },
                ))
                .await?;
                if preparation["state"] != "available" {
                    return Ok(preparation);
                }
                json!({"kind":"prepared","preparationId":preparation["id"]})
            } else if let Some(prepared) = prepared {
                json!({"kind":"prepared","preparationId":prepared})
            } else if let (Some(message), Some(asset)) = (message, asset) {
                json!({"kind":"message","messageId":message,"assetId":asset})
            } else {
                bail!("provide a file, --prepared, or --message with --asset");
            };
            let result = remote
                .request(
                    reqwest::Method::POST,
                    &remote.path("custom-stickers")?,
                    Some(json!({"source":source})),
                    Some(&key),
                )
                .await?;
            job.finish(remote, result, &key).await
        }
        StickerCommand::Move {
            id,
            after,
            first,
            last,
            job,
        } => {
            if usize::from(after.is_some()) + usize::from(first) + usize::from(last) != 1 {
                bail!("choose --after, --first or --last");
            }
            let saved = remote.get(&remote.path("custom-stickers")?).await?;
            let after = if last {
                saved["items"]
                    .as_array()
                    .context("invalid Saved list")?
                    .iter()
                    .rev()
                    .find_map(|item| {
                        item["id"]
                            .as_str()
                            .filter(|item| *item != id)
                            .map(str::to_string)
                    })
            } else {
                after
            };
            let key = job.key();
            let response = remote
                .response_with_revision(
                    reqwest::Method::PATCH,
                    &remote.path(&format!("custom-stickers/{}", encode(&id)))?,
                    Some(json!({"position":{"afterId":after}})),
                    Some(&key),
                    Duration::from_secs(65),
                    Some(
                        saved["revision"]
                            .as_str()
                            .context("Saved revision missing")?,
                    ),
                )
                .await?;
            let value = serde_json::from_slice(&bounded_bytes(response, 32 * 1024 * 1024).await?)?;
            job.finish(remote, value, &key).await
        }
        StickerCommand::Remove { id, job } => {
            let saved = remote.get(&remote.path("custom-stickers")?).await?;
            let key = job.key();
            let response = remote
                .response_with_revision(
                    reqwest::Method::DELETE,
                    &remote.path(&format!("custom-stickers/{}", encode(&id)))?,
                    None,
                    Some(&key),
                    Duration::from_secs(65),
                    Some(
                        saved["revision"]
                            .as_str()
                            .context("Saved revision missing")?,
                    ),
                )
                .await?;
            let value = serde_json::from_slice(&bounded_bytes(response, 32 * 1024 * 1024).await?)?;
            job.finish(remote, value, &key).await
        }
        StickerCommand::Sync { job, scope } => {
            let key = job.key();
            let value = remote
                .request(
                    reqwest::Method::POST,
                    &remote.path("sticker-syncs")?,
                    Some(json!({"scope":scope})),
                    Some(&key),
                )
                .await?;
            job.finish(remote, value, &key).await
        }
        StickerCommand::Download {
            id,
            output,
            resume,
            job,
        } => {
            let key = job.key();
            let value = remote
                .request(
                    reqwest::Method::POST,
                    &remote.path("sticker-downloads")?,
                    Some(json!({"stickerId":id})),
                    Some(&key),
                )
                .await?;
            let value = job.finish(remote, value, &key).await?;
            if value["state"] != "available" {
                return Ok(value);
            }
            let item = remote
                .get(&remote.path(&format!("custom-stickers/{}", encode(&id)))?)
                .await?;
            let asset = &item["original"];
            let asset_id = asset["id"].as_str().context("Saved original unavailable")?;
            let extension = output
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mime = asset["contentType"]
                .as_str()
                .context("original MIME missing")?;
            if !matches!(
                (mime, extension.as_str()),
                ("image/gif", "gif")
                    | ("image/png", "png")
                    | ("image/jpeg", "jpg" | "jpeg")
                    | ("image/webp", "webp")
                    | ("image/x-wxgf", "wxgf")
            ) {
                bail!("output extension must match original format {mime}");
            }
            if resume {
                resumable_original(remote, asset_id, &output, asset).await?;
            } else {
                let response = remote
                    .response(
                        reqwest::Method::GET,
                        &remote.path(&format!("media/{}/content", encode(asset_id)))?,
                        None,
                        None,
                    )
                    .await?;
                private_media_output(
                    &output,
                    response,
                    16 * 1024 * 1024,
                    asset["byteLength"].as_u64(),
                    asset["sha256"].as_str(),
                )
                .await?;
            }
            Ok(
                json!({"state":"available","output":output,"contentType":mime,"byteLength":asset["byteLength"],"sha256":asset["sha256"]}),
            )
        }
        StickerCommand::Prepare {
            file,
            fit,
            start,
            duration,
            idempotency_key,
            detach,
            wait,
        } => {
            use std::io::Read;
            let source = std::fs::File::open(&file)?;
            let size = source.metadata()?;
            if !size.is_file()
                || size.len() == 0
                || size.len() > wechat_core::MAX_STICKER_SOURCE_BYTES as u64
            {
                bail!("sticker_source_size_limit");
            }
            let mut bytes = Vec::new();
            source
                .take(wechat_core::MAX_STICKER_SOURCE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > wechat_core::MAX_STICKER_SOURCE_BYTES {
                bail!("sticker_source_size_limit");
            }
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .context("invalid input filename")?;
            let trim = if let Some(duration) = duration {
                let start = start.unwrap_or(0.0);
                if !start.is_finite()
                    || start < 0.0
                    || start * 1000.0 > u32::MAX as f64
                    || !duration.is_finite()
                    || duration <= 0.0
                    || duration > 10.0
                {
                    bail!("invalid sticker segment");
                }
                Some(
                    json!({"startMs":(start*1000.0).round() as u32,"durationMs":(duration*1000.0).round() as u32}),
                )
            } else {
                None
            };
            let source = remote.upload(bytes, name).await?;
            let key = idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let result = remote.request(reqwest::Method::POST, &remote.path("sticker-preparations")?,
                Some(json!({"sourceId":source["id"],"fit":fit,"trim":trim})), Some(&key)).await
                .with_context(|| format!("Preparation response unavailable; retain idempotency key {key} and reuse it with the same input"))?;
            let mut result = client_actions::wait_resource(
                remote,
                result,
                "jobs",
                if detach { 0 } else { wait },
            )
            .await?;
            result["idempotencyKey"] = key.into();
            Ok(result)
        }
        StickerCommand::Preview {
            id,
            prepared,
            output,
            contact_sheet,
        } => {
            if let Some(id) = id {
                if !matches!(
                    output.extension().and_then(|s| s.to_str()),
                    Some("png" | "jpg" | "jpeg")
                ) {
                    bail!("preview output requires .png or .jpg");
                }
                let item = remote
                    .get(&remote.path(&format!("custom-stickers/{}", encode(&id)))?)
                    .await?;
                let asset = &item["original"];
                let preview = &asset["preview"];
                let asset_id = asset["id"]
                    .as_str()
                    .context("Saved original unavailable; download it first")?;
                let sha = preview["sha256"]
                    .as_str()
                    .context("Saved visual preview unavailable; download it first")?;
                let mime = preview["contentType"]
                    .as_str()
                    .context("Saved preview MIME missing")?;
                if (mime == "image/png") != output.extension().is_some_and(|s| s == "png") {
                    bail!("preview output extension must match {mime}");
                }
                let response = remote
                    .response(
                        reqwest::Method::GET,
                        &remote.path(&format!(
                            "media/{}/content?representation=preview",
                            encode(asset_id)
                        ))?,
                        None,
                        None,
                    )
                    .await?;
                private_media_output(
                    &output,
                    response,
                    wechat_core::MAX_STICKER_SOURCE_BYTES,
                    preview["byteLength"].as_u64(),
                    Some(sha),
                )
                .await?;
                return Ok(
                    json!({"state":"available","output":output,"contentType":mime,"sha256":sha,"source":"sampled_frames_or_still"}),
                );
            }
            let prepared = prepared.context("choose a Saved identity or --prepared")?;
            let job = remote
                .get(&remote.path(&format!("sticker-preparations/{}", encode(&prepared)))?)
                .await?;
            if job["state"] != "available" {
                return Ok(job);
            }
            let asset = &job["result"][if contact_sheet {
                "contactSheet"
            } else {
                "preview"
            }];
            let id = asset["id"]
                .as_str()
                .context("preparation preview missing")?;
            let response = remote
                .response(
                    reqwest::Method::GET,
                    &remote.path(&format!("media/{}/content", encode(id)))?,
                    None,
                    None,
                )
                .await?;
            private_media_output(
                &output,
                response,
                wechat_core::MAX_STICKER_SOURCE_BYTES,
                asset["byteLength"].as_u64(),
                asset["sha256"].as_str(),
            )
            .await?;
            Ok(
                json!({"state":"available","output":output,"contentType":asset["contentType"],"byteLength":asset["byteLength"],"sha256":asset["sha256"]}),
            )
        }
    }
}
