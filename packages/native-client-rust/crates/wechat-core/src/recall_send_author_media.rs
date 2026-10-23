//! Child of media: inspect the existing typed sealed plan without projecting it.
use super::*;

impl Core {
    pub(crate) fn recall_media_send_matches(
        &self,
        account: &str,
        message: &wechat_profile::Message,
    ) -> Result<(usize, bool)> {
        let (Some(thread), Some(server)) = (
            message.peer.as_deref(),
            message.server_message_id.as_deref(),
        ) else {
            return Ok((0, false));
        };
        let candidates = self
            .store
            .recall_media_send_candidates(account, thread, server)?;
        let count = candidates.len();
        let Some(row) = candidates.into_iter().next().filter(|_| count == 1) else {
            return Ok((count, false));
        };
        let plan = self
            .vault
            .open_record(account, &format!("media-job:{}", row.id), &row.payload)
            .ok()
            .and_then(|plain| serde_json::from_slice::<SendPlan>(&plain).ok());
        let Some(plan) = plan else {
            return Ok((count, false));
        };
        if row.kind != "send"
            || row.data["threadId"] != thread
            || plan.request.thread_id != thread
            || i64::from(plan.created_at) != message.create_time
            || plan.client_id.is_empty()
            || plan.client_id.len() > 1024
        {
            return Ok((count, false));
        }
        let source = crate::message_id(&message.id)?;
        if let Some(ids) = row.data["result"].get("messageIds") {
            let Some(ids) = ids.as_array() else {
                return Ok((count, false));
            };
            if !ids.is_empty()
                && (!ids.iter().all(|id| id.is_string())
                    || !ids.iter().any(|id| id.as_str() == Some(&source)))
            {
                return Ok((count, false));
            }
        }
        let expected_type = match plan.request.kind {
            MediaKind::File => 49,
            MediaKind::Image => 3,
            MediaKind::Video => 43,
            MediaKind::Voice => 34,
            _ => return Ok((count, false)),
        };
        if message.message_type != 10000 && message.message_type != expected_type {
            return Ok((count, false));
        }
        if plan.request.kind == MediaKind::Voice {
            // The producer sets accepted only after decode_voice_send establishes
            // exactly one matching client ID. A job can have accepted earlier
            // notes even when a later note has an uncertain outcome.
            let parts = plan
                .voice
                .iter()
                .enumerate()
                .filter(|(_, part)| part.server_id.as_deref() == Some(server))
                .collect::<Vec<_>>();
            let matches = parts.len() == 1
                && parts[0].1.status == "accepted"
                && parts[0].1.client_id == format!("{}_{}", plan.client_id, parts[0].0)
                && row.data["result"]["chunks"]
                    .as_array()
                    .is_some_and(|chunks| {
                        chunks
                            .iter()
                            .filter(|chunk| chunk["serverMessageId"] == server)
                            .count()
                            == 1
                            && chunks.iter().any(|chunk| {
                                chunk["serverMessageId"] == server
                                    && chunk["state"] == "accepted"
                                    && chunk["index"].as_u64() == Some(parts[0].0 as u64)
                            })
                    });
            return Ok((count, matches));
        }
        let receipt = serde_json::from_value::<wechat_protocol::media_send::MediaSendResult>(
            row.data["result"]["nativeReceipt"].clone(),
        )
        .ok();
        let matches = row.state == "accepted"
            && row.data["serverMessageId"] == server
            && plan.voice.is_empty()
            && receipt.is_some_and(|receipt| {
                receipt.status == wechat_protocol::media_send::SendStatus::ServerAccepted
                    && receipt.service_code == 0
                    && receipt.client_id_matches == Some(true)
                    && receipt.server_message_id.as_deref() == Some(server)
            });
        Ok((count, matches))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(
        kind: MediaKind,
    ) -> (
        tempfile::TempDir,
        Arc<wechat_store::Store>,
        Arc<Core>,
        wechat_profile::Message,
        SendPlan,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("other").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let field = || wechat_profile::DecodedField {
            text: None,
            state: wechat_profile::DecodeState::Null,
            compression_flag: None,
        };
        use md5::Digest;
        let message = wechat_profile::Message {
            id: wechat_profile::SourceMessageId {
                database: "message/message_0.db".into(),
                table: format!("Msg_{:x}", md5::Md5::digest(b"room@chatroom")),
                local_id: "7".into(),
            },
            server_message_id: Some("18446744073709551615".into()),
            server_id_signed: None,
            peer: Some("room@chatroom".into()),
            sender: Some(String::new()),
            outgoing: Some(false),
            message_type: 10000,
            local_type: "10000".into(),
            create_time: 123,
            sort_seq: None,
            content: field(),
            source: field(),
            raw_fields: Default::default(),
        };
        let plan = SendPlan {
            request: MediaSendRequest {
                thread_id: "room@chatroom".into(),
                kind,
                source_id: "synthetic-source".into(),
                standard_source_id: None,
                thumbnail_source_id: None,
                play_seconds: None,
                duration_ms: None,
                video_quality: None,
            },
            client_id: "synthetic-client".into(),
            created_at: 123,
            material: None,
            reservation: None,
            file_upload_progress_id: None,
            uploaded_file: None,
            uploaded_image: None,
            uploaded_video: None,
            prepared_video_thumbnail_source_id: None,
            prepared_video_source_id: None,
            original_video_material: None,
            voice: vec![],
            reuse_completed_upload: None,
        };
        (temporary, store, core, message, plan)
    }

    fn save(
        core: &Core,
        id: &str,
        plan: &SendPlan,
        message: &wechat_profile::Message,
        correlated: Option<bool>,
    ) -> MediaJobRecord {
        let payload = core
            .vault
            .seal(
                "one",
                &format!("media-job:{id}"),
                &serde_json::to_vec(plan).unwrap(),
            )
            .unwrap();
        let receipt = wechat_protocol::media_send::MediaSendResult {
            status: wechat_protocol::media_send::SendStatus::ServerAccepted,
            service_code: 0,
            server_message_id: message.server_message_id.clone(),
            client_id_matches: correlated,
        };
        let data = json!({"threadId":plan.request.thread_id,"sourceId":plan.request.source_id,"serverMessageId":message.server_message_id,
            "result":{"nativeReceipt":receipt,"messageIds":[crate::message_id(&message.id).unwrap()]}});
        let (mut row, _) = core
            .store
            .create_media_job(
                "one",
                id,
                &format!("send:{id}"),
                &"a".repeat(64),
                "send",
                &data,
                &payload,
            )
            .unwrap();
        row.state = "accepted".into();
        row.phase = "completed".into();
        core.store.save_media_job("one", &row).unwrap()
    }

    #[test]
    fn correlated_media_receipts_match_exact_cleared_source_without_mutation() {
        for kind in [MediaKind::File, MediaKind::Image, MediaKind::Video] {
            let (_temporary, store, core, message, plan) = setup(kind);
            let row = save(&core, "accepted", &plan, &message, Some(true));
            let events = store.events("one", 0, 100).unwrap().len();
            assert_eq!(
                core.recall_media_send_matches("one", &message).unwrap(),
                (1, true)
            );
            assert_eq!(
                store.media_job("one", &row.id).unwrap().payload,
                row.payload
            );
            assert_eq!(store.events("one", 0, 100).unwrap().len(), events);
            assert_eq!(
                core.recall_media_send_matches("other", &message).unwrap(),
                (0, false)
            );
            for changed in ["time", "source", "server", "thread", "type"] {
                let mut target = message.clone();
                match changed {
                    "time" => target.create_time += 1,
                    "source" => target.id.local_id = "8".into(),
                    "server" => target.server_message_id = Some("12".into()),
                    "thread" => target.peer = Some("other@chatroom".into()),
                    "type" => target.message_type = 1,
                    _ => unreachable!(),
                }
                assert!(
                    !core.recall_media_send_matches("one", &target).unwrap().1,
                    "{changed}"
                );
            }
        }
    }

    #[test]
    fn missing_correlation_wrong_sealed_plan_and_duplicate_jobs_stay_unknown() {
        for changed in [
            "missing",
            "false",
            "sealed_thread",
            "sealed_time",
            "native_server",
            "native_status",
            "duplicate",
        ] {
            let (_temporary, store, core, message, mut plan) = setup(MediaKind::File);
            if changed == "sealed_thread" {
                plan.request.thread_id = "other@chatroom".into();
            }
            if changed == "sealed_time" {
                plan.created_at += 1;
            }
            let mut row = save(
                &core,
                "accepted",
                &plan,
                &message,
                match changed {
                    "missing" => None,
                    "false" => Some(false),
                    _ => Some(true),
                },
            );
            // The unsealed selector must never override contradictory sealed evidence.
            row.data["threadId"] = json!(message.peer);
            if changed == "native_server" {
                row.data["result"]["nativeReceipt"]["serverMessageId"] = "12".into();
            }
            if changed == "native_status" {
                row.data["result"]["nativeReceipt"]["status"] = "needs_review".into();
            }
            store.save_media_job("one", &row).unwrap();
            if changed == "duplicate" {
                save(&core, "second", &plan, &message, Some(true));
            }
            assert!(
                !core.recall_media_send_matches("one", &message).unwrap().1,
                "{changed}"
            );
        }
    }

    #[test]
    fn accepted_voice_part_remains_proof_when_a_later_note_is_uncertain() {
        let (_temporary, store, core, message, mut plan) = setup(MediaKind::Voice);
        plan.voice = vec![
            VoicePart {
                client_id: "synthetic-client_0".into(),
                encoded: vec![1, 2],
                duration_ms: 1000,
                status: "accepted".into(),
                server_id: message.server_message_id.clone(),
            },
            VoicePart {
                client_id: "synthetic-client_1".into(),
                encoded: vec![3, 4],
                duration_ms: 1000,
                status: "needs_review".into(),
                server_id: Some("12".into()),
            },
        ];
        let mut row = save(&core, "voice", &plan, &message, Some(false));
        row.state = "needs_review".into();
        row.data["serverMessageId"] = "12".into();
        row.data["result"]["nativeReceipt"]["status"] = "needs_review".into();
        row.data["result"]["nativeReceipt"]["serverMessageId"] = "12".into();
        store.save_media_job("one", &row).unwrap();
        core.save_send_plan("one", &row.id, &plan).unwrap();
        assert_eq!(
            core.recall_media_send_matches("one", &message).unwrap(),
            (1, true)
        );
        for changed in ["uncertain", "client", "duplicate_part", "chunk_mismatch"] {
            let mut altered = plan.clone();
            match changed {
                "uncertain" => altered.voice[0].status = "needs_review".into(),
                "client" => altered.voice[0].client_id = "other-client".into(),
                "duplicate_part" => altered.voice[1].server_id = message.server_message_id.clone(),
                "chunk_mismatch" => {}
                _ => unreachable!(),
            }
            core.save_send_plan("one", &row.id, &altered).unwrap();
            if changed == "chunk_mismatch" {
                let mut saved = store.media_job("one", &row.id).unwrap();
                saved.data["result"]["chunks"][0]["index"] = 1.into();
                store.save_media_job("one", &saved).unwrap();
            }
            assert!(
                !core.recall_media_send_matches("one", &message).unwrap().1,
                "{changed}"
            );
        }
    }
}
