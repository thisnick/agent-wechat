use super::*;
use crate::upstream::{media::MediaTransport, Upstream, UpstreamKind};
use std::sync::{atomic::AtomicUsize, Condvar, Mutex};
use wechat_protocol::media::{DownloadedMedia, PreparedUpload};

pub(super) struct PausedDownloads {
    starts: std::sync::mpsc::Sender<String>,
    release: Arc<(Mutex<bool>, Condvar)>,
    count: Arc<AtomicUsize>,
    video_reply: Mutex<Option<Vec<u8>>>,
    channels_count: AtomicUsize,
    channels_replies: Mutex<Vec<std::result::Result<Vec<u8>, String>>>,
    /// Open unless a test holds Channels fetches to observe an in-flight job.
    channels_open: (Mutex<bool>, Condvar),
}
impl PausedDownloads {
    /// Keep the next Channels fetch running until `release_channels`, so a
    /// repeated request is checked while the first job is still in flight.
    fn hold_channels(&self) {
        *self.channels_open.0.lock().unwrap() = false;
    }
    fn release_channels(&self) {
        *self.channels_open.0.lock().unwrap() = true;
        self.channels_open.1.notify_all();
    }
}
struct PausedCdn(Arc<PausedDownloads>);
impl Upstream for Arc<PausedDownloads> {
    fn kind(&self) -> UpstreamKind {
        UpstreamKind::Native
    }
    fn media_transport_route(
        &self,
        _: &mut wechat_protocol::AccountState,
        _: Option<LargeCdnRoute>,
    ) -> Result<Box<dyn MediaTransport>> {
        Ok(Box::new(PausedCdn(self.clone())))
    }
    fn fetch_channels_video(
        &self,
        _: &mut wechat_protocol::AccountState,
        post: &wechat_protocol::ChannelsPost,
        max_bytes: usize,
        checkpoint: &mut dyn FnMut() -> Result<()>,
        progress: crate::upstream::media::Progress<'_>,
    ) -> Result<wechat_protocol::media::ChannelsVideo> {
        self.channels_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(post.object_id, 18_446_744_073_709_551_000);
        assert_eq!(post.nonce, "12345678901234567890_0_38_12_4_context");
        assert_eq!(max_bytes, wechat_protocol::media::MAX_CHANNELS_VIDEO_BYTES);
        checkpoint()?;
        // Bounded so a failed assertion while held cannot hang the test run.
        let (open, released) = &self.channels_open;
        drop(
            released
                .wait_timeout_while(open.lock().unwrap(), Duration::from_secs(10), |open| !*open)
                .unwrap(),
        );
        let reply = self
            .channels_replies
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("no synthetic Channels reply".into()));
        let bytes = reply.map_err(|code| anyhow::anyhow!(code))?;
        progress(bytes.len() as u64, Some(bytes.len() as u64))?;
        Ok(wechat_protocol::media::ChannelsVideo {
            facts: wechat_protocol::media::ChannelsDownloadFacts {
                received_bytes: bytes.len() as u64,
                decoded_box: wechat_protocol::media::channels_box_type(&bytes),
                ..Default::default()
            },
            bytes,
        })
    }
}
impl MediaTransport for PausedCdn {
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: crate::upstream::media::Progress<'_>,
    ) -> Result<DownloadedMedia> {
        self.0.count.fetch_add(1, Ordering::SeqCst);
        self.0.starts.send(descriptor.file_id.clone())?;
        let (lock, signal) = &*self.0.release;
        let (released, _) = signal
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(15), |released| {
                !*released
            })
            .unwrap();
        ensure!(*released, "synthetic transfer timed out");
        if let Some(bytes) = self.0.video_reply.lock().unwrap().clone() {
            assert_eq!(
                descriptor.strict_size,
                !descriptor.file_id.ends_with("normal"),
                "only a normal-video request may select the CDN representation"
            );
            progress(bytes.len() as u64, Some(bytes.len() as u64))?;
            return Ok(DownloadedMedia {
                md5: hex_string(&wechat_protocol::crypto::md5(&bytes)),
                sha256: digest(&bytes),
                bytes,
                requested_quality: descriptor.quality,
                substituted_filetype: Some(9),
                picture_format: None,
                advertised_size_matches: Some(false),
            });
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2).write_to(&mut bytes, image::ImageFormat::Png)?;
        let bytes = bytes.into_inner();
        progress(bytes.len() as u64, Some(bytes.len() as u64))?;
        Ok(DownloadedMedia {
            md5: hex_string(&wechat_protocol::crypto::md5(&bytes)),
            sha256: digest(&bytes),
            bytes,
            requested_quality: descriptor.quality,
            substituted_filetype: None,
            picture_format: None,
            advertised_size_matches: Some(true),
        })
    }
    fn upload(
        &self,
        _: &PreparedUpload,
        _: crate::upstream::media::Progress<'_>,
    ) -> Result<UploadedMedia> {
        bail!("unexpected upload")
    }
    fn upload_image(
        &self,
        _: &PreparedImageUpload,
        _: crate::upstream::media::Progress<'_>,
    ) -> Result<UploadedImage> {
        bail!("unexpected upload")
    }
    fn upload_video(
        &self,
        _: &PreparedVideoUpload,
        _: crate::upstream::media::Progress<'_>,
    ) -> Result<UploadedVideo> {
        bail!("unexpected upload")
    }
}
pub(super) fn fixture() -> (
    tempfile::TempDir,
    Arc<Core>,
    Arc<PausedDownloads>,
    std::sync::mpsc::Receiver<String>,
) {
    let (tmp, mut core) = crate::read_view::tests::fixture();
    let (starts, received) = std::sync::mpsc::channel();
    let upstream = Arc::new(PausedDownloads {
        starts,
        release: Arc::new((Mutex::new(false), Condvar::new())),
        count: Arc::new(AtomicUsize::new(0)),
        video_reply: Mutex::new(None),
        channels_count: AtomicUsize::new(0),
        channels_replies: Mutex::new(Vec::new()),
        channels_open: (Mutex::new(true), Condvar::new()),
    });
    Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
        .unwrap()
        .upstream = Arc::new(upstream.clone());
    let mut state = super::tests::accepted_media_state();
    state.own_username = "account".into();
    let format = [9; 16];
    let pair = wechat_profile::create_mmkv_v4(&format, [3; 16], 4096).unwrap();
    let pair = wechat_profile::stage_mmkv_values(
        pair.data(),
        pair.metadata(),
        &format,
        &[
            (
                "mmkv_key_user_name".into(),
                Some(
                    wechat_profile::mmkv_string_value(state.own_username.as_bytes())
                        .unwrap()
                        .to_vec(),
                ),
            ),
            ("mmkv_key_latest_login_uin".into(), Some(vec![1])),
            (
                "key_wechat_device_id".into(),
                Some(
                    wechat_profile::mmkv_string_value(&state.device.device_id)
                        .unwrap()
                        .to_vec(),
                ),
            ),
        ],
    )
    .unwrap();
    let mmkv = core
        .store
        .account("one")
        .unwrap()
        .profile_path
        .join("identity");
    std::fs::write(&mmkv, pair.data()).unwrap();
    std::fs::write(mmkv.with_extension("crc"), pair.metadata()).unwrap();
    core.bind_profile_identity("one", &mmkv, &format).unwrap();
    core.save_state("one", &state).unwrap();
    core.store
        .put_runtime("one", "desired", b"connected")
        .unwrap();
    (tmp, core, upstream, received)
}
fn queue(core: &Core, n: usize) -> String {
    queue_download(
        core,
        n,
        "image",
        "standard",
        MediaDescriptor {
            file_id: format!("image-{n}"),
            aes_key: vec![1; 16],
            quality: MediaQuality::Standard,
            expected_bytes: None,
            expected_md5: None,
            strict_size: false,
            expires_at: None,
        },
    )
}
fn queue_download(
    core: &Core,
    n: usize,
    kind: &str,
    variant: &str,
    descriptor: MediaDescriptor,
) -> String {
    let id = format!("{kind}-{n}");
    let asset: MediaAsset = serde_json::from_value(json!({
        "id":id,"accountId":"one","messageId":null,"kind":kind,"variant":variant,"actualVariant":null,
        "state":"pending","contentType":null,"byteLength":null,"sha256":null,"filename":null,"expiresAt":null,
        "reason":null,"retryable":true,"jobId":null,"contentUrl":null,"animationFrames":null,"officialWriteback":"unsupported","preview":null
    })).unwrap();
    let plan = DownloadPlan::Cdn { descriptor };
    let plan = core
        .vault
        .seal(
            "one",
            &format!("plan:{id}"),
            &serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
    core.store
        .put_media_asset(
            "one",
            &MediaAssetRecord {
                id: id.clone(),
                message_id: None,
                kind: kind.into(),
                variant: variant.into(),
                metadata: serde_json::to_value(asset).unwrap(),
                plan,
                payload: None,
            },
        )
        .unwrap();
    let job = format!("download-{n}");
    core.store
        .create_media_job(
            "one",
            &job,
            &job,
            &digest(job.as_bytes()),
            "download",
            &json!({"assetId":id}),
            b"",
        )
        .unwrap();
    job
}
fn release(upstream: &PausedDownloads) {
    let (lock, signal) = &*upstream.release;
    *lock.lock().unwrap() = true;
    signal.notify_all();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_image_downloads_overlap_without_blocking_history_or_outgoing_admission() {
    let (_tmp, core, upstream, received) = fixture();
    // Incoming downloads do not queue behind the outgoing submission guard.
    let outgoing = core.lock_media_job("one").await.unwrap();
    let jobs: Vec<_> = (0..3).map(|n| queue(&core, n)).collect();
    let mut workers = Vec::new();
    for id in &jobs {
        let local = core.clone();
        let id = id.clone();
        workers.push(tokio::spawn(async move {
            local.run_media_worker("one", &id).await
        }));
    }
    let _received = tokio::task::spawn_blocking(move || {
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(received.try_recv().is_err());
        received
    })
    .await
    .unwrap();
    assert_eq!(upstream.count.load(Ordering::SeqCst), 2);
    core.read("one", |view| {
        view.catalog_page("one", "threads", None, &Default::default())
    })
    .await
    .unwrap();
    let writer = tokio::time::timeout(Duration::from_secs(2), core.lock_account("one"))
        .await
        .unwrap()
        .unwrap();
    core.store.append_event("one", "test", &json!({})).unwrap();
    drop(writer);
    drop(outgoing);
    release(&upstream);
    for worker in workers {
        worker.await.unwrap().unwrap();
    }
    for id in &jobs {
        assert_eq!(core.store.media_job("one", id).unwrap().state, "available");
    }
    core.clone()
        .run_media_worker("one", &jobs[0])
        .await
        .unwrap();
    assert_eq!(upstream.count.load(Ordering::SeqCst), 3); // Same job never downloads twice.
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_image_cannot_publish_after_its_network_wait() {
    let (_tmp, core, upstream, received) = fixture();
    let id = queue(&core, 0);
    let local = core.clone();
    let job = id.clone();
    let worker = tokio::spawn(async move { local.run_media_worker("one", &job).await });
    tokio::task::spawn_blocking(move || received.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    core.cancel_media_job("one", &id).unwrap();
    release(&upstream);
    worker.await.unwrap().unwrap();
    assert_eq!(core.store.media_job("one", &id).unwrap().state, "cancelled");
    assert_ne!(
        core.media_asset("one", "image-0").unwrap().state,
        "available"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logout_during_an_image_download_rejects_the_late_result() {
    let (_tmp, core, upstream, received) = fixture();
    let id = queue(&core, 0);
    let local = core.clone();
    let job = id.clone();
    let worker = tokio::spawn(async move { local.run_media_worker("one", &job).await });
    tokio::task::spawn_blocking(move || received.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    let guard = core.lock_account("one").await.unwrap();
    core.store
        .put_runtime("one", "logout-epoch", b"new-login-boundary")
        .unwrap();
    drop(guard);
    release(&upstream);
    worker.await.unwrap().unwrap();
    assert_ne!(core.store.media_job("one", &id).unwrap().state, "available");
    assert_ne!(
        core.media_asset("one", "image-0").unwrap().state,
        "available"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_normal_video_is_published_but_cannot_replace_an_original() {
    for variant in [
        MediaVariant::Normal,
        MediaVariant::Original,
        MediaVariant::Thumbnail,
    ] {
        let (tmp, core, upstream, _received) = fixture();
        let source = tmp.path().join("synthetic.mp4");
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=green:s=32x32:d=0.1",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
                "-y",
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success());
        let bytes = std::fs::read(source).unwrap();
        *upstream.video_reply.lock().unwrap() = Some(bytes.clone());
        let variant_name = match variant {
            MediaVariant::Normal => "normal",
            MediaVariant::Original => "original",
            MediaVariant::Thumbnail => "thumbnail",
            _ => unreachable!(),
        };
        let job = queue_download(
            &core,
            0,
            "video",
            variant_name,
            MediaDescriptor {
                file_id: format!("synthetic-video-{variant_name}"),
                aes_key: vec![1; 16],
                quality: if variant == MediaVariant::Thumbnail {
                    MediaQuality::Thumbnail
                } else {
                    MediaQuality::Video
                },
                expected_bytes: Some(bytes.len() as u64 + 32),
                expected_md5: Some("0".repeat(32)),
                // Old discovery persisted strict normal-video plans as well.
                strict_size: true,
                expires_at: None,
            },
        );
        release(&upstream);
        core.clone().run_media_worker("one", &job).await.unwrap();
        let status = core.media_job("one", &job).unwrap();
        if variant == MediaVariant::Normal {
            assert_eq!(
                status.state,
                "available",
                "{}",
                serde_json::to_string(&status).unwrap()
            );
            let saved = core.media_asset("one", "video-0").unwrap();
            assert_eq!(saved.actual_variant, Some(MediaVariant::Normal));
            assert_eq!(saved.sha256.as_deref(), Some(digest(&bytes).as_str()));
            assert_eq!(saved.byte_length, Some(bytes.len() as u64));
        } else {
            assert_eq!(status.state, "unsupported");
            assert_eq!(status.reason.as_deref(), Some("unsupported_media"));
            assert_ne!(
                core.media_asset("one", "video-0").unwrap().state,
                "available"
            );
        }
        core.clone().run_media_worker("one", &job).await.unwrap();
        assert_eq!(upstream.count.load(Ordering::SeqCst), 1);
    }
}

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut value = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    value.extend_from_slice(kind);
    value.extend_from_slice(payload);
    value
}
/// A structurally valid synthetic MP4 for container checks.
fn channels_mp4(fill: u8) -> Vec<u8> {
    [
        atom(b"ftyp", b"isom\0\0\x02\0isomiso2"),
        atom(b"moov", &[0; 32]),
        atom(b"mdat", &vec![fill; 4096]),
    ]
    .concat()
}
/// Store one received Channels card in the fixture profile and return its message ID.
fn insert_channels_card(tmp: &tempfile::TempDir) -> String {
    use md5::Digest;
    let db = rusqlite::Connection::open(tmp.path().join("account/db_storage/message/message_0.db"))
        .unwrap();
    let password = [42u8; 32];
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
        0
    );
    db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000; INSERT INTO Name2Id(rowid,user_name,is_session)VALUES(1,'room@chatroom',1),(2,'sender',0);").unwrap();
    let table = format!("Msg_{:x}", md5::Md5::digest(b"room@chatroom"));
    db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER)")).unwrap();
    let xml = "sender:\n<msg><appmsg><type>51</type><title>Upgrade notice</title><url>https://example.invalid/update/</url><finderFeed><objectId>18446744073709551000</objectId><feedType>4</feedType><nickname>Publisher</nickname><desc>Synthetic post</desc><mediaCount>1</mediaCount><objectNonceId>12345678901234567890_0_38_12_4_context</objectNonceId><mediaList><media><videoPlayDuration>96</videoPlayDuration><url>https://example.invalid/private-video</url><coverUrl>https://example.invalid/private-cover</coverUrl><mediaType>4</mediaType></media></mediaList></finderFeed></appmsg></msg>";
    db.execute(
        &format!("INSERT INTO {table} VALUES(1,1,?1,1,2,?2,'',0,0)"),
        rusqlite::params![(51i64 << 32) | 49, xml],
    )
    .unwrap();
    crate::message_id(&wechat_profile::SourceMessageId {
        database: "message/message_0.db".into(),
        table,
        local_id: "1".into(),
    })
    .unwrap()
}
async fn wait_for_job(core: &Core, job: &str) -> String {
    for _ in 0..200 {
        let state = core.store.media_job("one", job).unwrap().state;
        if !matches!(state.as_str(), "queued" | "running") {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("media job did not finish");
}
fn channels_files(tmp: &tempfile::TempDir) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(tmp.path().join("data/accounts/one/agent/media"))
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channels_video_is_requested_once_cached_as_a_sidecar_file_and_renewed_after_a_miss() {
    let (tmp, core, upstream, _received) = fixture();
    let message = insert_channels_card(&tmp);

    let card = core.message("one", &message).unwrap();
    assert_eq!(
        card["body"],
        "[Channels video] Publisher: Synthetic post (1:36)"
    );
    assert_eq!(card["link"]["durationSeconds"], 96);
    assert!(card["link"]["url"].is_null() && card["link"]["title"].is_null());

    // Receiving and listing the card makes no Channels request.
    let assets = core.media_assets("one", &message).unwrap();
    assert_eq!(assets.len(), 2);
    let cover = &assets[0];
    assert_eq!(
        (cover.kind, cover.variant),
        (MediaKind::Image, MediaVariant::Thumbnail)
    );
    let video = assets[1].clone();
    assert_eq!(
        (video.kind, video.variant),
        (MediaKind::Video, MediaVariant::Normal)
    );
    assert_eq!(video.advertised_byte_length, None);
    assert_eq!(video.auto_download_allowed, Some(false));
    assert_eq!(video.official_writeback, "unsupported");
    assert_eq!(video.state, "pending");
    let public = serde_json::to_string(&(&card, &assets)).unwrap();
    for private in [
        "18446744073709551000",
        "12345678901234567890",
        "private-video",
        "private-cover",
        "Upgrade notice",
    ] {
        assert!(!public.contains(private), "{private}");
    }
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 0);

    // Two requests for the same video share one job and one fetch.
    let first_bytes = channels_mp4(1);
    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Ok(first_bytes.clone()));
    let request = || MediaTransferRequest {
        message_id: message.clone(),
        asset_id: Some(video.id.clone()),
        quality: MediaVariant::Normal,
    };
    upstream.hold_channels();
    let first = core.start_media_transfer("one", request()).unwrap();
    let again = core.start_media_transfer("one", request()).unwrap();
    assert_eq!(first.id, again.id);
    upstream.release_channels();
    assert_eq!(wait_for_job(&core, &first.id).await, "available");
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 1);
    let cached = core.media_asset("one", &video.id).unwrap();
    assert_eq!(cached.state, "available");
    assert_eq!(cached.content_type.as_deref(), Some("video/mp4"));
    assert_eq!(cached.official_writeback, "unsupported");
    assert_eq!(
        core.media_content("one", &video.id).unwrap().bytes,
        first_bytes
    );
    let files = channels_files(&tmp);
    assert_eq!(files.len(), 1, "one sidecar file holds the video");
    assert!(core
        .store
        .media_asset_profile_key("one", &video.id)
        .unwrap()
        .is_none());

    // A cached video is reused without another fetch.
    let reuse = core.start_media_transfer("one", request()).unwrap();
    assert_eq!(reuse.id, first.id);
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 1);

    // When the file disappears, reading reports the miss without fetching, and the next
    // request resolves again in one new job.
    std::fs::remove_file(&files[0]).unwrap();
    let Err(miss) = core.media_content("one", &video.id) else {
        panic!("a deleted cache file must not read as content");
    };
    assert!(format!("{miss:#}").contains("media_not_cached"));
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 1);
    let second_bytes = channels_mp4(2);
    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Ok(second_bytes.clone()));
    upstream.hold_channels();
    let renewed = core.start_media_transfer("one", request()).unwrap();
    assert_ne!(renewed.id, first.id);
    let duplicate = core.start_media_transfer("one", request()).unwrap();
    assert_eq!(duplicate.id, renewed.id);
    upstream.release_channels();
    assert_eq!(wait_for_job(&core, &renewed.id).await, "available");
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        core.media_content("one", &video.id).unwrap().bytes,
        second_bytes
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channels_failures_publish_nothing_and_a_new_request_resolves_again() {
    let (tmp, core, upstream, _received) = fixture();
    let message = insert_channels_card(&tmp);
    let video = core.media_assets("one", &message).unwrap()[1].clone();
    let request = || MediaTransferRequest {
        message_id: message.clone(),
        asset_id: Some(video.id.clone()),
        quality: MediaVariant::Normal,
    };

    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Err("channels_download_failed".into()));
    let failed = core.start_media_transfer("one", request()).unwrap();
    assert_eq!(wait_for_job(&core, &failed.id).await, "failed");
    let row = core.store.media_job("one", &failed.id).unwrap();
    assert_eq!(row.error_code.as_deref(), Some("channels_download_failed"));
    assert!(row.retryable);
    assert_ne!(
        core.media_asset("one", &video.id).unwrap().state,
        "available"
    );
    assert!(channels_files(&tmp).is_empty());

    // Not-a-video bytes are refused before publication.
    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Ok(b"<html>not a video</html>".to_vec()));
    let invalid = core.start_media_transfer("one", request()).unwrap();
    assert_ne!(invalid.id, failed.id);
    assert_eq!(wait_for_job(&core, &invalid.id).await, "failed");
    assert_eq!(
        core.store
            .media_job("one", &invalid.id)
            .unwrap()
            .error_code
            .as_deref(),
        Some("channels_integrity_failed")
    );
    let row = core.store.media_job("one", &invalid.id).unwrap();
    let detail = &row.data["result"]["channelsDownloadFailure"];
    assert_eq!(detail["check"], "container");
    assert_eq!(detail["receivedBytes"], 24);
    assert!(channels_files(&tmp).is_empty());

    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Err("channels_post_unavailable".into()));
    let gone = core.start_media_transfer("one", request()).unwrap();
    assert_ne!(gone.id, invalid.id);
    assert_eq!(wait_for_job(&core, &gone.id).await, "failed");
    let row = core.store.media_job("one", &gone.id).unwrap();
    assert_eq!(row.error_code.as_deref(), Some("channels_post_unavailable"));
    assert!(!row.retryable);
    // The failed job never retries by itself; asking again starts exactly one new lookup.
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 3);
    let bytes = channels_mp4(3);
    upstream
        .channels_replies
        .lock()
        .unwrap()
        .push(Ok(bytes.clone()));
    upstream.hold_channels();
    let again = core.start_media_transfer("one", request()).unwrap();
    assert_ne!(again.id, gone.id);
    assert_eq!(
        core.start_media_transfer("one", request()).unwrap().id,
        again.id
    );
    upstream.release_channels();
    assert_eq!(wait_for_job(&core, &again.id).await, "available");
    assert_eq!(upstream.channels_count.load(Ordering::SeqCst), 4);
    assert_eq!(core.media_content("one", &video.id).unwrap().bytes, bytes);
}
