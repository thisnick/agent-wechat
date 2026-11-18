//! Sealed plans rewritten without bytes the account keeps elsewhere. All
//! values are synthetic. The upstream counts voice transfers and refuses every
//! native request, so a rewrite or reuse that tried to transfer would fail.
use super::sticker_pack_download::{Acquisition, PackContents};
use super::*;
use std::sync::atomic::AtomicUsize;
use wechat_protocol::sticker_packs as wire;

#[derive(Default)]
struct CountingUpstream {
    voice_transfers: AtomicUsize,
}
impl crate::upstream::Upstream for CountingUpstream {
    fn kind(&self) -> crate::upstream::UpstreamKind {
        crate::upstream::UpstreamKind::Native
    }
    fn download_voice(
        &self,
        _: &mut wechat_protocol::AccountState,
        _: u64,
        _: u32,
        _: u32,
    ) -> Result<wechat_protocol::wire::VoiceChunk> {
        self.voice_transfers.fetch_add(1, Ordering::SeqCst);
        bail!("transport_unavailable")
    }
}
/// A managed official profile whose only upstream counts transfers.
fn counting_core() -> (tempfile::TempDir, Arc<Core>, Arc<CountingUpstream>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let upstream = Arc::new(CountingUpstream::default());
    let core = Core::with_upstream(store, None, upstream.clone()).unwrap();
    let mut state = tests::accepted_media_state();
    state.device.device_id = vec![b'A'; 16];
    state.uin = 7;
    state.cookie = vec![1; 15];
    state.auto_auth_key = wechat_protocol::protobuf::message(&[
        wechat_protocol::protobuf::bytes(1, &wechat_protocol::protobuf::buffer(b"synthetic-a")),
        wechat_protocol::protobuf::bytes(2, &wechat_protocol::protobuf::buffer(b"synthetic-b")),
    ])
    .unwrap();
    state.key_info = vec![4; 180];
    state.database_password = vec![42; 32];
    core.test_managed_profile("one", &state).unwrap();
    (dir, core, upstream)
}
fn control(dir: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.path().join("data/accounts/one/agent/control.sqlite")).unwrap()
}
fn voice_plan(core: &Core, id: &str) -> (usize, Option<Vec<u8>>) {
    let row = core.store.media_asset("one", id).unwrap();
    let DownloadPlan::Voice { inline, .. } = core.asset_plan("one", &row).unwrap().plan else {
        panic!("not a voice plan");
    };
    (row.plan.len(), inline)
}
/// Seal a voice plan as earlier versions wrote it, with encoded audio inside.
fn with_audio(core: &Core, id: &str, audio: Vec<u8>) {
    let row = core.store.media_asset("one", id).unwrap();
    let mut authorized = core.asset_plan("one", &row).unwrap();
    let DownloadPlan::Voice { inline, .. } = &mut authorized.plan else {
        panic!("not a voice plan");
    };
    *inline = Some(audio);
    let sealed = core
        .vault
        .seal(
            "one",
            &format!("plan:{id}"),
            &serde_json::to_vec(&authorized).unwrap(),
        )
        .unwrap();
    assert!(core
        .store
        .replace_media_asset_plan("one", id, &row.plan, &sealed)
        .unwrap());
}

#[test]
fn new_voice_plans_leave_audio_the_profile_holds_out_of_the_plan() {
    let (_dir, core, _) = counting_core();
    let (_, assets) = storage_tests::profile_voice(&core);
    assert_eq!(assets.len(), 2);
    for asset in &assets {
        let (length, inline) = voice_plan(&core, &asset.id);
        assert!(inline.is_none());
        assert!(length < 1024, "voice plan is {length} bytes");
    }
}

#[test]
fn voice_plans_drop_audio_the_profile_holds_and_downloads_read_the_profile() {
    let (_dir, core, upstream) = counting_core();
    let (encoded, assets) = storage_tests::profile_voice(&core);
    for asset in &assets {
        with_audio(&core, &asset.id, encoded.clone());
        assert!(voice_plan(&core, &asset.id).0 > 3 * encoded.len());
    }
    // One step of one plan, as if the process stopped after it: every plan is
    // either whole or rewritten, and each still opens.
    let first = core.compact_voice_plans("one", "", 1).unwrap();
    assert_eq!(first.rewritten, 1);
    let inline = assets
        .iter()
        .map(|asset| voice_plan(&core, &asset.id).1)
        .collect::<Vec<_>>();
    assert_eq!(inline.iter().filter(|audio| audio.is_none()).count(), 1);
    assert!(inline.contains(&Some(encoded.clone())));
    // The next pass finishes, and another finds nothing to do.
    assert_eq!(core.run_plan_compaction("one").unwrap().voice, 1);
    assert_eq!(core.run_plan_compaction("one").unwrap().voice, 0);
    for asset in &assets {
        let (length, inline) = voice_plan(&core, &asset.id);
        assert!(inline.is_none());
        assert!(length < 1024);
    }
    // A download reads the profile's copy; nothing is transferred.
    for (index, asset) in assets.iter().enumerate() {
        let job = format!("download-{index}");
        core.store
            .create_media_job(
                "one",
                &job,
                &format!("download:{job}"),
                &digest(job.as_bytes()),
                "download",
                &json!({"assetId":asset.id}),
                b"",
            )
            .unwrap();
        core.perform_download("one", &job).unwrap();
        let done = core.media_asset("one", &asset.id).unwrap();
        assert_eq!(done.state, "available");
        assert_eq!(done.official_writeback, "complete");
        let content = core.media_content("one", &asset.id).unwrap().bytes;
        if asset.variant == MediaVariant::Encoded {
            assert_eq!(content, encoded);
        } else {
            assert_eq!(
                content,
                wechat_protocol::silk::wav(&wechat_protocol::silk::decode(&encoded).unwrap())
                    .unwrap()
            );
        }
    }
    assert_eq!(upstream.voice_transfers.load(Ordering::SeqCst), 0);
}

#[test]
fn voice_plans_keep_audio_the_profile_does_not_return() {
    let (_dir, core, _) = counting_core();
    let (encoded, assets) = storage_tests::profile_voice(&core);
    // Same length, different bytes: the profile does not hold this audio.
    let mut other = encoded.clone();
    let last = other.len() - 1;
    other[last] ^= 0xff;
    with_audio(&core, &assets[0].id, other.clone());
    // A length a download would ignore is never read, so it is dropped.
    with_audio(&core, &assets[1].id, encoded[..encoded.len() - 1].to_vec());
    assert_eq!(core.run_plan_compaction("one").unwrap().voice, 1);
    assert_eq!(voice_plan(&core, &assets[0].id).1, Some(other));
    assert_eq!(voice_plan(&core, &assets[1].id).1, None);
}

#[test]
fn media_cache_repair_after_a_voice_rewrite_uses_only_owned_bytes() {
    let (_dir, core, upstream) = counting_core();
    let (encoded, assets) = storage_tests::profile_voice(&core);
    for asset in &assets {
        with_audio(&core, &asset.id, encoded.clone());
        let (bytes, mime) = if asset.variant == MediaVariant::Encoded {
            (encoded.clone(), "audio/silk")
        } else {
            (
                wechat_protocol::silk::wav(&wechat_protocol::silk::decode(&encoded).unwrap())
                    .unwrap(),
                "audio/wav",
            )
        };
        let cached = core
            .cache_asset("one", &asset.id, &bytes, mime, Some(asset.variant), None)
            .unwrap();
        assert_eq!(cached.official_writeback, "pending");
    }
    assert_eq!(core.run_plan_compaction("one").unwrap().voice, 2);
    let repaired = core.repair_media_writeback("one", 10).unwrap();
    assert_eq!(repaired["repaired"], 2, "{repaired}");
    for asset in &assets {
        assert_eq!(
            core.media_asset("one", &asset.id)
                .unwrap()
                .official_writeback,
            "complete"
        );
    }
    assert_eq!(upstream.voice_transfers.load(Ordering::SeqCst), 0);
}

const PACK: &str = "pack.fixture";
const ITEMS: [&str; 2] = [
    "2c5f182a5005cfdeec4bd4071fad0e39",
    "3cba29f7d86c1245ca196c29923a28eb",
];
fn archive() -> &'static [u8] {
    include_bytes!("../../wechat-media/tests/fixtures/sticker-pack.zip")
}
/// A finished acquisition's plan as earlier versions sealed it: the raw reply
/// and the decoded detail and captions, byte fields as JSON number arrays.
fn legacy_acquisition() -> Vec<u8> {
    let raw = |n: usize| vec![7_u8; n];
    let plan = Acquisition {
        pack: PACK.into(),
        item: None,
        fence: MediaAuthorizationFence {
            authorization: [3; 32],
            logout_epoch: None,
        },
        submitted: true,
        reply: None,
        detail: Some(wire::StickerPackDetail {
            summary: wire::StickerPackSummary {
                id: PACK.into(),
                name: "Fixture".into(),
                description: String::new(),
                icon_url: "https://example.invalid/icon".into(),
                cover_url: "https://example.invalid/cover".into(),
                pack_type: 4,
                flags: 0,
                expiry: 0,
                raw: raw(2_000),
            },
            items: ITEMS
                .iter()
                .map(|id| wire::StickerPackItem {
                    id: (*id).into(),
                    original_url: "https://example.invalid/original".into(),
                    thumbnail_url: "https://example.invalid/thumbnail".into(),
                    raw: raw(4_000),
                })
                .collect(),
            raw: raw(12_000),
        }),
        captions: Some(wire::StickerPackCaptions {
            items: vec![wire::StickerPackCaption {
                id: ITEMS[0].into(),
                captions: vec![("en".into(), "wave".into())],
            }],
            raw: raw(1_000),
        }),
        contents: None,
        locator: Some(MediaDescriptor {
            file_id: "synthetic-file".into(),
            aes_key: vec![5; 16],
            quality: wechat_protocol::media::MediaQuality::File,
            expected_bytes: Some(archive().len() as u64),
            expected_md5: None,
            strict_size: false,
            expires_at: None,
        }),
    };
    let mut value = serde_json::to_value(&plan).unwrap();
    value["reply"] = json!(raw(3_000));
    serde_json::to_vec(&value).unwrap()
}
/// A finished acquisition of a pack that was not added, with its archive.
fn finished_acquisition(core: &Core, dir: &tempfile::TempDir, id: &str, days_ago: i64) {
    let payload = core
        .vault
        .seal("one", &format!("media-job:{id}"), &legacy_acquisition())
        .unwrap();
    let (mut row, _) = core
        .store
        .create_media_job(
            "one",
            id,
            &format!("pack-download:{id}"),
            &digest(id.as_bytes()),
            "sticker_acquire",
            &json!({"packId":PACK,"preparationId":id,"serverAcceptance":"accepted","profileWriteback":"not_applicable"}),
            &payload,
        )
        .unwrap();
    core.store_pack_bytes(
        "one",
        id,
        &format!("{id}_archive"),
        "pack.zip",
        archive(),
        "application/zip",
        0,
    )
    .unwrap();
    row.state = "available".into();
    row.phase = "completed".into();
    row.retryable = false;
    core.store.save_media_job("one", &row).unwrap();
    let when = (chrono::Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339();
    control(dir)
        .execute(
            "UPDATE media_jobs SET updated_at=?2 WHERE id=?1",
            rusqlite::params![id, when],
        )
        .unwrap();
}
fn acquisition(core: &Core, id: &str) -> (usize, Acquisition) {
    let row = core.store.media_job("one", id).unwrap();
    let plain = core
        .vault
        .open_record("one", &format!("media-job:{id}"), &row.payload)
        .unwrap();
    (row.payload.len(), serde_json::from_slice(&plain).unwrap())
}

#[test]
fn reduced_acquisitions_still_let_a_re_added_pack_reuse_its_archive() {
    let (dir, core, upstream) = counting_core();
    finished_acquisition(&core, &dir, "job_old", 2);
    finished_acquisition(&core, &dir, "job_recent", 0);
    let (before, legacy) = acquisition(&core, "job_old");
    assert!(legacy.detail.is_some() && legacy.reply.is_some());
    let updated = core.store.media_job("one", "job_old").unwrap().updated_at;

    assert_eq!(
        core.run_plan_compaction("one")
            .unwrap()
            .sticker_acquisitions,
        1
    );
    assert_eq!(
        core.run_plan_compaction("one")
            .unwrap()
            .sticker_acquisitions,
        0
    );
    let (after, reduced) = acquisition(&core, "job_old");
    assert!(after * 10 < before, "{after} of {before} bytes");
    assert!(reduced.reply.is_none() && reduced.detail.is_none() && reduced.captions.is_none());
    let PackContents { items, captions } = reduced.contents.clone().unwrap();
    assert_eq!(items, ITEMS);
    assert_eq!(captions.len(), 1);
    assert_eq!(
        captions[0].captions,
        vec![("en".to_string(), "wave".to_string())]
    );
    assert!(reduced.locator.is_some());
    // The job itself is unchanged, including the time that orders reuse.
    let row = core.store.media_job("one", "job_old").unwrap();
    assert_eq!((row.state.as_str(), row.updated_at), ("available", updated));
    // A finished acquisition less than a day old keeps its whole plan.
    assert!(acquisition(&core, "job_recent").1.detail.is_some());

    // Adding the pack later reuses the newest owned archive. Make the reduced
    // plan the only candidate, then add the pack.
    control(&dir)
        .execute(
            "UPDATE media_jobs SET state='failed' WHERE id='job_recent'",
            [],
        )
        .unwrap();
    core.ensure_sticker_database("one", "test").unwrap();
    core.project_sticker_packs(
        "one",
        vec![wechat_profile::StickerPackRecord {
            id: PACK.into(),
            name: "Fixture".into(),
            sort_order: 1,
            ..Default::default()
        }],
    )
    .unwrap();
    core.promote_owned_sticker_pack("one", PACK).unwrap();
    let row = core.store.media_job("one", "job_old").unwrap();
    assert_eq!(row.data["profileWriteback"], "complete");
    assert_eq!(row.data["result"]["membership"], "added");
    let decoded =
        wechat_media::decode_sticker_archive(archive(), PACK, &ITEMS.map(String::from)).unwrap();
    assert_eq!(
        core.pack_cache_item("one", PACK, ITEMS[1], "original")
            .unwrap(),
        decoded.items[1].original
    );
    assert_eq!(upstream.voice_transfers.load(Ordering::SeqCst), 0);
}

#[test]
fn new_job_plans_write_byte_fields_as_text_and_read_earlier_arrays() {
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Plan {
        #[serde(default, with = "super::plan_compaction::plan_bytes::option")]
        one: Option<Vec<u8>>,
        #[serde(with = "super::plan_compaction::plan_bytes::list")]
        many: Vec<Vec<u8>>,
    }
    let plan = Plan {
        one: Some(vec![0, 1, 2, 255]),
        many: vec![vec![9; 300], vec![]],
    };
    let written = serde_json::to_value(&plan).unwrap();
    assert_eq!(written["one"], "AAEC/w==");
    assert!(written["many"][0].is_string());
    assert_eq!(serde_json::from_value::<Plan>(written).unwrap(), plan);
    let earlier = json!({"one":[0,1,2,255],"many":[vec![9;300],[]]});
    assert_eq!(serde_json::from_value::<Plan>(earlier).unwrap(), plan);
    assert_eq!(
        serde_json::from_value::<Plan>(json!({"one":null,"many":[]})).unwrap(),
        Plan {
            one: None,
            many: vec![]
        }
    );
    // Encoded size against the earlier number arrays.
    let bytes = (0..=255u8).cycle().take(30_000).collect::<Vec<_>>();
    let text = serde_json::to_vec(&Plan {
        one: Some(bytes.clone()),
        many: vec![],
    })
    .unwrap();
    let array = serde_json::to_vec(&json!({"one":bytes,"many":[]})).unwrap();
    assert!(
        text.len() * 2 < array.len(),
        "{} vs {}",
        text.len(),
        array.len()
    );
}

#[test]
fn a_removed_never_downloaded_row_comes_back_when_the_message_is_listed() {
    let (dir, core, upstream) = counting_core();
    // A shared link's thumbnail, and a voice note whose rows stay.
    let link = core
        .project_accepted_media(
            "one",
            "filehelper",
            49,
            std::str::from_utf8(include_bytes!("../tests/fixtures/shared-link.xml")).unwrap(),
            "Synthetic preview",
            1_700_000_100,
            "9000000000000000002",
            "link-test",
        )
        .unwrap();
    let message = crate::message_id(&link).unwrap();
    let thumbnails = core.media_assets("one", &message).unwrap();
    assert!(!thumbnails.is_empty());
    assert!(thumbnails.iter().all(|asset| asset.state == "pending"));
    let (_, voice) = storage_tests::profile_voice(&core);
    let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    control(&dir)
        .execute(
            "UPDATE media_assets SET updated_at=?1",
            rusqlite::params![old],
        )
        .unwrap();
    let retention = core.maintain_media_storage("one").unwrap()["retention"].clone();
    assert_eq!(retention["idleAssets"], thumbnails.len(), "{retention}");
    for asset in &thumbnails {
        assert_eq!(
            core.media_asset("one", &asset.id).unwrap_err().to_string(),
            "operation_expired"
        );
    }
    for asset in &voice {
        assert_eq!(core.media_asset("one", &asset.id).unwrap().id, asset.id);
    }
    let again = core.media_assets("one", &message).unwrap();
    assert_eq!(
        again.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        thumbnails.iter().map(|a| a.id.clone()).collect::<Vec<_>>()
    );
    for asset in &again {
        assert_eq!(core.media_asset("one", &asset.id).unwrap().state, "pending");
    }
    // A listing restarts the idle clock: rows listed after aging stay.
    control(&dir)
        .execute(
            "UPDATE media_assets SET updated_at=?1",
            rusqlite::params![old],
        )
        .unwrap();
    core.media_assets("one", &message).unwrap();
    let retention = core.maintain_media_storage("one").unwrap()["retention"].clone();
    assert_eq!(retention["idleAssets"], 0, "{retention}");
    for asset in &again {
        assert!(core.media_asset("one", &asset.id).is_ok());
    }
    assert_eq!(upstream.voice_transfers.load(Ordering::SeqCst), 0);
}
