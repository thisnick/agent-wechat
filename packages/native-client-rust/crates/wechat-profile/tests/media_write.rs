use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use rusqlite::Connection;
use sha2::Sha256;
use std::{collections::BTreeMap, fs, io::Cursor, path::Path};
use wechat_profile::*;
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (name, sql) in [
        (
            "contact/contact.db",
            include_str!("../src/layout/contact.db.sql"),
        ),
        (
            "session/session.db",
            include_str!("../src/layout/session.db.sql"),
        ),
        (
            "message/message_0.db",
            include_str!("../src/layout/message_0.db.sql"),
        ),
        (
            "message/media_0.db",
            include_str!("../src/layout/media_0.db.sql"),
        ),
        (
            "message/message_resource.db",
            include_str!("../src/layout/message_resource.db.sql"),
        ),
        (
            "hardlink/hardlink.db",
            include_str!("../src/layout/hardlink.db.sql"),
        ),
        (
            "head_image/head_image.db",
            include_str!("../src/layout/head_image.db.sql"),
        ),
    ] {
        let path = d.path().join("account/db_storage").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }
    d
}
fn options() -> WriteOptions {
    WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: true,
        durable_intent_id: "synthetic-media-intent".into(),
    }
}
fn row(kind: i64) -> MessageProjection {
    let mut f = BTreeMap::new();
    for (k, n) in [
        ("local_type", kind),
        ("sort_seq", 100000),
        ("create_time", 100),
        ("status", 3),
        ("upload_status", 0),
        ("download_status", 0),
        ("server_seq", 8),
        ("origin_source", 2),
        ("WCDB_CT_message_content", 0),
        ("WCDB_CT_source", 0),
    ] {
        f.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    for (k, v) in [
        ("message_content", "synthetic"),
        ("source", ""),
        ("compress_content", ""),
    ] {
        f.insert(k.into(), SqlValue::Text(v.into()));
    }
    MessageProjection {
        peer: "peer".into(),
        sender: "peer".into(),
        from: "peer".into(),
        to: "self".into(),
        server_message_id: "18446744073709551615".into(),
        fields: f,
        packed_other_fields: vec![88, 0],
    }
}
fn reader(location: ProfileLocation, keys: KeyFile) -> ProfileReader {
    ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap()
}
fn hardlink(bytes: &[u8]) -> HardlinkProjection {
    let mut f = RawFields::new();
    for (k, n) in [
        ("md5_hash", 42),
        ("type", 1),
        ("file_size", bytes.len() as i64),
        ("modify_time", 100),
    ] {
        f.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    f.insert(
        "md5".into(),
        SqlValue::Text(format!("{:x}", Md5::digest(bytes))),
    );
    f.insert("file_name".into(), SqlValue::Text("synthetic.txt".into()));
    f.insert("extra_buffer".into(), SqlValue::Blob(STANDARD.encode([])));
    HardlinkProjection {
        kind: HardlinkKind::File,
        first_directory: "2026-10".into(),
        second_directory: None,
        fields: f,
    }
}
#[test]
fn encoded_voice_uses_destination_namespace_and_conflicts_do_not_replace_bytes() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let applied = apply_message_projection(&location, &keys, &row(34), &o).unwrap();
    let bytes = b"\x02#!SILK_V3\x02\x00ab";
    let a =
        cache_voice_projection(&location, &keys, &applied.source_id, "peer", bytes, &o).unwrap();
    assert!(a.changed);
    assert!(
        !cache_voice_projection(&location, &keys, &applied.source_id, "peer", bytes, &o)
            .unwrap()
            .changed
    );
    assert!(
        cache_voice_projection(&location, &keys, &applied.source_id, "other", bytes, &o).is_err()
    );
    assert!(
        cache_voice_projection(&location, &keys, &applied.source_id, "peer", b"other", &o).is_err()
    );
    let r = reader(location.clone(), keys.clone());
    assert_eq!(
        r.cached_voice(
            &a.database,
            "peer",
            &applied.source_id.local_id,
            Some("18446744073709551615")
        )
        .unwrap()
        .bytes,
        bytes
    );
    drop(r);
    fs::copy(
        d.path().join("account/db_storage/message/media_0.db"),
        d.path().join("account/db_storage/message/media_1.db"),
    )
    .unwrap();
    assert!(matches!(
        cache_voice_projection(&location, &keys, &applied.source_id, "peer", bytes, &o),
        Err(ProfileError::WritesUnsupported)
    ));
}
#[test]
fn file_cache_hardlink_resource_and_avatar_roundtrip_without_opaque_defaults() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let applied = apply_message_projection(&location, &keys, &row(49), &o).unwrap();
    let bytes = b"Synthetic cache payload only\n";
    let path = "msg/file/2026-10/synthetic.txt";
    let hash = hex::encode(Sha256::digest(bytes));
    assert!(publish_cache_file(&location, path, bytes, &hash, &o).unwrap());
    assert!(!publish_cache_file(&location, path, bytes, &hash, &o).unwrap());
    assert!(publish_cache_file(
        &location,
        path,
        b"different",
        &hex::encode(Sha256::digest(b"different")),
        &o
    )
    .is_err());
    assert!(publish_cache_file(&location, "msg/../escape", bytes, &hash, &o).is_err());
    let h = hardlink(bytes);
    let a = apply_hardlink_projection(&location, &keys, &h, &o).unwrap();
    assert!(a.changed);
    assert!(
        !apply_hardlink_projection(&location, &keys, &h, &o)
            .unwrap()
            .changed
    );
    let mut detail = RawFields::new();
    for (k, n) in [
        ("type", 1),
        ("size", bytes.len() as i64),
        ("create_time", 100),
        ("access_time", 0),
        ("status", 1),
    ] {
        detail.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    detail.insert("data_index".into(), SqlValue::Text("0".into()));
    detail.insert("packed_info".into(), SqlValue::Blob(STANDARD.encode([])));
    let input = MediaResourceProjection {
        source: applied.source_id,
        chat: "peer".into(),
        packed_info: vec![],
        details: vec![detail],
    };
    let first = apply_media_resource_projection(&location, &keys, &input, &o).unwrap();
    assert!(first.changed);
    let next = apply_media_resource_projection(&location, &keys, &input, &o).unwrap();
    assert!(!next.changed);
    assert_eq!(first.message_id, next.message_id);
    assert_eq!(first.detail_ids, next.detail_ids);
    let mut out = Cursor::new(vec![]);
    image::DynamicImage::new_rgb8(3, 2)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    let png = out.into_inner();
    let avatar = AvatarProjection {
        username: "peer",
        source_url_md5: "00112233445566778899aabbccddeeff",
        bytes: &png,
        update_time: 100,
    };
    assert!(apply_avatar_projection(&location, &keys, &avatar, &o).unwrap());
    assert!(!apply_avatar_projection(&location, &keys, &avatar, &o).unwrap());
    let r = reader(location, keys);
    assert_eq!(
        r.cached_hardlink_file(HardlinkKind::File, &format!("{:x}", Md5::digest(bytes)))
            .unwrap()
            .bytes,
        bytes
    );
    assert_eq!(r.avatar("peer").unwrap().bytes, png);
}
#[test]
fn publication_refuses_symlinks_and_schema_extensions() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let bytes = b"payload";
    let hash = hex::encode(Sha256::digest(bytes));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(d.path(), d.path().join("account/msg")).unwrap();
        assert!(matches!(
            publish_cache_file(&location, "msg/file/x", bytes, &hash, &o),
            Err(ProfileError::UnsupportedFile)
        ));
    }
    let p = d.path().join("account/db_storage/hardlink/hardlink.db");
    Connection::open(p)
        .unwrap()
        .execute_batch("ALTER TABLE file_hardlink_info_v4 ADD COLUMN unknown_field INTEGER")
        .unwrap();
    assert!(matches!(
        apply_hardlink_projection(&location, &keys, &hardlink(bytes), &o),
        Err(ProfileError::UnsupportedSchema(_))
    ));
    assert!(!Path::new(d.path()).join("file/x").exists());
}

#[test]
fn image_quality_requires_message_resource_type_and_encoded_size_binding() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let mut keys = KeyFile::empty("account");
    keys.image_dat = Some(ImageDatKey::new([7; 16], 19));
    let o = options();
    fn png(w: u32, h: u32, extra: u8) -> Vec<u8> {
        let mut out = Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(w, h)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        let mut bytes = out.into_inner();
        bytes.resize(1100 + extra as usize, extra);
        bytes
    }
    let original = png(32, 32, 1);
    let standard = png(16, 16, 2);
    let thumbnail = png(8, 8, 3);
    let original_hash = format!("{:x}", Md5::digest(&original));
    let base = "0123456789abcdef0123456789abcdef";
    let directory = format!("{:x}", Md5::digest(b"peer"));
    let mut msg = row(3);
    msg.fields.insert(
        "message_content".into(),
        SqlValue::Text(format!("<msg><img md5=\"{original_hash}\"/></msg>")),
    );
    let applied = apply_message_projection(&location, &keys, &msg, &o).unwrap();
    let mut details = vec![];
    for (suffix, plain, ty) in [
        ("_h", &original, 65537),
        ("", &standard, 131073),
        ("_t", &thumbnail, 262145),
    ] {
        let encoded = encode_dat_v2(plain, keys.image_dat.as_ref().unwrap(), None).unwrap();
        let relative = format!("msg/attach/{directory}/2026-10/Img/{base}{suffix}.dat");
        publish_cache_file(
            &location,
            &relative,
            &encoded,
            &hex::encode(Sha256::digest(&encoded)),
            &o,
        )
        .unwrap();
        let mut detail = RawFields::new();
        for (k, n) in [
            ("type", ty),
            ("size", encoded.len() as i64),
            ("create_time", 100),
            ("access_time", 0),
            ("status", 1),
        ] {
            detail.insert(k.into(), SqlValue::Integer(n.to_string()));
        }
        detail.insert("data_index".into(), SqlValue::Text("0".into()));
        detail.insert("packed_info".into(), SqlValue::Blob(STANDARD.encode([])));
        details.push(detail);
        if suffix != "_t" {
            let mut h = hardlink(plain);
            h.kind = HardlinkKind::Image;
            h.first_directory = directory.clone();
            h.second_directory = Some("2026-10".into());
            h.fields
                .insert("type".into(), SqlValue::Integer("2".into()));
            h.fields.insert(
                "file_name".into(),
                SqlValue::Text(format!("{base}{suffix}.dat")),
            );
            apply_hardlink_projection(&location, &keys, &h, &o).unwrap();
        }
    }
    let mut packed = vec![18, 34, 10, 32];
    packed.extend(base.as_bytes());
    apply_media_resource_projection(
        &location,
        &keys,
        &MediaResourceProjection {
            source: applied.source_id.clone(),
            chat: "peer".into(),
            packed_info: packed,
            details,
        },
        &o,
    )
    .unwrap();
    let r = reader(location.clone(), keys.clone());
    for (quality, verified, expected) in [
        (
            ImageQuality::Original,
            VerifiedImageQuality::Original,
            &original,
        ),
        (
            ImageQuality::Standard,
            VerifiedImageQuality::Standard,
            &standard,
        ),
        (
            ImageQuality::Thumbnail,
            VerifiedImageQuality::Thumbnail,
            &thumbnail,
        ),
    ] {
        let found = r.cached_image(&applied.source_id, quality).unwrap();
        assert_eq!(found.quality, verified);
        assert_eq!(&found.media.bytes, expected);
    }
    drop(r);
    Connection::open(
        location
            .root()
            .join("db_storage/message/message_resource.db"),
    )
    .unwrap()
    .execute(
        "UPDATE MessageResourceDetail SET size=size+1 WHERE type=131073",
        [],
    )
    .unwrap();
    let r = reader(location, keys);
    assert!(matches!(
        r.cached_image(&applied.source_id, ImageQuality::Standard),
        Err(ProfileError::NotCached)
    ));
    assert!(r
        .cached_image(&applied.source_id, ImageQuality::Original)
        .is_ok());
}
#[test]
fn native_md5_hash_matches_public_synthetic_vectors() {
    assert_eq!(
        hardlink_md5_hash("00000000000000000000000000000000").unwrap(),
        924413047
    );
    assert_eq!(
        hardlink_md5_hash("0123456789abcdef0123456789abcdef").unwrap(),
        2977716756
    );
    assert!(hardlink_md5_hash("short").is_err());
}
#[test]
fn jpeg_trailers_survive_official_image_writeback_and_readback() {
    let pixels = image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 37 + y * 19) as u8, (x * 13) as u8, (y * 31) as u8])
    });
    let mut output = Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(pixels)
        .write_to(&mut output, image::ImageFormat::Jpeg)
        .unwrap();
    let mut bytes = output.into_inner();
    let checksum = Md5::digest(&bytes);
    bytes.extend_from_slice(&0x12345678u64.to_le_bytes());
    bytes.extend_from_slice(&checksum);
    assert_exact_image_cache(&bytes);
}

#[test]
fn wxgf_writeback_and_readback_preserve_original_container_in_every_quality() {
    assert_exact_image_cache(include_bytes!(
        "../../wechat-media/tests/fixtures/static.wxgf"
    ));
}

#[test]
fn animated_webp_with_appended_source_data_survives_official_image_cache() {
    let mut bytes = include_bytes!("../../wechat-media/tests/fixtures/two-frame.webp").to_vec();
    bytes.extend_from_slice(&[0x42; 1200]);
    assert_exact_image_cache(&bytes);
}

fn assert_exact_image_cache(bytes: &[u8]) {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let mut keys = KeyFile::empty("account");
    keys.image_dat = Some(ImageDatKey::new([7; 16], 19));
    let o = options();
    let mut message = row(3);
    message.fields.insert(
        "message_content".into(),
        SqlValue::Text(format!(
            "<msg><img md5=\"{:x}\"/></msg>",
            Md5::digest(bytes)
        )),
    );
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    for quality in [
        ImageQuality::Original,
        ImageQuality::Standard,
        ImageQuality::Thumbnail,
    ] {
        let applied =
            cache_image_projection(&location, &keys, &source, "peer", quality, bytes, &o).unwrap();
        let encoded = fs::read(d.path().join("account").join(&applied.relative_path)).unwrap();
        let (decoded, layout) = decode_dat_v2(&encoded, keys.image_dat.as_ref().unwrap()).unwrap();
        assert_eq!(decoded, bytes);
        assert_eq!(
            encode_dat_v2(&decoded, keys.image_dat.as_ref().unwrap(), Some(layout)).unwrap(),
            encoded
        );
        assert_eq!(
            reader(location.clone(), keys.clone())
                .cached_image(&source, quality)
                .unwrap()
                .media
                .bytes,
            bytes
        );
        assert!(
            !cache_image_projection(&location, &keys, &source, "peer", quality, bytes, &o)
                .unwrap()
                .changed
        );
    }
}

#[test]
fn generic_image_variants_and_hardlink_bytes_roundtrip_without_fixture_metadata() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let mut keys = KeyFile::empty("account");
    keys.image_dat = Some(ImageDatKey::new([7; 16], 19));
    let o = options();
    fn png(w: u32, extra: u8) -> Vec<u8> {
        let mut out = Cursor::new(vec![]);
        image::DynamicImage::new_rgb8(w, w)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        let mut b = out.into_inner();
        b.resize(1200 + extra as usize, extra);
        b
    }
    let original = png(32, 1);
    let standard = png(16, 2);
    let thumbnail = png(8, 3);
    let mut message = row(3);
    message.fields.insert(
        "message_content".into(),
        SqlValue::Text(format!(
            "<msg><img md5=\"{:x}\"/></msg>",
            Md5::digest(&original)
        )),
    );
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    for (quality, bytes) in [
        (ImageQuality::Thumbnail, &thumbnail),
        (ImageQuality::Original, &original),
        (ImageQuality::Standard, &standard),
    ] {
        assert!(
            cache_image_projection(&location, &keys, &source, "peer", quality, bytes, &o)
                .unwrap()
                .changed
        );
        assert!(
            !cache_image_projection(&location, &keys, &source, "peer", quality, bytes, &o)
                .unwrap()
                .changed
        );
        assert_eq!(
            reader(location.clone(), keys.clone())
                .cached_image(&source, quality)
                .unwrap()
                .media
                .bytes,
            *bytes
        );
    }
    assert!(cache_image_projection(
        &location,
        &keys,
        &source,
        "peer",
        ImageQuality::Original,
        &standard,
        &o
    )
    .is_err());
    let mut message = row(49);
    message.server_message_id = "79".into();
    message
        .fields
        .insert("sort_seq".into(), SqlValue::Integer("101000".into()));
    message
        .fields
        .insert("create_time".into(), SqlValue::Integer("101".into()));
    message.fields.insert(
        "local_type".into(),
        SqlValue::Integer(((6u64 << 32) | 49).to_string()),
    );
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    let bytes = b"Synthetic ordinary attachment\n";
    assert!(
        cache_hardlink_media_projection(
            &location,
            &keys,
            &source,
            "peer",
            HardlinkKind::File,
            "sample.txt",
            bytes,
            &o
        )
        .unwrap()
        .changed
    );
    assert!(
        !cache_hardlink_media_projection(
            &location,
            &keys,
            &source,
            "peer",
            HardlinkKind::File,
            "sample.txt",
            bytes,
            &o
        )
        .unwrap()
        .changed
    );
    assert_eq!(
        reader(location, keys)
            .cached_hardlink_file(HardlinkKind::File, &format!("{:x}", Md5::digest(bytes)))
            .unwrap()
            .bytes,
        bytes
    );
}

#[test]
fn image_dat_identity_derivation_uses_hex_ascii_and_bit_shift_xor() {
    let bytes = vec![42; 1200];
    for (uin, aes, xor) in [
        (0, *b"39596593354ebe66", 0),
        (7, *b"1580fc73f5b64d25", 7),
        (256, *b"12486d7fb49572d0", 128),
        (512, *b"6c1962b6cbed93fb", 128),
        (1024, *b"af7a54baad62d3ac", 128),
        (2048, *b"5f5c6fc5b160caa1", 0),
        (u32::MAX, *b"705b42d935a58cea", 255),
    ] {
        let derived = ImageDatKey::derive(uin, "synthetic_self").unwrap();
        assert_eq!(
            encode_dat_v2(&bytes, &derived, None).unwrap(),
            encode_dat_v2(&bytes, &ImageDatKey::new(aes, xor), None).unwrap()
        );
    }
    assert!(ImageDatKey::derive(7, "").is_err());
}
#[test]
fn managed_layout_marker_upgrade_is_explicit_known_empty_and_idempotent() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    assert!(ensure_managed_layout_version(&location, &keys, &o).unwrap());
    assert!(!ensure_managed_layout_version(&location, &keys, &o).unwrap());
    Connection::open(d.path().join("account/db_storage/hardlink/hardlink.db"))
        .unwrap()
        .execute(
            "UPDATE db_info SET ValueInt64=3 WHERE Key='table_version'",
            [],
        )
        .unwrap();
    assert!(matches!(
        ensure_managed_layout_version(&location, &keys, &o),
        Err(ProfileError::InvalidMetadata)
    ));
}

fn attachment_row(title: &str, extension: &str, bytes: &[u8]) -> MessageProjection {
    let mut message = row((6i64 << 32) | 49);
    message.fields.insert("message_content".into(), SqlValue::Text(format!(
        "<msg><appmsg><title>{title}</title><type>6</type><appattach><totallen>{}</totallen><fileext>{extension}</fileext></appattach><md5>{:x}</md5></appmsg></msg>", bytes.len(), Md5::digest(bytes))));
    message
}
fn resource_db(d: &tempfile::TempDir) -> Connection {
    Connection::open(
        d.path()
            .join("account/db_storage/message/message_resource.db"),
    )
    .unwrap()
}
#[test]
fn file_resources_bind_original_and_local_names_and_replay_without_duplication() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let bytes = b"synthetic ordinary file payload\n";
    let source = apply_message_projection(
        &location,
        &keys,
        &attachment_row("a&amp;b.PDF", "PDF", bytes),
        &o,
    )
    .unwrap()
    .source_id;
    let first = cache_file_projection(&location, &keys, &source, "peer", bytes, &o).unwrap();
    assert!(first.changed);
    assert!(first.relative_path.starts_with("msg/file/1970-01/"));
    assert!(first.relative_path.ends_with("_a&b.PDF"));
    let db = resource_db(&d);
    let info: (i64,i64,String,i64,i64) = db.query_row("SELECT i.message_local_type,i.message_svr_id,n.user_name,i.message_local_id,i.message_origin_source FROM MessageResourceInfo i JOIN ChatName2Id n ON n.rowid=i.chat_id",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(
        info,
        (
            (6i64 << 32) | 49,
            -1,
            "peer".into(),
            source.local_id.parse().unwrap(),
            2
        )
    );
    let detail: (i64, i64, i64, String, Vec<u8>) = db
        .query_row(
            "SELECT type,size,status,data_index,packed_info FROM MessageResourceDetail",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        (detail.0, detail.1, detail.2, detail.3.as_str()),
        (0x330003, bytes.len() as i64, 1, "0")
    );
    let name = first.relative_path.rsplit('/').next().unwrap();
    let mut expected = vec![10, (2 + 7 + 2 + name.len()) as u8, 10, 7];
    expected.extend(b"a&b.PDF");
    expected.extend([18, name.len() as u8]);
    expected.extend(name.as_bytes());
    assert_eq!(detail.4, expected);
    assert!(
        !cache_file_projection(&location, &keys, &source, "peer", bytes, &o)
            .unwrap()
            .changed
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM MessageResourceDetail", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.execute("UPDATE MessageResourceDetail SET access_time=999", [])
        .unwrap();
    let hardlink_db =
        Connection::open(d.path().join("account/db_storage/hardlink/hardlink.db")).unwrap();
    hardlink_db
        .execute("UPDATE file_hardlink_info_v4 SET modify_time=777", [])
        .unwrap();
    assert!(
        !cache_file_projection(&location, &keys, &source, "peer", bytes, &o)
            .unwrap()
            .changed
    );
    assert_eq!(
        db.query_row("SELECT access_time FROM MessageResourceDetail", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        999
    );
    assert_eq!(
        hardlink_db
            .query_row("SELECT modify_time FROM file_hardlink_info_v4", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        777
    );
    assert!(cache_file_projection(&location, &keys, &source, "peer", b"other", &o).is_err());
    assert_eq!(
        reader(location, keys)
            .cached_hardlink_file(HardlinkKind::File, &format!("{:x}", Md5::digest(bytes)))
            .unwrap()
            .bytes,
        bytes
    );
}
#[test]
fn pending_file_resource_adds_only_local_name_and_preserves_access_time() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let bytes = b"synthetic";
    let source =
        apply_message_projection(&location, &keys, &attachment_row("a.txt", "txt", bytes), &o)
            .unwrap()
            .source_id;
    let mut detail = RawFields::new();
    for (key, n) in [
        ("type", 0x390003),
        ("size", bytes.len() as i64),
        ("create_time", 100),
        ("access_time", 123),
        ("status", 0),
    ] {
        detail.insert(key.into(), SqlValue::Integer(n.to_string()));
    }
    detail.insert("data_index".into(), SqlValue::Text("0".into()));
    detail.insert(
        "packed_info".into(),
        SqlValue::Blob(STANDARD.encode([10, 7, 10, 5, b'a', b'.', b't', b'x', b't'])),
    );
    let applied = apply_media_resource_projection(
        &location,
        &keys,
        &MediaResourceProjection {
            source: source.clone(),
            chat: "peer".into(),
            packed_info: vec![],
            details: vec![detail],
        },
        &o,
    )
    .unwrap();
    assert!(
        cache_file_projection(&location, &keys, &source, "peer", bytes, &o)
            .unwrap()
            .changed
    );
    let db = resource_db(&d);
    assert_eq!(
        db.query_row(
            "SELECT resource_id,status,size,access_time FROM MessageResourceDetail",
            [],
            |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?
            ))
        )
        .unwrap(),
        (applied.detail_ids[0], 1, bytes.len() as i64, 123)
    );
    assert!(
        !cache_file_projection(&location, &keys, &source, "peer", bytes, &o)
            .unwrap()
            .changed
    );
}
fn movie_container_fixture(brand: Option<&[u8; 4]>, media: &[u8]) -> Vec<u8> {
    // Container-shape fixtures; core encoder tests cover playable source streams.
    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }
    let mut bytes = Vec::new();
    if let Some(brand) = brand {
        bytes.extend(atom(b"ftyp", &[brand.as_slice(), &[0; 4]].concat()));
    }
    bytes.extend(atom(b"moov", &[0; 8]));
    bytes.extend(atom(b"mdat", media));
    bytes
}
#[test]
fn video_resource_variants_share_basename_and_preserve_other_variants() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let normal = movie_container_fixture(Some(b"isom"), b"normal");
    let original = movie_container_fixture(None, b"legacy original");
    let mut jpeg = Cursor::new(vec![]);
    image::DynamicImage::new_rgb8(3, 2)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();
    let mut message = row(43);
    message.fields.insert("message_content".into(),SqlValue::Text(format!("<msg><videomsg length=\"{}\" md5=\"{:x}\" rawlength=\"{}\" rawmd5=\"{:x}\" cdnthumblength=\"{}\"/></msg>",normal.len(),Md5::digest(&normal),original.len(),Md5::digest(&original),jpeg.len())));
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    let a = cache_video_projection(
        &location,
        &keys,
        &source,
        "peer",
        VideoQuality::Normal,
        &normal,
        &o,
    )
    .unwrap();
    let b = cache_video_projection(
        &location,
        &keys,
        &source,
        "peer",
        VideoQuality::Original,
        &original,
        &o,
    )
    .unwrap();
    let c = cache_video_projection(
        &location,
        &keys,
        &source,
        "peer",
        VideoQuality::Thumbnail,
        &jpeg,
        &o,
    )
    .unwrap();
    assert_eq!(
        a.relative_path.strip_suffix(".mp4"),
        b.relative_path.strip_suffix("_raw.mp4")
    );
    assert_eq!(
        a.relative_path.strip_suffix(".mp4"),
        c.relative_path.strip_suffix(".jpg")
    );
    assert!(
        !cache_video_projection(
            &location,
            &keys,
            &source,
            "peer",
            VideoQuality::Normal,
            &normal,
            &o
        )
        .unwrap()
        .changed
    );
    assert!(cache_video_projection(
        &location,
        &keys,
        &source,
        "peer",
        VideoQuality::Original,
        &normal,
        &o
    )
    .is_err());
    assert!(cache_video_projection(
        &location,
        &keys,
        &source,
        "peer",
        VideoQuality::Thumbnail,
        &normal,
        &o
    )
    .is_err());
    let db = resource_db(&d);
    let types = db
        .prepare("SELECT type,size,status FROM MessageResourceDetail ORDER BY type")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        types,
        vec![
            (65538, original.len() as i64, 1),
            (131074, normal.len() as i64, 1),
            (196610, jpeg.len() as i64, 1)
        ]
    );
    let hardlinks =
        Connection::open(d.path().join("account/db_storage/hardlink/hardlink.db")).unwrap();
    assert_eq!(hardlinks.query_row("SELECT count(*) FROM video_hardlink_info_v4 WHERE type=3 AND dir2=0 AND length(extra_buffer)=0",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    let reader = reader(location, keys);
    for bytes in [normal.as_slice(), original.as_slice(), jpeg.as_slice()] {
        assert_eq!(
            reader
                .cached_hardlink_file(HardlinkKind::Video, &format!("{:x}", Md5::digest(bytes)))
                .unwrap()
                .bytes,
            bytes
        );
    }
    assert_eq!(
        reader
            .cached_hardlink_file(
                HardlinkKind::Video,
                &format!("{:x}", Md5::digest(&original))
            )
            .unwrap()
            .media_type,
        "video/quicktime"
    );
}
#[test]
fn legacy_quicktime_hardlink_cache_preserves_bytes_and_reports_its_container_type() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let source = apply_message_projection(&location, &keys, &row(43), &o)
        .unwrap()
        .source_id;
    let original = movie_container_fixture(None, b"legacy original");
    cache_hardlink_media_projection(
        &location,
        &keys,
        &source,
        "peer",
        HardlinkKind::Video,
        "legacy.mov",
        &original,
        &o,
    )
    .unwrap();
    assert!(cache_hardlink_media_projection(
        &location,
        &keys,
        &source,
        "peer",
        HardlinkKind::Video,
        "wrong.mov",
        b"RIFFxxxxAVI ",
        &o,
    )
    .is_err());
    let cached = reader(location, keys)
        .cached_hardlink_file(
            HardlinkKind::Video,
            &format!("{:x}", Md5::digest(&original)),
        )
        .unwrap();
    assert_eq!(cached.bytes, original);
    assert_eq!(cached.media_type, "video/quicktime");
}
#[test]
fn malformed_attachment_metadata_does_not_create_cache_or_resource_rows() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let mut message = attachment_row("../bad.txt", "txt", b"synthetic");
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    assert!(cache_file_projection(&location, &keys, &source, "peer", b"synthetic", &o).is_err());
    assert!(!d.path().join("account/msg").exists());
    message.server_message_id = "100".into();
    message
        .fields
        .insert("sort_seq".into(), SqlValue::Integer("101000".into()));
    message.fields.insert(
        "message_content".into(),
        SqlValue::Text(
            "<!DOCTYPE msg [<!ENTITY x 'bad'>]><msg><appmsg><title>&x;</title></appmsg></msg>"
                .into(),
        ),
    );
    let source = apply_message_projection(&location, &keys, &message, &o)
        .unwrap()
        .source_id;
    assert!(cache_file_projection(&location, &keys, &source, "peer", b"synthetic", &o).is_err());
    assert_eq!(
        resource_db(&d)
            .query_row("SELECT count(*) FROM MessageResourceInfo", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn file_resource_categories_match_supported_registry_and_generic_fallback() {
    for (extension, expected) in [
        ("ZIP", 0x10003),
        ("wav", 0xb0003),
        ("dmg", 0x150003),
        ("pkg", 0x160003),
        ("exe", 0x170003),
        ("bat", 0x180003),
        ("CSV", 0x1f0003),
        ("numbers", 0x200003),
        ("ods", 0x210003),
        ("ppsx", 0x290003),
        ("key", 0x2a0003),
        ("odp", 0x2b0003),
        ("PDF", 0x330003),
        ("docm", 0x340003),
        ("pages", 0x350003),
        ("wps", 0x360003),
        ("odt", 0x370003),
        ("rtf", 0x380003),
        ("txt", 0x390003),
        ("cpp", 0x3a0003),
        ("mp4", 3),
        ("", 3),
        ("unknown", 3),
    ] {
        assert_eq!(file_resource_type(extension), expected);
    }
}

#[test]
fn exact_completed_resource_aliases_are_replays_and_divergent_aliases_stay_blocked() {
    let d = fixture();
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let bytes = b"synthetic completed file";
    let source = apply_message_projection(
        &location,
        &keys,
        &attachment_row("report.pdf", "pdf", bytes),
        &o,
    )
    .unwrap()
    .source_id;
    cache_file_projection(&location, &keys, &source, "peer", bytes, &o).unwrap();
    let db = resource_db(&d);
    db.execute_batch("INSERT INTO MessageResourceInfo(chat_id,sender_id,message_local_type,message_create_time,message_local_id,message_svr_id,message_origin_source,packed_info) SELECT chat_id,sender_id,message_local_type,message_create_time,message_local_id,message_svr_id,message_origin_source,packed_info FROM MessageResourceInfo LIMIT 1").unwrap();
    let alias = db.last_insert_rowid();
    db.execute("INSERT INTO MessageResourceDetail(message_id,type,size,create_time,access_time,status,data_index,packed_info) SELECT ?1,type,size,create_time,access_time,status,data_index,packed_info FROM MessageResourceDetail LIMIT 1",[alias]).unwrap();
    let ids = db
        .prepare("SELECT resource_id FROM MessageResourceDetail ORDER BY resource_id")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        !cache_file_projection(&location, &keys, &source, "peer", bytes, &o)
            .unwrap()
            .changed
    );
    db.execute(
        "UPDATE MessageResourceDetail SET status=0 WHERE message_id=?1",
        [alias],
    )
    .unwrap();
    assert!(cache_file_projection(&location, &keys, &source, "peer", bytes, &o).is_err());
    db.execute(
        "UPDATE MessageResourceDetail SET status=1,access_time=1 WHERE message_id=?1",
        [alias],
    )
    .unwrap();
    assert!(cache_file_projection(&location, &keys, &source, "peer", bytes, &o).is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM MessageResourceInfo", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.prepare("SELECT resource_id FROM MessageResourceDetail ORDER BY resource_id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap(),
        ids
    );
}
