use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use wechat_protocol::media::{
    MediaDescriptor, MediaQuality, PreparedUpload, UploadedImage, UploadedMedia, UploadedVideo,
};

pub(super) fn request(
    db: &mut Connection,
    session: &Session,
    input: MediaRequest,
) -> Result<Value> {
    check_session(db, session)?;
    match input {
        MediaRequest::Sticker { descriptor } => {
            let bytes = include_bytes!("../fixtures/sticker.gif");
            ensure!(
                descriptor.url == sticker_url(&session.account)
                    && descriptor.expected_bytes == bytes.len() as u64
                    && descriptor.expected_md5 == hex(&wechat_protocol::crypto::md5(bytes))
                    && descriptor.expires_at.is_none(),
                "simulator_sticker_not_found"
            );
            Ok(json!(STANDARD.encode(bytes)))
        }
        MediaRequest::Upload {
            metadata,
            encrypted,
        } => {
            save_object(
                db,
                &session.account,
                &metadata,
                &STANDARD.decode(encrypted)?,
            )?;
            Ok(serde_json::to_value(metadata)?)
        }
        MediaRequest::Download { descriptor } => {
            let (raw, encrypted): (String, Vec<u8>) = db.query_row(
                "SELECT metadata,encrypted FROM media_objects WHERE account=?1 AND id=?2 AND quality=?3",
                params![session.account,descriptor.file_id,descriptor.quality.filetype()], |r| Ok((r.get(0)?,r.get(1)?)),
            ).context("simulator_media_not_found")?;
            let metadata: UploadedMedia = serde_json::from_str(&raw)?;
            ensure!(
                metadata.descriptor.aes_key == descriptor.aes_key,
                "simulator_media_key_mismatch"
            );
            let bytes = validate_object(&metadata, &encrypted)?;
            Ok(json!(STANDARD.encode(bytes)))
        }
        MediaRequest::Voice {
            server_id,
            length,
            offset,
        } => {
            let bytes: Vec<u8> = db
                .query_row(
                    "SELECT data FROM voice_objects WHERE account=?1 AND server_id=?2",
                    params![session.account, server_id.to_string()],
                    |r| r.get(0),
                )
                .context("simulator_media_not_found")?;
            ensure!(
                bytes.len() == length as usize && offset < length,
                "invalid_voice_offset"
            );
            let end = (offset as usize + 32 * 1024).min(bytes.len());
            Ok(json!(STANDARD.encode(&bytes[offset as usize..end])))
        }
        MediaRequest::Submit {
            destination,
            client_id,
            created_at,
            message_type,
            content,
            voice,
        } => {
            validate_destination(db, &session.account, &destination)?;
            ensure!(
                !client_id.is_empty()
                    && client_id.len() <= 200
                    && !client_id.chars().any(char::is_control)
                    && created_at > 0
                    && [3, 34, 43, 49].contains(&message_type)
                    && !content.is_empty()
                    && content.len() <= MAX_BODY,
                "invalid_media_send"
            );
            let hash = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    &destination,
                    created_at,
                    message_type,
                    &content,
                    &voice
                ))?)
            );
            let existing: Option<(String,Option<String>,bool)> = db.query_row(
                "SELECT hash,server_id,accepted FROM media_sends WHERE account=?1 AND client_id=?2", params![session.account,client_id],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((old, server_id, accepted)) = existing {
                ensure!(old == hash, "simulator_send_identity_conflict");
                return Ok(serde_json::to_value(SendReply {
                    accepted,
                    server_id,
                })?);
            }
            let encoded = voice.map(|value| STANDARD.decode(value)).transpose()?;
            ensure!(
                (message_type == 34) == encoded.is_some(),
                "invalid_voice_payload"
            );
            if let Some(bytes) = &encoded {
                wechat_protocol::silk::decode(bytes)?;
            }
            let tx = db.transaction()?;
            let fault: String = tx.query_row(
                "SELECT fault FROM accounts WHERE id=?1",
                [&session.account],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE accounts SET fault='none' WHERE id=?1",
                [&session.account],
            )?;
            let accepted = fault != "reject";
            let server_id = if accepted {
                let (server, _) = store_message(
                    &tx,
                    &session.account,
                    message_type,
                    &own(&session.account),
                    &destination,
                    content.as_bytes(),
                    created_at,
                )?;
                if let Some(bytes) = encoded {
                    tx.execute(
                        "INSERT INTO voice_objects VALUES(?1,?2,?3)",
                        params![session.account, server, bytes],
                    )?;
                }
                Some(server)
            } else {
                None
            };
            tx.execute(
                "INSERT INTO media_sends VALUES(?1,?2,?3,?4,?5)",
                params![session.account, client_id, hash, server_id, accepted],
            )?;
            tx.commit()?;
            if fault == "lost_acknowledgment" {
                bail!("simulated_lost_acknowledgment");
            }
            Ok(serde_json::to_value(SendReply {
                accepted,
                server_id,
            })?)
        }
    }
}
fn validate_object(metadata: &UploadedMedia, encrypted: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        metadata.descriptor.file_id.starts_with("simm_")
            && metadata.descriptor.file_id.len() <= 128
            && metadata.descriptor.aes_key.len() == 16
            && metadata.plaintext_bytes > 0
            && metadata.plaintext_bytes as usize <= wechat_core::MAX_SOURCE_BYTES
            && encrypted.len() == (metadata.plaintext_bytes as usize / 16 + 1) * 16,
        "invalid_simulator_media"
    );
    let bytes = wechat_protocol::media::decrypt_cdn(encrypted, &metadata.descriptor.aes_key)?;
    let md5 = hex(&wechat_protocol::crypto::md5(&bytes));
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    ensure!(
        bytes.len() == metadata.plaintext_bytes as usize
            && metadata.md5 == md5
            && metadata.sha256 == sha256
            && metadata.descriptor.expected_md5.as_deref() == Some(md5.as_str())
            && metadata.descriptor.expected_bytes == Some(bytes.len() as u64),
        "media_integrity_failed"
    );
    Ok(bytes)
}
fn save_object(
    db: &Connection,
    account: &str,
    metadata: &UploadedMedia,
    encrypted: &[u8],
) -> Result<()> {
    validate_object(metadata, encrypted)?;
    let previous: Option<String> = db
        .query_row(
            "SELECT metadata FROM media_objects WHERE account=?1 AND id=?2 AND quality=?3",
            params![
                account,
                metadata.descriptor.file_id,
                metadata.descriptor.quality.filetype()
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let old: UploadedMedia = serde_json::from_str(&previous)?;
        ensure!(
            old.sha256 == metadata.sha256 && old.descriptor.aes_key == metadata.descriptor.aes_key,
            "simulator_media_identity_conflict"
        );
        return Ok(());
    }
    db.execute(
        "INSERT INTO media_objects VALUES(?1,?2,?3,?4,?5)",
        params![
            account,
            metadata.descriptor.file_id,
            metadata.descriptor.quality.filetype(),
            serde_json::to_string(metadata)?,
            encrypted
        ],
    )?;
    Ok(())
}
pub(super) fn receive(db: &mut Connection, account: &str, peer: &str, kind: &str) -> Result<()> {
    validate_destination(db, account, peer)?;
    ensure!(
        ["image", "file", "video", "voice", "sticker"].contains(&kind),
        "unsupported_simulator_media"
    );
    seed(db, account)?;
    let tx = db.transaction()?;
    let image = image_bytes()?;
    let (message_type, xml, voice) = if kind == "sticker" {
        let bytes = include_bytes!("../fixtures/sticker.gif");
        (
            47,
            format!(
                "<msg><emoji cdnurl=\"{}\" len=\"{}\" md5=\"{}\" /></msg>",
                sticker_url(account),
                bytes.len(),
                hex(&wechat_protocol::crypto::md5(bytes))
            ),
            None,
        )
    } else if kind == "voice" {
        let pcm: Vec<i16> = (0..16000)
            .map(|n| ((n as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 1800.0) as i16)
            .collect();
        let bytes = wechat_protocol::silk::encode(&pcm)?;
        (
            34,
            wechat_protocol::media_send::local_voice_xml(bytes.len(), 1000)?,
            Some(bytes),
        )
    } else {
        let (bytes, quality, name) = match kind {
            "image" => (image.clone(), MediaQuality::Original, "simulator-image.png"),
            "video" => (
                include_bytes!("../fixtures/sample.mp4").to_vec(),
                MediaQuality::Video,
                "simulator-video.mp4",
            ),
            _ => (
                b"A file from the local AgentWechat simulator.\n".to_vec(),
                MediaQuality::File,
                "simulator-note.txt",
            ),
        };
        let material = format!(
            "{:x}",
            Sha256::digest(format!("{account}:{kind}:{}", head(&tx, account)?))
        );
        let prepared =
            PreparedUpload::with_material(&bytes, quality, name, peer, &[7; 16], &material[..32])?;
        let primary = fixture_object(&tx, account, &prepared, quality, &bytes)?;
        let (message_type, xml) = if kind == "image" {
            let standard = fixture_object(&tx, account, &prepared, MediaQuality::Standard, &bytes)?;
            let thumbnail =
                fixture_object(&tx, account, &prepared, MediaQuality::Thumbnail, &bytes)?;
            (
                3,
                wechat_protocol::media_send::local_image_xml(&UploadedImage {
                    original: primary,
                    standard,
                    thumbnail,
                })?,
            )
        } else if kind == "video" {
            let thumbnail =
                fixture_object(&tx, account, &prepared, MediaQuality::Thumbnail, &image)?;
            (
                43,
                wechat_protocol::media_send::local_video_xml(
                    &UploadedVideo {
                        video: primary,
                        thumbnail,
                        original: None,
                    },
                    1,
                )?,
            )
        } else {
            (
                49,
                wechat_protocol::media_send::local_file_xml(peer, &primary)?,
            )
        };
        (message_type, xml, None)
    };
    let (server, command) = store_message(
        &tx,
        account,
        message_type,
        peer,
        &own(account),
        xml.as_bytes(),
        clock(&tx)?,
    )?;
    if let Some(bytes) = voice {
        tx.execute(
            "INSERT INTO voice_objects VALUES(?1,?2,?3)",
            params![account, server, bytes],
        )?;
    }
    append(&tx, account, &command)?;
    notify(&tx, account, 1)?;
    tx.commit()?;
    Ok(())
}
fn fixture_object(
    db: &Connection,
    account: &str,
    original: &PreparedUpload,
    quality: MediaQuality,
    bytes: &[u8],
) -> Result<UploadedMedia> {
    let metadata = UploadedMedia {
        descriptor: MediaDescriptor {
            file_id: format!("simm_{}", original.file_key),
            aes_key: original.aes_key.clone(),
            quality,
            expected_bytes: Some(bytes.len() as u64),
            expected_md5: Some(hex(&wechat_protocol::crypto::md5(bytes))),
            strict_size: true,
            expires_at: None,
        },
        plaintext_bytes: bytes.len() as u32,
        md5: hex(&wechat_protocol::crypto::md5(bytes)),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        file_name: original.file_name.clone(),
        width: Some(256),
        height: Some(160),
    };
    let prepared = PreparedUpload::with_material(
        bytes,
        original.kind,
        &original.file_name,
        &original.destination,
        &original.aes_key,
        &original.file_key,
    )?;
    save_object(db, account, &metadata, &prepared.encrypted)?;
    Ok(metadata)
}
fn image_bytes() -> Result<Vec<u8>> {
    let image = image::RgbImage::from_fn(256, 160, |x, y| {
        image::Rgb([189, (70 + x / 4) as u8, (25 + y / 2) as u8])
    });
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}
fn sticker_url(account: &str) -> String {
    // Only metadata uses this approved host. Simulator retrieval is a local RPC
    // with exact fixture identity checks and never resolves or fetches this URL.
    format!("https://snsvideo.c2c.wechat.com/agentwechat-simulator/{account}/sticker.gif")
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn login(engine: &Engine, account: &str) -> Session {
        let challenge = engine
            .request(Request::StartLogin {
                account: account.into(),
                device: DeviceProfile::generate("UnifiedPCLinux x86_64".into()).unwrap(),
                computer_name: "fixture".into(),
                saved: false,
            })
            .unwrap();
        engine
            .control(Control::Login {
                account: account.into(),
                status: LoginStatus::Approved,
            })
            .unwrap();
        let reply: Authorization = serde_json::from_value(
            engine
                .request(Request::CompleteLogin {
                    challenge: challenge["id"].as_str().unwrap().into(),
                })
                .unwrap(),
        )
        .unwrap();
        Session {
            account: account.into(),
            generation: u64::from_le_bytes(reply.state.cookie.as_slice().try_into().unwrap()),
        }
    }
    #[test]
    fn fixture_bytes_are_account_scoped_and_integrity_checked() {
        let temp = wechat_platform::private_tempdir("sim-media-test-").unwrap();
        let engine = Engine::open(&temp.path().join("state.sqlite")).unwrap();
        let first = login(&engine, "first");
        let other = login(&engine, "other");
        for kind in ["image", "file", "video", "voice"] {
            engine
                .control(Control::ReceiveMedia {
                    account: first.account.clone(),
                    peer: "sim_mei".into(),
                    kind: kind.into(),
                })
                .unwrap();
        }
        let descriptor: MediaDescriptor = {
            let db = engine.db.lock().unwrap();
            let raw:String=db.query_row("SELECT metadata FROM media_objects WHERE account='first' AND quality=?1 LIMIT 1",[MediaQuality::Original.filetype()],|r|r.get(0)).unwrap();
            serde_json::from_str::<UploadedMedia>(&raw)
                .unwrap()
                .descriptor
                .clone()
        };
        let encoded = engine
            .request(Request::Media {
                session: first,
                request: MediaRequest::Download {
                    descriptor: descriptor.clone(),
                },
            })
            .unwrap();
        let bytes = STANDARD.decode(encoded.as_str().unwrap()).unwrap();
        assert_eq!(bytes, image_bytes().unwrap());
        assert!(engine
            .request(Request::Media {
                session: other,
                request: MediaRequest::Download {
                    descriptor: descriptor.clone()
                }
            })
            .is_err());
        let db = engine.db.lock().unwrap();
        let raw: String = db
            .query_row("SELECT metadata FROM media_objects LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        let mut metadata: UploadedMedia = serde_json::from_str(&raw).unwrap();
        metadata.sha256 = "invalid".into();
        assert!(save_object(&db, "first", &metadata, &[0; 16]).is_err());
    }
    #[test]
    fn animated_sticker_fixture_is_checked_and_never_fetches_a_url() {
        let temp = wechat_platform::private_tempdir("sim-sticker-test-").unwrap();
        let engine = Engine::open(&temp.path().join("state.sqlite")).unwrap();
        let first = login(&engine, "first");
        let other = login(&engine, "other");
        let bytes = include_bytes!("../fixtures/sticker.gif");
        let descriptor = wechat_protocol::media::StickerDescriptor {
            url: sticker_url("first"),
            expected_bytes: bytes.len() as u64,
            expected_md5: hex(&wechat_protocol::crypto::md5(bytes)),
            expires_at: None,
        };
        let encoded = engine
            .request(Request::Media {
                session: first.clone(),
                request: MediaRequest::Sticker {
                    descriptor: descriptor.clone(),
                },
            })
            .unwrap();
        let decoded = STANDARD.decode(encoded.as_str().unwrap()).unwrap();
        let inspection = wechat_protocol::media::inspect_image(&decoded).unwrap();
        assert_eq!(inspection.content_type, "image/gif");
        assert_eq!(inspection.frames, 2);
        assert!(engine
            .request(Request::Media {
                session: other,
                request: MediaRequest::Sticker {
                    descriptor: descriptor.clone()
                },
            })
            .is_err());
        for (index, mut changed) in [descriptor.clone(), descriptor.clone()]
            .into_iter()
            .enumerate()
        {
            if index == 0 {
                changed.expected_bytes += 1;
            } else {
                changed.expected_md5 = "0".repeat(32);
            }
            assert!(engine
                .request(Request::Media {
                    session: first.clone(),
                    request: MediaRequest::Sticker {
                        descriptor: changed
                    },
                })
                .is_err());
        }
        let mut changed = descriptor;
        changed.url = "https://snsvideo.c2c.wechat.com/other".into();
        assert!(engine
            .request(Request::Media {
                session: first,
                request: MediaRequest::Sticker {
                    descriptor: changed
                },
            })
            .is_err());
    }
    #[test]
    fn media_acceptance_survives_a_lost_acknowledgment_without_duplicate_submission() {
        let temp = wechat_platform::private_tempdir("sim-media-send-").unwrap();
        let engine = Engine::open(&temp.path().join("state.sqlite")).unwrap();
        let session = login(&engine, "one");
        engine
            .control(Control::SendFault {
                account: "one".into(),
                fault: SendFault::LostAcknowledgment,
            })
            .unwrap();
        let send = || Request::Media {
            session: session.clone(),
            request: MediaRequest::Submit {
                destination: "sim_mei".into(),
                client_id: "durable-media-key".into(),
                created_at: INITIAL_TIME,
                message_type: 49,
                content: "<msg><appmsg><type>6</type></appmsg></msg>".into(),
                voice: None,
            },
        };
        assert!(engine.request(send()).is_err());
        let first = engine.request(send()).unwrap();
        let second = engine.request(send()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first["accepted"], true);
        let db = engine.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM media_sends WHERE account='one'",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
            1
        );
    }
}
