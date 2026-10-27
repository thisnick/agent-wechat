//! Read-only attribution from existing accepted send receipts after profile clearing.
//! This is a child of runtime so encrypted intents use the actual SendPayload type.
use super::*;

fn canonical_server(value: &str) -> bool {
    value
        .parse::<u64>()
        .is_ok_and(|number| number > 0 && number.to_string() == value)
}

impl Core {
    /// Missing, conflicting or unsupported evidence leaves the author unknown.
    /// This does not project a send, reconcile a recall or retain new attributes.
    pub(crate) fn recall_send_author(
        &self,
        account: &str,
        message: &wechat_profile::Message,
        own: &str,
    ) -> Result<Option<String>> {
        let Some(thread) = message.peer.as_deref() else {
            return Ok(None);
        };
        let Some(server) = message
            .server_message_id
            .as_deref()
            .filter(|server| canonical_server(server))
        else {
            return Ok(None);
        };
        if own.is_empty()
            || own.len() > 512
            || own.chars().any(|c| c.is_control() || c.is_whitespace())
            || thread.is_empty()
            || thread.len() > 512
            || thread.chars().any(|c| c.is_control() || c.is_whitespace())
            || message
                .sender
                .as_deref()
                .filter(|sender| !sender.is_empty())
                .is_some_and(|sender| sender != own)
            || u32::try_from(message.create_time)
                .ok()
                .filter(|n| *n > 0)
                .is_none()
            || self.profile_username(account)?.as_deref() != Some(own)
        {
            return Ok(None);
        }
        use md5::Digest;
        let table = format!("Msg_{:x}", md5::Md5::digest(thread.as_bytes()));
        if !message.id.table.eq_ignore_ascii_case(&table) {
            return Ok(None);
        }
        let source = crate::message_id(&message.id)?;
        let candidates = self
            .store
            .recall_text_send_candidates(account, thread, server)?;
        let (media_count, media_matches) = self.recall_media_send_matches(account, message)?;
        if candidates.len() + media_count != 1 {
            return Ok(None);
        }
        if media_matches {
            return Ok(Some(own.into()));
        }
        let Some((intent, encrypted)) = candidates.into_iter().next() else {
            return Ok(None);
        };
        let payload = self
            .vault
            .open_record(account, "send-payload", &encrypted)
            .ok()
            .and_then(|plain| serde_json::from_slice::<SendPayload>(&plain).ok());
        let Some(payload) = payload else {
            return Ok(None);
        };
        let expected_type = if payload.share.is_some() || payload.quote.is_some() {
            49
        } else {
            1
        };
        let matches = intent.account_id == account
            && intent.state == "accepted"
            && intent.destination == thread
            && intent.server_id.as_deref() == Some(server)
            && payload.thread_id == thread
            && i64::from(payload.created_at) == message.create_time
            && payload.client_id > 0
            && intent.client_id == payload.client_id.to_string()
            && !(payload.share.is_some() && payload.quote.is_some())
            && payload
                .share
                .as_ref()
                .is_none_or(|share| share.destination() == thread)
            && (message.message_type == 10000 || message.message_type == expected_type)
            && intent
                .local_message_id
                .as_deref()
                .is_none_or(|id| id == source);
        Ok(matches.then(|| own.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (
        tempfile::TempDir,
        Arc<Store>,
        Arc<Core>,
        wechat_profile::Message,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        store.create_account("other").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let state = AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![b'A'; 16],
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 1,
            own_username: "synthetic_self".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };
        core.restore_credentials("one", &serde_json::to_vec(&state).unwrap())
            .unwrap();
        let field = || wechat_profile::DecodedField {
            text: None,
            state: wechat_profile::DecodeState::Null,
            compression_flag: None,
        };
        use md5::Digest;
        let message = wechat_profile::Message {
            id: wechat_profile::SourceMessageId {
                database: "message/message_0.db".into(),
                table: format!("Msg_{:x}", md5::Md5::digest(b"peer")),
                local_id: "7".into(),
            },
            server_message_id: Some("18446744073709551615".into()),
            server_id_signed: None,
            peer: Some("peer".into()),
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
        (temporary, store, core, message)
    }

    fn intent(core: &Core, key: &str, message: &wechat_profile::Message) -> SendIntent {
        let payload = SendPayload {
            thread_id: "peer".into(),
            text: "original body".into(),
            created_at: 123,
            client_id: 42,
            share: None,
            quote: None,
            mentions: None,
        };
        let encrypted = core
            .vault
            .seal(
                "one",
                "send-payload",
                &serde_json::to_vec(&payload).unwrap(),
            )
            .unwrap();
        let (intent, _) = core
            .store
            .create_send("one", key, "peer", "synthetic-hash", &encrypted, "42")
            .unwrap();
        core.store
            .update_send(
                "one",
                &intent.id,
                "accepted",
                message.server_message_id.as_deref(),
                None,
            )
            .unwrap()
    }

    #[test]
    fn accepted_text_author_survives_clear_and_restart_without_changing_receipts() {
        let (temporary, store, core, message) = setup();
        let intent = intent(&core, "accepted", &message);
        let source = crate::message_id(&message.id).unwrap();
        store
            .finish_text_projection("one", &intent.id, &source)
            .unwrap();
        let events = store.events("one", 0, 100).unwrap().len();
        let payload = store.send_payload("one", &intent.id).unwrap();
        assert_eq!(
            core.recall_send_author("one", &message, "synthetic_self")
                .unwrap()
                .as_deref(),
            Some("synthetic_self")
        );
        assert_eq!(store.events("one", 0, 100).unwrap().len(), events);
        assert_eq!(store.send_payload("one", &intent.id).unwrap(), payload);
        drop(core);
        drop(store);
        let store = Arc::new(Store::open_existing(&temporary.path().join("data")).unwrap());
        let core = Core::new(store, None).unwrap();
        assert_eq!(
            core.recall_send_author("one", &message, "synthetic_self")
                .unwrap()
                .as_deref(),
            Some("synthetic_self")
        );
    }

    #[test]
    fn text_evidence_rejects_reused_source_wrong_identity_uncertainty_and_ambiguity() {
        let (_temporary, store, core, message) = setup();
        let intent = intent(&core, "first", &message);
        for changed in [
            "server", "thread", "time", "table", "own", "account", "sender", "source",
        ] {
            let mut target = message.clone();
            match changed {
                "server" => target.server_message_id = Some("12".into()),
                "thread" => target.peer = Some("other".into()),
                "time" => target.create_time += 1,
                "table" => target.id.table = "Msg_wrong".into(),
                "sender" => target.sender = Some("peer".into()),
                "source" => store
                    .finish_text_projection("one", &intent.id, "different-source")
                    .unwrap(),
                _ => {}
            }
            assert!(
                core.recall_send_author(
                    if changed == "account" { "other" } else { "one" },
                    &target,
                    if changed == "own" {
                        "other_self"
                    } else {
                        "synthetic_self"
                    }
                )
                .unwrap()
                .is_none(),
                "{changed}"
            );
        }
        store
            .clear_runtime("one", &format!("send-projected:{}", intent.id))
            .unwrap();
        store
            .update_send("one", &intent.id, "needs_review", None, None)
            .unwrap();
        assert!(core
            .recall_send_author("one", &message, "synthetic_self")
            .unwrap()
            .is_none());
        store
            .update_send("one", &intent.id, "accepted", None, None)
            .unwrap();
        self::intent(&core, "second", &message);
        assert!(core
            .recall_send_author("one", &message, "synthetic_self")
            .unwrap()
            .is_none());
    }

    #[test]
    fn sealed_share_or_text_identity_must_agree_with_the_accepted_receipt() {
        for changed in [
            "share",
            "share_thread",
            "client",
            "sealed_thread",
            "sealed_time",
            "cipher",
        ] {
            let (_temporary, store, core, message) = setup();
            let payload = SendPayload {
                thread_id: if changed == "sealed_thread" {
                    "other"
                } else {
                    "peer"
                }
                .into(),
                text: "original".into(),
                created_at: if changed == "sealed_time" { 124 } else { 123 },
                client_id: if changed == "client" { 43 } else { 42 },
                share: matches!(changed, "share" | "share_thread").then(|| {
                    crate::ShareSendRequest::Share {
                        thread_id: if changed == "share_thread" {
                            "other"
                        } else {
                            "peer"
                        }
                        .into(),
                        title: "Synthetic share".into(),
                        url: "https://example.com/".into(),
                        description: None,
                        thumbnail_url: None,
                    }
                }),
                quote: None,
                mentions: None,
            };
            let encrypted = if changed == "cipher" {
                b"invalid ciphertext".to_vec()
            } else {
                core.vault
                    .seal(
                        "one",
                        "send-payload",
                        &serde_json::to_vec(&payload).unwrap(),
                    )
                    .unwrap()
            };
            let (intent, _) = store
                .create_send(
                    "one",
                    "accepted",
                    "peer",
                    "synthetic-hash",
                    &encrypted,
                    "42",
                )
                .unwrap();
            store
                .update_send(
                    "one",
                    &intent.id,
                    "accepted",
                    message.server_message_id.as_deref(),
                    None,
                )
                .unwrap();
            assert_eq!(
                core.recall_send_author("one", &message, "synthetic_self")
                    .unwrap()
                    .is_some(),
                changed == "share",
                "{changed}"
            );
        }
    }
}
