//! Explicit whole-conversation reads and one-attempt cross-device notification.
use super::*;
use wechat_protocol::{SessionReadNotification, SessionReadReply};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadNotificationReceipt {
    notification: SessionReadNotification,
    through_message_id: String,
    logout_epoch: Option<Vec<u8>>,
    local_result: Value,
    state: String,
    service_code: Option<i32>,
    authorization_saved: bool,
    updated_at: String,
}

fn notification_key(account: &str, notification: &SessionReadNotification) -> Result<String> {
    Ok(format!(
        "thread-read-notification:{:x}",
        Sha256::digest(serde_json::to_vec(&(
            account,
            &notification.peer_username,
            &notification.last_message_server_id,
            notification.last_message_create_time,
        ))?)
    ))
}

impl ReadNotificationReceipt {
    fn response(&self) -> Value {
        let mut value = self.local_result.clone();
        if !value.is_object() {
            value = json!({});
        }
        value["readScope"] = json!("thread");
        value["throughMessageId"] = json!(self.through_message_id);
        value["phoneReceiptSent"] = json!(self.state == "accepted");
        value["phoneNotification"] = json!({
            "state":self.state,
            "serviceCode":self.service_code,
            "authorizationSaved":self.authorization_saved,
            "retryable":false,
            "lastMessageServerId":self.notification.last_message_server_id,
            "lastMessageCreateTime":self.notification.last_message_create_time,
        });
        value
    }
}

impl Core {
    fn read_notification_receipt(
        &self,
        account: &str,
        notification: &SessionReadNotification,
    ) -> Result<Option<ReadNotificationReceipt>> {
        let key = notification_key(account, notification)?;
        let Some(cipher) = self.store.runtime(account, &key)? else {
            return Ok(None);
        };
        let mut receipt: ReadNotificationReceipt =
            serde_json::from_slice(&self.vault.open_record(account, &key, &cipher)?)?;
        if receipt.notification != *notification {
            bail!("read_notification_identity_conflict");
        }
        if receipt.state == "submitting" {
            // An admitted request can have reached the service before a crash.
            // Recovery records uncertainty; it never submits the request again.
            receipt.state = "needs_review".into();
            receipt.updated_at = now();
            self.save_read_notification(account, &key, &receipt)?;
        }
        if !matches!(
            receipt.state.as_str(),
            "accepted" | "rejected" | "needs_review"
        ) {
            bail!("invalid_read_notification_state");
        }
        Ok(Some(receipt))
    }

    fn save_read_notification(
        &self,
        account: &str,
        key: &str,
        receipt: &ReadNotificationReceipt,
    ) -> Result<()> {
        self.store.put_runtime(
            account,
            key,
            &self
                .vault
                .seal(account, key, &serde_json::to_vec(receipt)?)?,
        )
    }

    /// Explicitly read a whole thread through a frozen local-order boundary.
    /// Local profile projection and remote notification have separate receipts.
    pub async fn mark_thread_read(
        self: &Arc<Self>,
        account: &str,
        thread: &str,
        expected: &str,
    ) -> Result<Value> {
        if thread.is_empty() || thread.len() > 512 || expected.len() > 128 {
            bail!("invalid read request");
        }
        let core = self.clone();
        let account = account.to_owned();
        let thread = thread.to_owned();
        let expected = expected.to_owned();
        let guard = self.lock_account(&account).await?;
        blocking(move || {
            let _guard = guard;
            let state = core.account_state(&account)?;
            let reader = core.profile_reader(&account)?;
            let page = reader.messages(&wechat_profile::MessageQuery {
                chat: Some(thread.clone()),
                own_username: Some(state.own_username.clone()),
                limit: Some(200),
                ..Default::default()
            })?;
            // Profile order is authoritative. Server IDs are opaque identities,
            // never an ordering key or a substitute for a message timestamp.
            let latest = page
                .items
                .iter()
                .find(|message| {
                    message.peer.as_deref() == Some(thread.as_str())
                        && message.server_message_id.as_deref().is_some_and(|id| {
                            id.parse::<u64>()
                                .is_ok_and(|n| n != 0 && n.to_string() == id)
                        })
                        && u32::try_from(message.create_time).is_ok_and(|time| time != 0)
                })
                .context("read_notification_boundary_unavailable")?;
            let through = crate::message_id(&latest.id)?;
            let mut notification = SessionReadNotification {
                peer_username: thread.clone(),
                last_message_server_id: latest
                    .server_message_id
                    .clone()
                    .context("read_notification_boundary_unavailable")?,
                last_message_create_time: u32::try_from(latest.create_time)?,
                client_message_id: String::new(),
            };
            notification.client_message_id = notification_key(&account, &notification)?;
            let epoch = core.store.runtime(&account, "logout-epoch")?;
            drop(reader);
            // The same account guard spans the frozen boundary, local CAS and
            // remote admission. Later sync arrivals cannot widen this request.
            let local = core.project_thread_read(
                &account,
                &thread,
                i64::from(notification.last_message_create_time),
                &expected,
            )?;
            core.submit_read_notification(
                &account,
                notification,
                through,
                epoch,
                local,
                |state, request| client()?.notify_read_session(state, request),
            )
        })
        .await
    }

    /// Recovery completes only an already-durable local projection. It never
    /// reconstructs or retries a native notification after an interrupted task.
    pub(super) async fn recover_thread_read_projection(
        self: &Arc<Self>,
        account: &str,
        thread: &str,
        through_time: i64,
        expected: &str,
    ) -> Result<Value> {
        let core = self.clone();
        let account = account.to_owned();
        let thread = thread.to_owned();
        let expected = expected.to_owned();
        let guard = self.lock_account(&account).await?;
        blocking(move || {
            let _guard = guard;
            core.project_thread_read(&account, &thread, through_time, &expected)
        })
        .await
    }

    /// The caller holds the account mutation guard. Reuse the existing read
    /// journal so a crash between profile commit and event publication recovers.
    fn project_thread_read(
        &self,
        account: &str,
        thread: &str,
        through_time: i64,
        expected: &str,
    ) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let counter = self.store.read_version(account, thread)?;
        let intent = json!({"threadReadTime":through_time,"expectedVersion":expected});
        if self.read_state_version(account, thread, reader.generation())? != expected
            && self
                .store
                .runtime(account, &format!("read-pending:{thread}"))?
                .as_deref()
                != Some(serde_json::to_vec(&intent)?.as_slice())
        {
            bail!("read_state_conflict");
        }
        drop(reader);
        let durable = format!(
            "read-thread:{:x}",
            Sha256::digest(serde_json::to_vec(&intent)?)
        );
        let options = self.write_options(account, &durable)?;
        let next = self.store.begin_read(account, thread, &counter, &intent)?;
        let (location, keys) = self.profile_location_keys(account)?;
        let projection =
            wechat_profile::mark_read_thread(&location, &keys, thread, through_time, &options)?;
        let reader = self.profile_reader(account)?;
        let version = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(reader.generation(), thread, &next))?)
        );
        self.store.finish_read_projection(account, thread, &next, &durable,
            &json!({"threadId":thread,"version":version,"readScope":"thread","throughTime":through_time}))?;
        Ok(
            json!({"threadId":thread,"version":version,"projection":projection,"readScope":"thread","phoneReceiptSent":false}),
        )
    }

    /// Caller owns the account guard. Admission and the logout fence share a
    /// transaction; the durable submitting record precedes the one native call.
    fn submit_read_notification<F>(
        &self,
        account: &str,
        notification: SessionReadNotification,
        through: String,
        epoch: Option<Vec<u8>>,
        local: Value,
        remote: F,
    ) -> Result<Value>
    where
        F: FnOnce(&mut AccountState, &SessionReadNotification) -> Result<SessionReadReply>,
    {
        if let Some(mut prior) = self.read_notification_receipt(account, &notification)? {
            // A later explicit read of the same server boundary still clears a
            // locally toggled unread marker, without sending the receipt twice.
            if local.get("version").is_some() {
                prior.local_result = local;
                prior.updated_at = now();
                self.save_read_notification(
                    account,
                    &notification_key(account, &notification)?,
                    &prior,
                )?;
            }
            return Ok(prior.response());
        }
        let mut state = self.account_state(account)?;
        let key = notification_key(account, &notification)?;
        let mut receipt = ReadNotificationReceipt {
            notification,
            through_message_id: through,
            logout_epoch: epoch.clone(),
            local_result: local,
            state: "submitting".into(),
            service_code: None,
            authorization_saved: false,
            updated_at: now(),
        };
        let cipher = self
            .vault
            .seal(account, &key, &serde_json::to_vec(&receipt)?)?;
        self.store.control(account, |database| {
            let tx = database.transaction()?;
            let logged_out: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime WHERE key='logged-out')", [], |row| row.get(0))?;
            let current_epoch: Vec<u8> = tx.query_row("SELECT COALESCE((SELECT value FROM runtime WHERE key='logout-epoch'),CAST('' AS BLOB))", [], |row| row.get(0))?;
            if logged_out || current_epoch.as_slice() != epoch.as_deref().unwrap_or_default() {
                bail!("auth_required");
            }
            tx.execute("INSERT INTO runtime(key,value)VALUES(?1,?2)", (&key, &cipher))?;
            tx.commit()?;
            Ok(())
        })?;
        let outcome = remote(&mut state, &receipt.notification);
        // A successful service response is still retained if a concurrent
        // logout forbids publishing the resulting authorization cookie.
        receipt.authorization_saved = match &outcome {
            Ok(reply) => self.persist_state(account, &state, reply.accepted).is_ok(),
            Err(_) => false,
        };
        match outcome {
            Ok(reply) => {
                receipt.state = if reply.accepted {
                    "accepted"
                } else {
                    "rejected"
                }
                .into();
                receipt.service_code = Some(reply.service_code);
            }
            Err(_) => receipt.state = "needs_review".into(),
        }
        receipt.updated_at = now();
        self.save_read_notification(account, &key, &receipt)?;
        Ok(receipt.response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn fixture() -> (tempfile::TempDir, Arc<Core>) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        let core = Core::new(store, None).unwrap();
        let state = AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: b"0123456789abcdef".to_vec(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 42,
            own_username: "synthetic_user".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![4; 16],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };
        core.store
            .save_active_authorization(
                "a",
                &core
                    .vault
                    .seal("a", "account-state", &serde_json::to_vec(&state).unwrap())
                    .unwrap(),
            )
            .unwrap();
        (directory, core)
    }

    fn notification() -> SessionReadNotification {
        SessionReadNotification {
            peer_username: "peer".into(),
            last_message_server_id: "18446744073709551614".into(),
            last_message_create_time: 100,
            client_message_id: "stable-id".into(),
        }
    }

    #[test]
    fn accepted_rejected_and_unknown_receipts_replay_without_another_submission() {
        for outcome in [Some(0), Some(-13), None] {
            let (_directory, core) = fixture();
            let calls = AtomicUsize::new(0);
            let first = core
                .submit_read_notification(
                    "a",
                    notification(),
                    "msg-fixture".into(),
                    None,
                    json!({"version":"local-read","projection":{"wholeThread":true}}),
                    |_state, _request| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert!(core
                            .store
                            .runtime("a", &notification_key("a", &notification()).unwrap())
                            .unwrap()
                            .is_some());
                        match outcome {
                            Some(code) => Ok(SessionReadReply {
                                service_code: code,
                                accepted: code == 0,
                            }),
                            None => bail!("lost reply"),
                        }
                    },
                )
                .unwrap();
            assert_eq!(
                first["phoneNotification"]["state"],
                match outcome {
                    Some(0) => "accepted",
                    Some(_) => "rejected",
                    None => "needs_review",
                }
            );
            let second = core
                .submit_read_notification(
                    "a",
                    notification(),
                    "msg-fixture".into(),
                    None,
                    json!({}),
                    |_, _| panic!("must not resubmit"),
                )
                .unwrap();
            assert_eq!(first, second);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(core
                .read_notification_receipt("b", &notification())
                .unwrap()
                .is_none());
            let mut changed = notification();
            changed.last_message_create_time += 1;
            assert!(core
                .read_notification_receipt("a", &changed)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn interrupted_submission_becomes_uncertain_and_logout_fences_prevent_the_call() {
        let (_directory, core) = fixture();
        let key = notification_key("a", &notification()).unwrap();
        let receipt = ReadNotificationReceipt {
            notification: notification(),
            through_message_id: "msg-fixture".into(),
            logout_epoch: None,
            local_result: json!({}),
            state: "submitting".into(),
            service_code: None,
            authorization_saved: false,
            updated_at: now(),
        };
        core.save_read_notification("a", &key, &receipt).unwrap();
        let replay = core
            .submit_read_notification(
                "a",
                notification(),
                "msg-fixture".into(),
                None,
                json!({}),
                |_, _| panic!("must not retry interrupted request"),
            )
            .unwrap();
        assert_eq!(replay["phoneNotification"]["state"], "needs_review");
        let (_directory, core) = fixture();
        core.store
            .put_runtime("a", "logout-epoch", b"newer-epoch")
            .unwrap();
        assert!(core
            .submit_read_notification(
                "a",
                notification(),
                "msg-fixture".into(),
                None,
                json!({}),
                |_, _| panic!("epoch changed before submission")
            )
            .is_err());
        assert!(core.store.runtime("a", &key).unwrap().is_none());
        core.store
            .put_runtime("a", "logged-out", b"newer-epoch")
            .unwrap();
        assert!(core
            .submit_read_notification(
                "a",
                notification(),
                "msg-fixture".into(),
                Some(b"newer-epoch".to_vec()),
                json!({}),
                |_, _| panic!("logged out before submission")
            )
            .is_err());
    }
}
