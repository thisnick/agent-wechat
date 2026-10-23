use super::*;

const MAX_UPLOAD_TOKEN_BYTES: usize = 65536;
const BLOCKS_PER_GROUP: usize = 128;
const MAX_BLOCKS: usize = (MAX_LARGE_FILE_BYTES + 16).div_ceil(512 * 1024);

// The reference sends the current 128-block group and at most its two preceding
// groups. This uploader is sequential: only blocks before `next_block` have a
// verified acknowledgement. Offsets and alreadyupsize remain file-relative.
fn acknowledged_bitmap(next_block: usize) -> Vec<u8> {
    let group = next_block / BLOCKS_PER_GROUP;
    let first_group = group.saturating_sub(2);
    let mut bitmap = vec![0; (group - first_group + 1) * 16];
    for bit in 0..next_block - first_group * BLOCKS_PER_GROUP {
        bitmap[bit / 8] |= 1 << (bit % 8);
    }
    bitmap
}

impl CdnClient {
    /// Upload one ordinary file using previously persisted source/key material.
    /// Object readback and business-message submission remain separate operations.
    pub fn upload_large_file_with_progress(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        ensure!(
            matches!(prepared.kind, MediaQuality::File | MediaQuality::LargeFile),
            "Expected an ordinary file upload"
        );
        upload_large_file(
            &self.config,
            prepared,
            task_id,
            |path, fields| self.post(path, fields),
            progress,
        )
    }

    /// Upload the unmodified original as a separate type-4 object. The normal
    /// rendition and thumbnail use their own paired upload and final metadata.
    /// Persist the task ID and prepared material before calling this method.
    pub fn upload_video_original_with_progress(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<UploadedMedia> {
        upload_video_original(
            self,
            prepared,
            task_id,
            |path, fields| self.post(path, fields),
            progress,
        )
    }
}

fn upload_video_original(
    client: &CdnClient,
    prepared: &PreparedUpload,
    task_id: u64,
    post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    ensure!(
        prepared.kind == MediaQuality::Video && task_id != 0,
        "Expected an original video upload and task ID"
    );
    prepared.validate()?;
    if prepared.encrypted.len() < MAX_CDN_TRANSFER_BYTES {
        client.upload_with_transport(prepared, progress, post)
    } else {
        upload_large_file(&client.config, prepared, task_id, post, progress)
    }
}

fn common_fields(
    conf: &CdnConfig,
    prepared: &PreparedUpload,
    task_id: u64,
    sequence: u32,
) -> CdnFields {
    let mut fields = CdnFields::new();
    for (key, value) in [
        ("ver", conf.version as u64),
        ("seq", sequence as u64),
        ("weixinnum", conf.uin),
        ("nettype", 1),
        ("ipseq", 0),
        ("retrycnt", 0),
        ("lastretcode", 0),
        ("waitsec", 0),
        ("clientversion", conf.client_version as u64),
        ("taskid", task_id),
        ("from", 0),
        ("bizid", 1),
        ("appid", 0),
        ("filetype", u64::from(prepared.kind.filetype())),
        ("optype", 0),
        ("expectbizid", 0),
        ("expectappid", 0),
        ("expectfiletype", u64::from(prepared.kind.filetype())),
        ("expectoptype", 0),
        ("frontid", conf.front_id as u64),
    ] {
        put(&mut fields, key, value);
    }
    fields.insert("authkey".into(), conf.auth_key.clone());
    fields.insert(
        "clientostype".into(),
        conf.client_os_type.as_bytes().to_vec(),
    );
    fields.insert("filekey".into(), prepared.file_key.as_bytes().to_vec());
    if let Some(location) = conf.location {
        for (key, value) in [
            ("province", location.province),
            ("city", location.city),
            ("isp", location.isp),
        ] {
            put(&mut fields, key, value);
        }
    }
    fields
}

fn validate_reply(reply: &CdnFields, sequence: u32, version: u32) -> Result<()> {
    ensure!(
        integer(reply, "seq")? == i64::from(sequence),
        "Large-file reply sequence mismatch"
    );
    let code = integer(reply, "retcode")?;
    ensure!(code == 0, "Large-file upload returned {code}");
    for key in ["isretry", "retrysec", "isoverload", "isgetcdn"] {
        ensure!(
            integer(reply, key)? == 0,
            "Unsupported large-file upload retry or routing control"
        );
    }
    ensure!(
        integer(reply, "ver")? == i64::from(version),
        "Large-file reply version mismatch"
    );
    Ok(())
}

fn opaque_field(reply: &CdnFields, key: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let value = reply
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("Missing large-file {key}"))?;
    ensure!(
        !value.is_empty() && value.len() <= max_bytes,
        "Invalid large-file {key} length"
    );
    Ok(value.clone())
}

fn upload_large_file(
    conf: &CdnConfig,
    prepared: &PreparedUpload,
    task_id: u64,
    mut post: impl FnMut(&str, &CdnFields) -> Result<CdnFields>,
    mut progress: impl FnMut(u64, Option<u64>) -> Result<()>,
) -> Result<UploadedMedia> {
    ensure!(
        ((prepared.kind == MediaQuality::File
            && prepared.plaintext_bytes as usize > MAX_NATIVE_UPLOAD_BYTES
            && prepared.plaintext_bytes as usize <= MAX_CDN_TRANSFER_BYTES)
            || (prepared.kind == MediaQuality::LargeFile
                && prepared.plaintext_bytes as usize > MAX_CDN_TRANSFER_BYTES
                && prepared.plaintext_bytes as usize <= MAX_LARGE_FILE_BYTES)
            || (prepared.kind == MediaQuality::Video
                && prepared.plaintext_bytes as usize > MAX_NATIVE_UPLOAD_BYTES
                && prepared.plaintext_bytes as usize <= wechat_media::MAX_VIDEO_SOURCE_BYTES))
            && prepared.encrypted.len() >= MAX_CDN_TRANSFER_BYTES
            && task_id != 0,
        "Unsupported large-file upload profile"
    );
    conf.validate()?;
    ensure!(
        conf.location.is_some(),
        "Missing authenticated CDN location"
    );
    prepared.validate()?;
    let total = prepared.encrypted.len() as u64;
    let mut sequence = 0u32;
    // No automatic retries: a failed request leaves the caller's persisted
    // source/key material intact and never submits a business message.
    let mut exchange =
        |phase: &str,
         extra: CdnFields,
         acknowledged: u64,
         progress: &mut dyn FnMut(u64, Option<u64>) -> Result<()>| {
            progress(acknowledged, Some(total))?;
            sequence += 1;
            let mut fields = common_fields(conf, prepared, task_id, sequence);
            fields.extend(extra);
            let path = format!(
                "/cdn/{}/{sequence}/{}/{phase}",
                conf.uin,
                prepared.kind.filetype()
            );
            let reply = post(&path, &fields)?;
            validate_reply(&reply, sequence, conf.version)?;
            Ok::<_, anyhow::Error>(reply)
        };

    let mut apply = CdnFields::new();
    for (key, value) in [
        ("apptype", 1),
        ("chatnum", 1),
        ("reqtype", 0),
        ("type", u64::from(prepared.kind.filetype())),
        ("prepare", 0),
        ("num", 1),
        ("type_0", u64::from(prepared.kind.filetype())),
        ("cpr_0", 0),
        ("size_0", total),
        ("rawsize_0", prepared.plaintext_bytes as u64),
        ("format_0", 0),
    ] {
        put(&mut apply, key, value);
    }
    apply.insert(
        "chatid".into(),
        encrypt_recipient(&prepared.destination)?.into_bytes(),
    );
    apply.insert("rawfilemd5_0".into(), prepared.md5.as_bytes().to_vec());
    if let Some((_, extension)) = prepared.file_name.rsplit_once('.') {
        if !extension.is_empty() {
            apply.insert("ext_0".into(), extension.as_bytes().to_vec());
        }
    }
    let reply = exchange("apply", apply, 0, &mut progress)?;
    ensure!(
        integer(&reply, "hit")? == 0,
        "Large-file cache hits require unsupported key recovery"
    );
    ensure!(
        integer(&reply, "type")? == i64::from(prepared.kind.filetype())
            && integer(&reply, "num")? == 1
            && integer(&reply, "type_0")? == i64::from(prepared.kind.filetype())
            && integer(&reply, "fin_0")? == 0,
        "Unsupported large-file apply result"
    );
    let block_size = usize::try_from(integer(&reply, "blocksize_0")?)?;
    ensure!(
        [512 * 1024, 1024 * 1024].contains(&block_size)
            && prepared.encrypted.len().div_ceil(block_size) <= MAX_BLOCKS,
        "Unsupported large-file block size"
    );
    let upinfo = opaque_field(&reply, "upinfo_0", MAX_UPLOAD_TOKEN_BYTES)?;
    progress(0, Some(total))?;

    let mut acknowledged = 0u64;
    let mut etags = Vec::new();
    for (index, chunk) in prepared.encrypted.chunks(block_size).enumerate() {
        let mut fields = CdnFields::new();
        put(&mut fields, "alreadyupsize", acknowledged);
        fields.insert("alreadyupbit".into(), acknowledged_bitmap(index));
        fields.insert("upinfo".into(), upinfo.clone());
        put(&mut fields, "offset", acknowledged);
        fields.insert("blockmd5".into(), hex::encode(md5(chunk)).into_bytes());
        fields.insert("blockdata".into(), chunk.to_vec());
        let reply = exchange("uploadblock", fields, acknowledged, &mut progress)?;
        ensure!(
            u64::try_from(integer(&reply, "offset")?)? == acknowledged
                && integer(&reply, "fin")? == 0,
            "Large-file block acknowledgement mismatch"
        );
        etags.push(opaque_field(&reply, "etag", 8192)?);
        acknowledged += chunk.len() as u64;
        progress(acknowledged, Some(total))?;
    }
    ensure!(acknowledged == total, "Incomplete large-file upload");

    let mut finish = CdnFields::new();
    finish.insert("upinfo".into(), upinfo);
    put(&mut finish, "blocksum", etags.len());
    for (index, etag) in etags.into_iter().enumerate() {
        finish.insert(format!("etag_{index}"), etag);
    }
    let reply = exchange("fin", finish, acknowledged, &mut progress)?;
    let finished_upinfo = opaque_field(&reply, "upinfo", MAX_UPLOAD_TOKEN_BYTES)?;
    progress(acknowledged, Some(total))?;
    let mut complete = CdnFields::new();
    put(&mut complete, "type", prepared.kind.filetype());
    put(&mut complete, "num", 1);
    complete.insert("upinfo_0".into(), finished_upinfo);
    let reply = exchange("complete", complete, acknowledged, &mut progress)?;
    let file_id = String::from_utf8(opaque_field(&reply, "fileid", 8192)?)?;
    ensure!(
        !file_id.chars().any(|c| c.is_control()),
        "Invalid large-file identifier"
    );
    // Return the correlated completion even if cancellation arrived while this
    // request was in flight. The caller persists it before checking its fence.
    Ok(UploadedMedia {
        descriptor: MediaDescriptor {
            file_id,
            aes_key: prepared.aes_key.clone(),
            quality: prepared.kind,
            expected_bytes: Some(prepared.plaintext_bytes as u64),
            expected_md5: Some(prepared.md5.clone()),
            strict_size: true,
            expires_at: None,
        },
        plaintext_bytes: prepared.plaintext_bytes,
        md5: prepared.md5.clone(),
        sha256: prepared.sha256.clone(),
        file_name: prepared.file_name.clone(),
        width: None,
        height: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::OnceLock};

    #[test]
    fn acknowledgement_windows_cross_multiple_groups_without_wrapping() {
        assert_eq!(acknowledged_bitmap(0), vec![0; 16]);
        let mut before_boundary = vec![255; 16];
        before_boundary[15] = 127;
        assert_eq!(acknowledged_bitmap(127), before_boundary);
        assert_eq!(
            acknowledged_bitmap(128),
            [vec![255; 16], vec![0; 16]].concat()
        );
        assert_eq!(
            acknowledged_bitmap(256),
            [vec![255; 32], vec![0; 16]].concat()
        );
        for index in [384, 512, 1024] {
            assert_eq!(
                acknowledged_bitmap(index),
                [vec![255; 32], vec![0; 16]].concat()
            );
        }
        let mut after_boundary = [vec![255; 32], vec![0; 16]].concat();
        after_boundary[32] = 1;
        assert_eq!(acknowledged_bitmap(513), after_boundary);
        assert_eq!(MAX_BLOCKS, 1025);
    }

    #[test]
    fn final_request_serializes_all_maximum_file_etags_in_order() {
        let mut fields = CdnFields::new();
        put(&mut fields, "blocksum", MAX_BLOCKS);
        fields.insert("upinfo".into(), b"synthetic-finish-context".to_vec());
        for index in 0..MAX_BLOCKS {
            fields.insert(
                format!("etag_{index}"),
                format!("synthetic-{index}").into_bytes(),
            );
        }
        let wire = pack_cdn(&fields).unwrap();
        let mut cursor = crate::mmtls::Cursor::new(&wire);
        let mut decoded = CdnFields::new();
        for _ in 0..MAX_BLOCKS + 2 {
            let key = std::str::from_utf8(cursor.sized(4).unwrap())
                .unwrap()
                .to_owned();
            decoded.insert(key, cursor.sized(4).unwrap().to_vec());
        }
        cursor.end().unwrap();
        assert_eq!(integer(&decoded, "blocksum").unwrap(), 1025);
        for index in 0..MAX_BLOCKS {
            assert_eq!(
                decoded[&format!("etag_{index}")],
                format!("synthetic-{index}").as_bytes()
            );
        }
        fields.extend((MAX_BLOCKS..2048).map(|i| (format!("etag_{i}"), vec![1])));
        assert!(pack_cdn(&fields).is_err());
    }

    fn config() -> CdnConfig {
        CdnConfig {
            version: 1,
            uin: 12345,
            client_version: 456,
            client_os_type: "test-client".into(),
            auth_key: b"test-credential".to_vec(),
            server_ips: vec![Ipv4Addr::new(1, 1, 1, 1)],
            front_id: 789,
            location: Some(CdnLocation {
                province: 1234,
                city: 56,
                isp: 78,
            }),
        }
    }

    fn prepared() -> &'static PreparedUpload {
        static VALUE: OnceLock<PreparedUpload> = OnceLock::new();
        VALUE.get_or_init(|| {
            PreparedUpload::with_material(
                &vec![0x5a; MAX_CDN_TRANSFER_BYTES + 17],
                MediaQuality::LargeFile,
                "boundary.fixture.bin",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap()
        })
    }

    // This peer accepts only the captured four-phase wire contract. It checks
    // offsets, encrypted block hashes, prior acknowledgements and both tokens.
    struct Peer<'a> {
        prepared: &'a PreparedUpload,
        block_size: usize,
        requests: usize,
    }

    impl<'a> Peer<'a> {
        fn new(prepared: &'a PreparedUpload, block_size: usize) -> Self {
            Self {
                prepared,
                block_size,
                requests: 0,
            }
        }

        fn blocks(&self) -> usize {
            self.prepared.encrypted.len().div_ceil(self.block_size)
        }

        fn respond(&mut self, path: &str, fields: &CdnFields) -> Result<CdnFields> {
            self.requests += 1;
            let sequence = self.requests;
            let kind = self.prepared.kind.filetype();
            let phase = match sequence {
                1 => "apply",
                n if n <= self.blocks() + 1 => "uploadblock",
                n if n == self.blocks() + 2 => "fin",
                n if n == self.blocks() + 3 => "complete",
                _ => panic!("Request after completion"),
            };
            assert_eq!(path, format!("/cdn/12345/{sequence}/{kind}/{phase}"));
            assert_eq!(integer(fields, "seq")?, sequence as i64);
            assert_eq!(integer(fields, "taskid")?, 987654321);
            assert_eq!(integer(fields, "filetype")?, i64::from(kind));
            assert_eq!(integer(fields, "expectfiletype")?, i64::from(kind));
            assert_eq!(integer(fields, "frontid")?, 789);
            for key in ["ipseq", "retrycnt", "lastretcode", "waitsec"] {
                assert_eq!(integer(fields, key)?, 0);
            }
            assert_eq!(integer(fields, "province")?, 1234);
            assert_eq!(integer(fields, "city")?, 56);
            assert_eq!(integer(fields, "isp")?, 78);
            assert_eq!(fields["authkey"], b"test-credential");
            assert_eq!(fields["filekey"], self.prepared.file_key.as_bytes());
            assert!(!fields.contains_key("aeskey"));
            assert!(!fields.contains_key("signature"));
            let mut reply = CdnFields::new();
            for (key, value) in [
                ("ver", 1),
                ("seq", sequence),
                ("retcode", 0),
                ("isretry", 0),
                ("retrysec", 0),
                ("isoverload", 0),
                ("isgetcdn", 0),
            ] {
                put(&mut reply, key, value);
            }
            match phase {
                "apply" => {
                    assert_eq!(integer(fields, "type")?, i64::from(kind));
                    assert_eq!(integer(fields, "type_0")?, i64::from(kind));
                    assert_eq!(integer(fields, "num")?, 1);
                    assert_eq!(integer(fields, "apptype")?, 1);
                    assert_eq!(integer(fields, "chatnum")?, 1);
                    assert_eq!(
                        integer(fields, "size_0")?,
                        self.prepared.encrypted.len() as i64
                    );
                    assert_eq!(
                        integer(fields, "rawsize_0")?,
                        i64::from(self.prepared.plaintext_bytes)
                    );
                    assert_eq!(fields["rawfilemd5_0"], self.prepared.md5.as_bytes());
                    assert_eq!(
                        fields["ext_0"],
                        self.prepared
                            .file_name
                            .rsplit('.')
                            .next()
                            .unwrap()
                            .as_bytes()
                    );
                    assert_eq!(
                        fields["chatid"],
                        encrypt_recipient("filehelper")?.as_bytes()
                    );
                    for (key, value) in [
                        ("hit", 0),
                        ("type", kind as usize),
                        ("num", 1),
                        ("type_0", kind as usize),
                        ("fin_0", 0),
                        ("blocksize_0", self.block_size),
                    ] {
                        put(&mut reply, key, value);
                    }
                    reply.insert("upinfo_0".into(), b"apply-token".to_vec());
                }
                "uploadblock" => {
                    let index = sequence - 2;
                    let offset = index * self.block_size;
                    let end = (offset + self.block_size).min(self.prepared.encrypted.len());
                    let expected = &self.prepared.encrypted[offset..end];
                    assert_eq!(integer(fields, "offset")?, offset as i64);
                    assert_eq!(integer(fields, "alreadyupsize")?, offset as i64);
                    assert_eq!(fields["upinfo"], b"apply-token");
                    assert_eq!(fields["blockdata"], expected);
                    assert_eq!(fields["blockmd5"], hex::encode(md5(expected)).as_bytes());
                    let bitmap: Vec<u8> = (0..128)
                        .collect::<Vec<_>>()
                        .chunks(8)
                        .map(|bits| {
                            bits.iter().fold(0, |value, bit| {
                                value | (u8::from(*bit < index) << (bit % 8))
                            })
                        })
                        .collect();
                    assert_eq!(fields["alreadyupbit"], bitmap);
                    put(&mut reply, "offset", offset);
                    put(&mut reply, "fin", 0);
                    reply.insert("etag".into(), format!("etag-{index}").into_bytes());
                }
                "fin" => {
                    assert_eq!(fields["upinfo"], b"apply-token");
                    assert_eq!(integer(fields, "blocksum")?, self.blocks() as i64);
                    for index in 0..self.blocks() {
                        assert_eq!(
                            fields[&format!("etag_{index}")],
                            format!("etag-{index}").as_bytes()
                        );
                    }
                    reply.insert("upinfo".into(), b"finish-token".to_vec());
                }
                "complete" => {
                    assert_eq!(integer(fields, "type")?, i64::from(kind));
                    assert_eq!(integer(fields, "num")?, 1);
                    assert_eq!(fields["upinfo_0"], b"finish-token");
                    reply.insert("fileid".into(), b"completed-object".to_vec());
                }
                _ => unreachable!(),
            }
            Ok(reply)
        }
    }

    #[test]
    fn transfers_ordered_blocks_and_uses_finish_token() {
        let prepared = prepared();
        for block_size in [512 * 1024, 1024 * 1024] {
            let mut peer = Peer::new(prepared, block_size);
            let events = RefCell::new(Vec::new());
            let progress = RefCell::new(Vec::new());
            let result = upload_large_file(
                &config(),
                prepared,
                987654321,
                |path, fields| {
                    events.borrow_mut().push("request");
                    peer.respond(path, fields)
                },
                |done, total| {
                    events.borrow_mut().push("progress");
                    progress.borrow_mut().push(done);
                    assert_eq!(total, Some(prepared.encrypted.len() as u64));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(peer.requests, peer.blocks() + 3);
            let events = events.borrow();
            for event in events[..events.len() - 2].chunks(3) {
                assert_eq!(event, ["progress", "request", "progress"]);
            }
            assert_eq!(&events[events.len() - 2..], ["progress", "request"]);
            let positions = progress.borrow();
            assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(
                positions.last().copied(),
                Some(prepared.encrypted.len() as u64)
            );
            assert_eq!(result.descriptor.file_id, "completed-object");
            assert_eq!(result.descriptor.quality, MediaQuality::LargeFile);
            assert_eq!(
                result.descriptor.expected_bytes,
                Some(prepared.plaintext_bytes as u64)
            );
            assert_eq!(result.descriptor.aes_key, prepared.aes_key);
            assert_eq!(result.md5, prepared.md5);
            assert_eq!(result.sha256, prepared.sha256);
        }
    }

    #[test]
    fn standalone_original_video_uses_type_four_without_thumbnail() {
        let mut raw = prepared().clone();
        raw.kind = MediaQuality::Video;
        raw.file_name = "original.mp4".into();
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        let cancelled = std::cell::Cell::new(false);
        let mut peer = Peer::new(&raw, 1024 * 1024);
        let result = upload_video_original(
            &cdn,
            &raw,
            987654321,
            |path, fields| {
                assert!(!fields.contains_key("thumbdata"));
                assert!(!fields.contains_key("type_1"));
                let response = peer.respond(path, fields)?;
                if path.ends_with("/complete") {
                    cancelled.set(true);
                }
                Ok(response)
            },
            |_, _| {
                ensure!(!cancelled.get(), "cancelled");
                Ok(())
            },
        )
        .unwrap();
        assert!(cancelled.get());
        assert_eq!(result.descriptor.quality, MediaQuality::Video);
        assert_eq!(
            result.descriptor.expected_bytes,
            Some(u64::from(raw.plaintext_bytes))
        );
        assert_eq!(result.descriptor.aes_key, raw.aes_key);
        assert_eq!(result.md5, raw.md5);
        assert_eq!(result.sha256, raw.sha256);
        assert_eq!(peer.requests, peer.blocks() + 3);
        assert!(cdn
            .upload_large_file_with_progress(&raw, 987654321, |_, _| {
                panic!("Video admitted by ordinary-file interface")
            })
            .is_err());
    }

    #[test]
    fn small_original_video_uses_standalone_legacy_and_rejects_bad_material_locally() {
        let raw = PreparedUpload::with_material(
            b"\0\0\0\x0cftypisom",
            MediaQuality::Video,
            "original.mp4",
            "filehelper",
            &[0x31; 16],
            "00112233445566778899aabbccddeeff",
        )
        .unwrap();
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        let mut calls = 0;
        let result = upload_video_original(
            &cdn,
            &raw,
            987654321,
            |path, fields| {
                calls += 1;
                assert_eq!(path, "/uploadv3");
                assert_eq!(integer(fields, "filetype")?, 4);
                assert!(!fields.contains_key("thumbdata"));
                assert_eq!(integer(fields, "hasthumb")?, 0);
                let mut reply = CdnFields::new();
                put(&mut reply, "retcode", 0);
                for key in ["seq", "rangestart", "rangeend"] {
                    reply.insert(key.into(), fields[key].clone());
                }
                reply.insert("fileid".into(), b"small-original".to_vec());
                Ok(reply)
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.descriptor.quality, MediaQuality::Video);
        let mut over = raw.clone();
        over.plaintext_bytes = wechat_media::MAX_VIDEO_SOURCE_BYTES as u32 + 1;
        let mut wrong = raw.clone();
        wrong.kind = MediaQuality::File;
        let mut corrupt = raw.clone();
        corrupt.encrypted[0] ^= 1;
        for (value, task) in [
            (&over, 987654321),
            (&wrong, 987654321),
            (&corrupt, 987654321),
            (&raw, 0),
        ] {
            assert!(upload_video_original(
                &cdn,
                value,
                task,
                |_, _| { panic!("Invalid original video reached transport") },
                |_, _| Ok(())
            )
            .is_err());
        }
    }

    #[test]
    fn original_video_padding_boundary_selects_one_carrier_without_fallback() {
        let cdn = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
        for (size, expected_path) in [
            (MAX_NATIVE_UPLOAD_BYTES, "/uploadv3"),
            (MAX_NATIVE_UPLOAD_BYTES + 1, "/cdn/12345/1/4/apply"),
        ] {
            let raw = PreparedUpload::with_material(
                &vec![0x5a; size],
                MediaQuality::Video,
                "original.mp4",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap();
            let mut calls = 0;
            let error = upload_video_original(
                &cdn,
                &raw,
                987654321,
                |path, fields| {
                    calls += 1;
                    assert_eq!(path, expected_path);
                    assert_eq!(integer(fields, "filetype")?, 4);
                    bail!("isolated transport failure")
                },
                |_, _| Ok(()),
            )
            .err()
            .unwrap();
            assert_eq!(error.to_string(), "isolated transport failure");
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn padding_band_and_exact_threshold_keep_filetype_five() {
        for (plaintext, ciphertext, requests) in [
            (MAX_CDN_TRANSFER_BYTES - 16, MAX_CDN_TRANSFER_BYTES, 28),
            (MAX_CDN_TRANSFER_BYTES - 1, MAX_CDN_TRANSFER_BYTES, 28),
            (MAX_CDN_TRANSFER_BYTES, MAX_CDN_TRANSFER_BYTES + 16, 29),
        ] {
            let prepared = PreparedUpload::with_material(
                &vec![0x5a; plaintext],
                MediaQuality::File,
                "boundary.fixture.bin",
                "filehelper",
                &[0x31; 16],
                "00112233445566778899aabbccddeeff",
            )
            .unwrap();
            assert_eq!(prepared.encrypted.len(), ciphertext);
            let legacy = CdnClient::new(config(), Duration::from_secs(1)).unwrap();
            let rejected = legacy.upload_with_progress(&prepared, |_, _| {
                panic!("Legacy transfer started for a multiphase file")
            });
            assert_eq!(
                rejected.err().unwrap().to_string(),
                "File requires multiphase upload"
            );
            let mut peer = Peer::new(&prepared, 1024 * 1024);
            let result = upload_large_file(
                &config(),
                &prepared,
                987654321,
                |path, fields| peer.respond(path, fields),
                |_, _| Ok(()),
            )
            .unwrap();
            assert_eq!(result.descriptor.quality, MediaQuality::File);
            assert_eq!(peer.requests, requests);
        }
    }

    #[test]
    fn completed_object_receipt_survives_late_cancellation() {
        let cancelled = std::cell::Cell::new(false);
        let mut peer = Peer::new(prepared(), 1024 * 1024);
        let uploaded = upload_large_file(
            &config(),
            prepared(),
            987654321,
            |path, fields| {
                let reply = peer.respond(path, fields)?;
                if path.ends_with("/complete") {
                    cancelled.set(true);
                }
                Ok(reply)
            },
            |_, _| {
                ensure!(!cancelled.get(), "cancelled");
                Ok(())
            },
        )
        .unwrap();
        assert!(cancelled.get());
        assert_eq!(uploaded.descriptor.file_id, "completed-object");
        assert_eq!(peer.requests, peer.blocks() + 3);
    }

    #[test]
    fn rejects_apply_changes_and_cache_hits_without_sending_blocks() {
        for (key, value) in [
            ("seq", 2),
            ("ver", 2),
            ("retcode", -1),
            ("isretry", 1),
            ("retrysec", 1),
            ("isoverload", 1),
            ("isgetcdn", 1),
            ("hit", 1),
            ("type_0", 5),
            ("num", 2),
            ("fin_0", 1),
            ("blocksize_0", 2 * 1024 * 1024),
            ("blocksize_0", -1),
        ] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    put(&mut reply, key, value);
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err(), "Accepted invalid {key}");
            assert_eq!(peer.requests, 1);
        }
    }

    #[test]
    fn rejected_reply_preserves_server_code_without_trusting_wrong_sequence() {
        let mut reply = CdnFields::new();
        put(&mut reply, "seq", 1);
        put(&mut reply, "ver", 0);
        put(&mut reply, "retcode", -5103015);
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file upload returned -5103015"
        );
        put(&mut reply, "seq", 2);
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file reply sequence mismatch"
        );
        put(&mut reply, "seq", 1);
        put(&mut reply, "retcode", 0);
        for key in ["isretry", "retrysec", "isoverload", "isgetcdn"] {
            put(&mut reply, key, 0);
        }
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Large-file reply version mismatch"
        );
        put(&mut reply, "isretry", 1);
        assert_eq!(
            validate_reply(&reply, 1, 1).unwrap_err().to_string(),
            "Unsupported large-file upload retry or routing control"
        );
        put(&mut reply, "isretry", 0);
        put(&mut reply, "ver", 1);
        validate_reply(&reply, 1, 1).unwrap();
    }

    #[test]
    fn missing_authenticated_location_stops_before_any_exchange() {
        let mut conf = config();
        conf.location = None;
        let mut requests = 0;
        let result = upload_large_file(
            &conf,
            prepared(),
            987654321,
            |_, _| {
                requests += 1;
                bail!("unexpected request")
            },
            |_, _| Ok(()),
        );
        assert_eq!(
            result.err().unwrap().to_string(),
            "Missing authenticated CDN location"
        );
        assert_eq!(requests, 0);
    }

    #[test]
    fn rejects_conflicting_or_incomplete_block_acknowledgement() {
        for bad in ["offset", "fin", "etag", "seq"] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    if path.ends_with("/uploadblock") {
                        if bad == "etag" {
                            reply.remove("etag");
                        } else {
                            put(&mut reply, bad, 1);
                        }
                    }
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err(), "Accepted invalid {bad}");
            assert_eq!(peer.requests, 2);
        }
    }

    #[test]
    fn cancellation_before_request_or_after_ack_never_continues() {
        for cancel_at in [1, 2, 3, 4, 5, 6] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let mut callbacks = 0;
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| peer.respond(path, fields),
                |_, _| {
                    callbacks += 1;
                    ensure!(callbacks != cancel_at, "cancelled");
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(callbacks, cancel_at);
            assert_eq!(peer.requests, cancel_at / 2);
        }
    }

    #[test]
    fn transport_failure_is_not_retried_and_missing_completion_is_not_success() {
        let mut requests = 0;
        assert!(upload_large_file(
            &config(),
            prepared(),
            987654321,
            |_, _| {
                requests += 1;
                bail!("transport failed")
            },
            |_, _| Ok(())
        )
        .is_err());
        assert_eq!(requests, 1);
        for phase in ["fin", "complete"] {
            let mut peer = Peer::new(prepared(), 1024 * 1024);
            let result = upload_large_file(
                &config(),
                prepared(),
                987654321,
                |path, fields| {
                    let mut reply = peer.respond(path, fields)?;
                    if path.ends_with(&format!("/{phase}")) {
                        reply.remove(if phase == "fin" { "upinfo" } else { "fileid" });
                    }
                    Ok(reply)
                },
                |_, _| Ok(()),
            );
            assert!(result.is_err());
            assert_eq!(
                peer.requests,
                peer.blocks() + if phase == "fin" { 2 } else { 3 }
            );
        }
    }

    #[test]
    fn invalid_persisted_material_and_task_are_rejected_before_network() {
        let mut altered = prepared().clone();
        altered.encrypted[0] ^= 1;
        let small = PreparedUpload::with_material(
            b"small control",
            MediaQuality::File,
            "small.bin",
            "filehelper",
            &[0x31; 16],
            "00112233445566778899aabbccddeeff",
        )
        .unwrap();
        let mut requests = 0;
        for (prepared, task) in [(&altered, 987654321), (&small, 987654321), (prepared(), 0)] {
            assert!(upload_large_file(
                &config(),
                prepared,
                task,
                |_, _| {
                    requests += 1;
                    bail!("must not reach transport")
                },
                |_, _| Ok(())
            )
            .is_err());
        }
        assert_eq!(requests, 0);
    }
}
