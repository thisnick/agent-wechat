use super::*;
use crate::upstream::{media::MediaTransport, Upstream, UpstreamKind};
use std::sync::{atomic::AtomicUsize, Condvar, Mutex};
use wechat_protocol::media::DownloadedMedia;

struct PausedDownloads {
    starts: std::sync::mpsc::Sender<String>,
    release: Arc<(Mutex<bool>, Condvar)>,
    count: Arc<AtomicUsize>,
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
    fn upload_image(&self, _: &PreparedImageUpload) -> Result<UploadedImage> {
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
fn fixture() -> (
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
    let id = format!("image-{n}");
    let asset: MediaAsset = serde_json::from_value(json!({
        "id":id,"accountId":"one","messageId":null,"kind":"image","variant":"standard","actualVariant":null,
        "state":"pending","contentType":null,"byteLength":null,"sha256":null,"filename":null,"expiresAt":null,
        "reason":null,"retryable":true,"jobId":null,"contentUrl":null,"animationFrames":null,"officialWriteback":"unsupported","preview":null
    })).unwrap();
    let plan = DownloadPlan::Cdn {
        descriptor: MediaDescriptor {
            file_id: id.clone(),
            aes_key: vec![1; 16],
            quality: MediaQuality::Standard,
            expected_bytes: None,
            expected_md5: None,
            strict_size: false,
            expires_at: None,
        },
    };
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
                kind: "image".into(),
                variant: "standard".into(),
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
