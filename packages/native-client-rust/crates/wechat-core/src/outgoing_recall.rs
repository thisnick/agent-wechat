//! A recall has one durable target claim and at most one native submission.
use super::*;
use wechat_protocol::{MessageRecall, MessageRecallReply};
use wechat_store::StoredMessageRecall;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecallAttempt {
    message_id: String,
    own_username: String,
    own_uin: u32,
    request: MessageRecall,
    message_type: i64,
    authorization_generation: String,
    ownership_generation: String,
    logout_epoch: Option<Vec<u8>>,
}

fn hash(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn exact_server(value: &str) -> bool {
    value
        .parse::<u64>()
        .is_ok_and(|n| n > 0 && n.to_string() == value)
}
fn purpose(id: &str) -> String {
    format!("message-recall:{id}")
}
fn result(attempt: &RecallAttempt, status: &str, remote: &str) -> Value {
    json!({"messageId":attempt.message_id,"threadId":attempt.request.peer_username,"serverId":attempt.request.server_id,"status":status,"remoteOutcome":remote,"serviceCode":null,"retryable":false,"profileProjection":"pending"})
}

impl Core {
    fn recall_attempt(&self, record: &StoredMessageRecall) -> Result<RecallAttempt> {
        let op = &record.operation;
        let attempt: RecallAttempt = serde_json::from_slice(&self.vault.open_record(
            &op.account_id,
            &purpose(&op.id),
            &record.encrypted_request,
        )?)?;
        if op.kind != "message_recall"
            || op.result["messageId"] != attempt.message_id
            || op.result["threadId"] != attempt.request.peer_username
            || op.result["serverId"] != attempt.request.server_id
        {
            bail!("recall_operation_identity_conflict");
        }
        attempt.request.validate()?;
        Ok(attempt)
    }

    /// Admit one exact own message, return immediately, and let the server own
    /// the attempt. Disconnecting the caller never cancels or resubmits it.
    pub async fn recall_message(
        self: &Arc<Self>,
        account: &str,
        message: &str,
        expected_server_id: &str,
        key: &str,
    ) -> Result<Operation> {
        if self.stopping.load(Ordering::Acquire) {
            bail!("service_stopping");
        }
        if !exact_server(expected_server_id) || key.is_empty() || key.len() > 200 {
            bail!("invalid_recall_request");
        }
        crate::source_message_id(message)?;
        let core = self.clone();
        let account = account.to_owned();
        let message = message.to_owned();
        let server = expected_server_id.to_owned();
        let key = key.to_owned();
        let guard = self.lock_account(&account).await?;
        let worker_account = account.clone();
        let (operation, created) = blocking(move || {
            let _guard = guard;
            core.admit_message_recall(&account, &message, &server, &key)
        })
        .await?;
        if created {
            let core = self.clone();
            let id = operation.id.clone();
            tokio::spawn(async move {
                let outcome = async {
                    let guard = core.lock_account(&worker_account).await?;
                    let worker = core.clone();
                    let account = worker_account.clone();
                    let operation_id = id.clone();
                    blocking(move || {
                        let _guard = guard;
                        worker.execute_recall(&account, &operation_id, |state, request| {
                            client()?.recall_message(state, request)
                        })
                    })
                    .await
                }
                .await;
                if outcome.is_err() {
                    let _ = core.hold_interrupted_recall(&worker_account, &id);
                }
            });
        }
        Ok(operation)
    }

    fn admit_message_recall(
        &self,
        account: &str,
        message: &str,
        server: &str,
        key: &str,
    ) -> Result<(Operation, bool)> {
        if !exact_server(server) || key.is_empty() || key.len() > 200 {
            bail!("invalid_recall_request");
        }
        crate::source_message_id(message)?;
        let request_hash = hash(&(account, message, server))?;
        let account = account.to_owned();
        let message = message.to_owned();
        let server = server.to_owned();
        let key = key.to_owned();
        if let Some(prior) = self
            .store
            .message_recall_by_key(&account, &key, &request_hash)?
        {
            return Ok((self.reconcile_recall(prior)?, false));
        }
        let source_key = hash(&(&message, &server))?;
        if let Some(prior) =
            self.store
                .message_recall_by_source(&account, &key, &request_hash, &source_key)?
        {
            return Ok((self.reconcile_recall(prior)?, false));
        }
        let state = self.account_state(&account)?;
        let reader = self.profile_reader(&account)?;
        let target = reader.message(
            &crate::source_message_id(&message)?,
            Some(&state.own_username),
        )?;
        if target.server_message_id.as_deref() != Some(server.as_str()) {
            bail!("recall_target_changed");
        }
        let peer = target
            .peer
            .as_deref()
            .filter(|p| {
                !p.is_empty()
                    && p.len() <= 512
                    && !p.chars().any(|c| c.is_whitespace() || c.is_control())
            })
            .context("recall_target_unavailable")?;
        let source_local = target
            .id
            .local_id
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .context("recall_target_unavailable")?;
        let created_at = u32::try_from(target.create_time)
            .ok()
            .filter(|n| *n > 0)
            .context("recall_target_unavailable")?;
        let cipher = self
            .store
            .runtime(&account, "account-state")?
            .context("auth_required")?;
        let attempt = RecallAttempt {
            message_id: message.clone(),
            own_username: state.own_username.clone(),
            own_uin: state.uin,
            request: MessageRecall::for_source(peer.into(), server, source_local, created_at),
            message_type: target.message_type,
            authorization_generation: format!("{:x}", Sha256::digest(&cipher)),
            ownership_generation: self.ownership_generation.clone(),
            logout_epoch: self.store.runtime(&account, "logout-epoch")?,
        };
        let marker = self.observe_profile_recall(&account, &target)?;
        let self_owned = target.outgoing == Some(true)
            && target.sender.as_deref() == Some(state.own_username.as_str());
        let pending = matches!(target.raw_fields.get("status"),Some(wechat_profile::SqlValue::Integer(n)) if n=="7");
        let (op_state, status, remote) = if marker.is_some() {
            ("succeeded", "already_recalled", "confirmed")
        } else if !self_owned {
            ("failed", "not_self", "not_attempted")
        } else if pending || target.message_type <= 0 || target.message_type >= 10000 {
            ("failed", "unavailable", "not_attempted")
        } else {
            ("queued", "pending", "not_attempted")
        };
        if op_state == "queued"
            && (self
                .store
                .runtime(&account, "validated-generation")?
                .as_deref()
                != Some(self.ownership_generation.as_bytes())
                || self.store.runtime(&account, "desired")?.as_deref() != Some(b"connected")
                || !self.store.saved_login_authorization_current(
                    &account,
                    &cipher,
                    attempt.logout_epoch.as_deref(),
                )?)
        {
            // Unauthenticated requests do not permanently claim a target.
            // The worker still performs the atomic generation check.
            bail!("auth_required");
        }
        let stamp = now();
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            account_id: account.clone(),
            kind: "message_recall".into(),
            state: op_state.into(),
            result: result(&attempt, status, remote),
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        let encrypted =
            self.vault
                .seal(&account, &purpose(&op.id), &serde_json::to_vec(&attempt)?)?;
        let (record, created) = self.store.create_message_recall(
            &op,
            &key,
            &request_hash,
            &hash(&(
                &account,
                &attempt.request.peer_username,
                &attempt.request.server_id,
            ))?,
            &hash(&(&message, &attempt.request.server_id))?,
            &encrypted,
        )?;
        let should_run = created && record.phase == "queued";
        let operation = if should_run {
            record.operation
        } else {
            self.reconcile_recall(record)?
        };
        Ok((operation, should_run))
    }

    /// Re-read the source-qualified row before admission. Content is never a
    /// selector, and an opaque local ID alone never identifies a recall target.
    fn current_recall_target(
        &self,
        account: &str,
        attempt: &RecallAttempt,
        state: &AccountState,
    ) -> Result<bool> {
        if state.own_username != attempt.own_username || state.uin != attempt.own_uin {
            bail!("account_identity_mismatch");
        }
        let target = self.profile_reader(account)?.message(
            &crate::source_message_id(&attempt.message_id)?,
            Some(&state.own_username),
        )?;
        if target.server_message_id.as_deref() != Some(attempt.request.server_id.as_str())
            || target.peer.as_deref() != Some(attempt.request.peer_username.as_str())
            || target.id.local_id != attempt.request.local_id.to_string()
            || target.create_time != i64::from(attempt.request.create_time)
        {
            bail!("recall_target_changed");
        }
        if self.observe_profile_recall(account, &target)?.is_some() {
            return Ok(true);
        }
        if target.message_type != attempt.message_type
            || target.outgoing != Some(true)
            || target.sender.as_deref() != Some(attempt.own_username.as_str())
            || matches!(target.raw_fields.get("status"),Some(wechat_profile::SqlValue::Integer(n)) if n=="7")
        {
            bail!("recall_target_changed");
        }
        Ok(false)
    }

    fn execute_recall<F>(&self, account: &str, id: &str, remote: F) -> Result<Operation>
    where
        F: FnOnce(&mut AccountState, &MessageRecall) -> Result<MessageRecallReply>,
    {
        let record = self.store.message_recall(account, id)?;
        if record.phase != "queued" {
            return self.reconcile_recall(record);
        }
        let attempt = self.recall_attempt(&record)?;
        let mut op = record.operation;
        let preflight = (|| -> Result<(AccountState, Vec<u8>)> {
            if self.stopping.load(Ordering::Acquire) {
                bail!("service_stopping");
            }
            let state = self.account_state(account)?;
            if self.current_recall_target(account, &attempt, &state)? {
                return Ok((state, vec![]));
            }
            let cipher = self
                .store
                .runtime(account, "account-state")?
                .context("auth_required")?;
            if attempt.ownership_generation != self.ownership_generation
                || format!("{:x}", Sha256::digest(&cipher)) != attempt.authorization_generation
            {
                bail!("authorization_generation_changed");
            }
            attempt.request.validate()?;
            Ok((state, cipher))
        })();
        let (mut state, expected) = match preflight {
            Ok((_state, cipher)) if cipher.is_empty() => {
                return self.finish_confirmed_recall(op, &attempt, "already_recalled", false)
            }
            Ok(value) => value,
            Err(_) => {
                op.state = "failed".into();
                op.result["status"] = json!("unavailable");
                op.result["code"] = json!("recall_preflight_failed");
                op.updated_at = now();
                self.store.save_message_recall_progress(&op, "failed")?;
                return Ok(op);
            }
        };
        match self.store.claim_message_recall(
            account,
            id,
            &expected,
            attempt.logout_epoch.as_deref(),
            &self.ownership_generation,
        ) {
            Ok(true) => {}
            Ok(false) => return self.reconcile_recall(self.store.message_recall(account, id)?),
            Err(_) => {
                op.state = "failed".into();
                op.result["status"] = json!("unavailable");
                op.result["code"] = json!("authorization_generation_changed");
                op.updated_at = now();
                self.store.save_message_recall_progress(&op, "failed")?;
                return Ok(op);
            }
        }
        // The durable submitting state exists before this single call. All I/O
        // and parsing errors after it are uncertain, even if no bytes were sent.
        op.state = "running".into();
        op.result["remoteOutcome"] = json!("unknown");
        match remote(&mut state, &attempt.request) {
            Ok(reply) => {
                op.result["serviceCode"] = json!(reply.service_code);
                op.updated_at = now();
                if reply.accepted {
                    op.result["remoteOutcome"] = json!("accepted");
                    self.store.save_message_recall_progress(&op, "accepted")?;
                    // An older callback cannot replace a newer login/logout.
                    let installed = (|| -> Result<()> {
                        self.check_state_profile(account, &state)?;
                        let sealed = self.vault.seal(
                            account,
                            "account-state",
                            &serde_json::to_vec(&state)?,
                        )?;
                        self.store.save_push_authorization(
                            account,
                            &sealed,
                            &expected,
                            attempt.logout_epoch.as_deref(),
                        )?;
                        let _ = self.project_saved_login(account, &state);
                        Ok(())
                    })()
                    .is_ok();
                    op.result["authorizationSaved"] = json!(installed);
                    self.finish_confirmed_recall(op, &attempt, "recalled", true)
                } else {
                    op.state = "failed".into();
                    op.result["status"] = json!("not_eligible");
                    op.result["remoteOutcome"] = json!("rejected");
                    self.store.save_message_recall_progress(&op, "failed")?;
                    Ok(op)
                }
            }
            Err(_) => {
                op.state = "needs_review".into();
                op.result["status"] = json!("needs_review");
                op.result["remoteOutcome"] = json!("unknown");
                op.result["code"] = json!("recall_outcome_unknown");
                op.updated_at = now();
                self.store
                    .save_message_recall_progress(&op, "needs_review")?;
                Ok(op)
            }
        }
    }

    fn finish_confirmed_recall(
        &self,
        mut op: Operation,
        attempt: &RecallAttempt,
        status: &str,
        accepted: bool,
    ) -> Result<Operation> {
        let projected = self
            .apply_confirmed_recall(
                &op.account_id,
                &attempt.request.peer_username,
                &attempt.request.server_id,
                None,
                if accepted { "outgoing" } else { "reconciled" },
                &op.id,
            )
            .is_ok();
        let marker = self.store.recall_marker(
            &op.account_id,
            &attempt.request.peer_username,
            &attempt.request.server_id,
        )?;
        if marker.is_none() {
            op.state = "needs_review".into();
            op.result["status"] = json!("needs_review");
            op.result["code"] = json!("recall_marker_pending");
            op.updated_at = now();
            self.store.save_message_recall_progress(&op, "accepted")?;
            return Ok(op);
        }
        op.state = "succeeded".into();
        op.result["status"] = json!(status);
        if !accepted {
            op.result["remoteOutcome"] = json!("confirmed");
        }
        op.result["profileProjection"] = json!(if projected { "complete" } else { "pending" });
        if let Some(result) = op.result.as_object_mut() {
            result.remove("code");
        }
        op.updated_at = now();
        self.store.save_message_recall_progress(&op, "succeeded")?;
        Ok(op)
    }

    fn reconcile_recall(&self, record: StoredMessageRecall) -> Result<Operation> {
        let attempt = self.recall_attempt(&record)?;
        if record.phase == "accepted" {
            return self.finish_confirmed_recall(record.operation, &attempt, "recalled", true);
        }
        if record.phase == "succeeded" && record.operation.result["profileProjection"] == "pending"
        {
            let status = if record.operation.result["status"] == "recalled" {
                "recalled"
            } else {
                "already_recalled"
            };
            let accepted = record.operation.result["remoteOutcome"] == "accepted";
            return self.finish_confirmed_recall(record.operation, &attempt, status, accepted);
        }
        if record.phase == "needs_review" {
            // A later official import can provide confirmation even before a
            // sync control created the sidecar marker. Reconcile the same row.
            if let Ok(target) =
                self.profile_reader(&record.operation.account_id)
                    .and_then(|reader| {
                        Ok(reader.message(
                            &crate::source_message_id(&attempt.message_id)?,
                            Some(&attempt.own_username),
                        )?)
                    })
            {
                if target.server_message_id.as_deref() == Some(attempt.request.server_id.as_str())
                    && target.peer.as_deref() == Some(attempt.request.peer_username.as_str())
                    && target.create_time == i64::from(attempt.request.create_time)
                {
                    let _ = self.observe_profile_recall(&record.operation.account_id, &target);
                }
            }
            if self
                .store
                .recall_marker(
                    &record.operation.account_id,
                    &attempt.request.peer_username,
                    &attempt.request.server_id,
                )?
                .is_some()
            {
                return self.finish_confirmed_recall(
                    record.operation,
                    &attempt,
                    "already_recalled",
                    false,
                );
            }
        }
        Ok(record.operation)
    }
    fn hold_interrupted_recall(&self, account: &str, id: &str) -> Result<()> {
        let record = self.store.message_recall(account, id)?;
        let mut op = record.operation;
        let phase = match record.phase.as_str() {
            "queued" => {
                op.state = "failed".into();
                op.result["status"] = json!("unavailable");
                "failed"
            }
            "submitting" => {
                op.state = "needs_review".into();
                op.result["status"] = json!("needs_review");
                "needs_review"
            }
            "accepted" => {
                op.state = "needs_review".into();
                op.result["status"] = json!("needs_review");
                "accepted"
            }
            _ => return Ok(()),
        };
        op.result["code"] = json!("operation_interrupted");
        op.updated_at = now();
        self.store.save_message_recall_progress(&op, phase)
    }
    pub(super) fn recover_outgoing_recall_projections(&self, account: &str) -> Result<()> {
        for record in self.store.accepted_message_recalls(account)? {
            self.reconcile_recall(record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::atomic::AtomicUsize;

    fn fixture() -> (tempfile::TempDir, Arc<Store>, Arc<Core>, String) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let state = AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![b'A'; 16],
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 7,
            own_username: "synthetic_self".into(),
            cookie: vec![1; 15],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: wechat_protocol::protobuf::message(&[
                wechat_protocol::protobuf::bytes(
                    1,
                    &wechat_protocol::protobuf::buffer(b"synthetic-a"),
                ),
                wechat_protocol::protobuf::bytes(
                    2,
                    &wechat_protocol::protobuf::buffer(b"synthetic-b"),
                ),
            ])
            .unwrap(),
            key_info: vec![4; 180],
            database_password: vec![42; 32],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };
        core.configure_profile_format_key(&[9; 16]).unwrap();
        core.install_login_state("one", &state, None).unwrap();
        core.ensure_managed_profile("one", &state).unwrap();
        let source = core
            .project_accepted_text(
                "one",
                "peer",
                "Synthetic own text",
                100,
                &u64::MAX.to_string(),
                "send-synthetic",
            )
            .unwrap();
        let id = crate::message_id(&source).unwrap();
        (temporary, store, core, id)
    }
    fn admit(core: &Core, id: &str, key: &str) -> Operation {
        let (op, created) = core
            .admit_message_recall("one", id, &u64::MAX.to_string(), key)
            .unwrap();
        assert!(created);
        op
    }
    fn accepted() -> Result<MessageRecallReply> {
        Ok(MessageRecallReply {
            accepted: true,
            service_code: 0,
        })
    }
    fn original_sql(core: &Core, id: &str, sql: &str) {
        let source = crate::source_message_id(id).unwrap();
        let keys: Value = serde_json::from_slice(
            &std::fs::read(core.store.keys_file("one").unwrap().unwrap()).unwrap(),
        )
        .unwrap();
        let key = keys["databases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|key| key["relativePath"] == source.database)
            .unwrap()["rawPageKeyBase64"]
            .as_str()
            .unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(key)
            .unwrap();
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let (location, _) = core.profile_location_keys("one").unwrap();
        let db =
            rusqlite::Connection::open(location.root().join("db_storage").join(&source.database))
                .unwrap();
        db.execute_batch(&format!(
            "PRAGMA key=\"x'{hex}'\";PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;"
        ))
        .unwrap();
        assert!(source
            .table
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        db.execute_batch(&sql.replace("__TABLE__", &source.table))
            .unwrap();
    }

    #[test]
    fn accepted_recall_preserves_exact_row_and_replays_all_keys_without_transport() {
        let (_temporary, store, core, id) = fixture();
        let before = core.message("one", &id).unwrap();
        let op = admit(&core, &id, "first");
        let calls = AtomicUsize::new(0);
        let done = core
            .execute_recall("one", &op.id, |_, request| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    store.message_recall("one", &op.id).unwrap().phase,
                    "submitting"
                );
                assert_eq!(request.server_id, u64::MAX.to_string());
                assert_eq!(request.create_time, 100);
                assert_eq!(
                    request.client_message_id,
                    format!("peer_100_{}", request.local_id)
                );
                accepted()
            })
            .unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.result["status"], "recalled");
        assert_eq!(done.result["profileProjection"], "complete");
        let after = core.message("one", &id).unwrap();
        assert_eq!(after["id"], before["id"]);
        assert_eq!(after["serverId"], before["serverId"]);
        assert_eq!(after["body"], crate::recalls::RECALLED_BODY);
        for key in ["first", "other-key"] {
            let (again, created) = core
                .admit_message_recall("one", &id, &u64::MAX.to_string(), key)
                .unwrap();
            assert!(!created);
            assert_eq!(again.id, op.id);
            assert_eq!(again.state, "succeeded");
        }
        core.execute_recall("one", &op.id, |_, _| panic!("must not submit again"))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.pending_message_recalls("one").unwrap(), 0);
        assert_eq!(
            core.cancel_operation("one", &op.id)
                .unwrap_err()
                .to_string(),
            "operation_cannot_cancel"
        );
        assert_eq!(
            store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "message.recalled")
                .count(),
            1
        );
    }

    #[test]
    fn unknown_outcome_holds_target_across_keys_until_exact_confirmation() {
        let (_temporary, store, core, id) = fixture();
        let op = admit(&core, &id, "first");
        let done = core
            .execute_recall("one", &op.id, |_, _| bail!("synthetic connection loss"))
            .unwrap();
        assert_eq!(done.state, "needs_review");
        assert_eq!(done.result["remoteOutcome"], "unknown");
        assert_eq!(store.pending_message_recalls("one").unwrap(), 1);
        for key in ["first", "second", "third"] {
            let (again, created) = core
                .admit_message_recall("one", &id, &u64::MAX.to_string(), key)
                .unwrap();
            assert!(!created);
            assert_eq!(again.id, op.id);
            assert_eq!(again.state, "needs_review");
        }
        core.execute_recall("one", &op.id, |_, _| panic!("unknown outcome cannot retry"))
            .unwrap();
        assert_eq!(
            core.cancel_operation("one", &op.id)
                .unwrap_err()
                .to_string(),
            "operation_cannot_cancel"
        );
        let (location, keys) = core.profile_location_keys("one").unwrap();
        wechat_profile::apply_recall_projection(
            &location,
            &keys,
            "peer",
            &u64::MAX.to_string(),
            None,
            &core
                .write_options("one", "synthetic-official-recall")
                .unwrap(),
        )
        .unwrap();
        let (confirmed, created) = core
            .admit_message_recall("one", &id, &u64::MAX.to_string(), "fourth")
            .unwrap();
        assert!(!created);
        assert_eq!(confirmed.id, op.id);
        assert_eq!(confirmed.result["status"], "already_recalled");
        assert_eq!(confirmed.state, "succeeded");
        assert_eq!(store.pending_message_recalls("one").unwrap(), 0);
    }

    #[test]
    fn service_rejection_is_retained_without_an_age_policy_or_retry() {
        let (_temporary, store, core, id) = fixture();
        let op = admit(&core, &id, "first");
        let done = core
            .execute_recall("one", &op.id, |_, request| {
                assert_eq!(request.create_time, 100);
                Ok(MessageRecallReply {
                    accepted: false,
                    service_code: -13,
                })
            })
            .unwrap();
        assert_eq!(done.state, "failed");
        assert_eq!(done.result["status"], "not_eligible");
        assert_eq!(done.result["serviceCode"], -13);
        assert!(store
            .recall_marker("one", "peer", &u64::MAX.to_string())
            .unwrap()
            .is_none());
        assert_eq!(store.pending_message_recalls("one").unwrap(), 0);
        core.execute_recall("one", &op.id, |_, _| {
            panic!("definitive rejection must not retry")
        })
        .unwrap();
        let (again, created) = core
            .admit_message_recall("one", &id, &u64::MAX.to_string(), "second")
            .unwrap();
        assert!(!created);
        assert_eq!(again.id, op.id);
    }

    #[test]
    fn wrong_server_and_reused_local_row_never_recall_another_message() {
        let (_temporary, store, core, id) = fixture();
        assert_eq!(
            core.admit_message_recall("one", &id, "20", "first")
                .unwrap_err()
                .to_string(),
            "recall_target_changed"
        );
        let op = admit(&core, &id, "first");
        original_sql(&core, &id, "UPDATE __TABLE__ SET server_id=20;");
        let done = core
            .execute_recall("one", &op.id, |_, _| panic!("row was reused"))
            .unwrap();
        assert_eq!(done.state, "failed");
        assert_eq!(done.result["remoteOutcome"], "not_attempted");
        assert_eq!(store.pending_message_recalls("one").unwrap(), 0);
        // Even a different key returns the frozen old operation, without
        // guessing the replacement row's current identity or submitting it.
        let (again, created) = core
            .admit_message_recall("one", &id, &u64::MAX.to_string(), "second")
            .unwrap();
        assert!(!created);
        assert_eq!(again.id, op.id);
    }

    #[test]
    fn incoming_message_and_provisional_own_id_are_not_submitted() {
        for change in ["UPDATE __TABLE__ SET real_sender_id=(SELECT rowid FROM Name2Id WHERE user_name='peer');","UPDATE __TABLE__ SET status=7;"]{
            let (_temporary,_store,core,id)=fixture();original_sql(&core,&id,change);
            let (op,created)=core.admit_message_recall("one",&id,&u64::MAX.to_string(),"first").unwrap();
            assert!(!created);assert_eq!(op.state,"failed");assert!(matches!(op.result["status"].as_str(),Some("not_self"|"unavailable")));
            core.execute_recall("one",&op.id,|_,_|panic!("not an eligible own confirmed message")).unwrap();
        }
    }

    #[test]
    fn accepted_reply_cannot_restore_superseded_authorization() {
        let (_temporary, store, core, id) = fixture();
        let op = admit(&core, &id, "first");
        let mut new_cipher = vec![];
        let done = core
            .execute_recall("one", &op.id, |state, _| {
                let stamp = now();
                let logout = Operation {
                    id: "synthetic-logout".into(),
                    account_id: "one".into(),
                    kind: "logout".into(),
                    state: "queued".into(),
                    result: json!({}),
                    created_at: stamp.clone(),
                    updated_at: stamp,
                };
                store.begin_logout(&logout, b"new-epoch")?;
                let mut newer = state.clone();
                newer.cookie = vec![9; 15];
                new_cipher =
                    core.vault
                        .seal("one", "account-state", &serde_json::to_vec(&newer)?)?;
                store.commit_login_authorization(
                    "one",
                    &new_cipher,
                    Some(b"new-epoch"),
                    &core.ownership_generation,
                )?;
                state.cookie = vec![2; 15];
                accepted()
            })
            .unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.result["authorizationSaved"], false);
        assert_eq!(
            store.runtime("one", "account-state").unwrap().unwrap(),
            new_cipher
        );
        assert_eq!(
            store.runtime("one", "logout-epoch").unwrap().unwrap(),
            b"new-epoch"
        );
    }

    #[test]
    fn projection_failure_keeps_marker_and_repairs_locally_without_resubmission() {
        let (_temporary, store, core, id) = fixture();
        let op = admit(&core, &id, "first");
        let binding = store
            .runtime("one", "profile-write-binding")
            .unwrap()
            .unwrap();
        let done = core
            .execute_recall("one", &op.id, |_, _| {
                store.clear_runtime("one", "profile-write-binding")?;
                accepted()
            })
            .unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.result["profileProjection"], "pending");
        assert_eq!(
            core.message("one", &id).unwrap()["body"],
            crate::recalls::RECALLED_BODY
        );
        store
            .put_runtime("one", "profile-write-binding", &binding)
            .unwrap();
        let repaired = core
            .execute_recall("one", &op.id, |_, _| panic!("local repair cannot submit"))
            .unwrap();
        assert_eq!(repaired.result["profileProjection"], "complete");
        assert_eq!(store.pending_recall_projections("one").unwrap(), 0);
    }
}
