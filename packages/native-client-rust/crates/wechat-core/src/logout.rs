//! Local inhibition is committed before a single optional remote logout.
use super::*;

fn result(remote: &str, reason: &str, retained: bool, code: Option<i32>) -> Value {
    json!({"localCommitted":true,"autoLoginSuppressed":true,"credentialsRetained":retained,
        "remoteOutcome":remote,"reason":reason,"serviceCode":code,
        "savedDeviceRevocation":"unverified","historyRetained":true})
}

impl Core {
    pub async fn logout(self: &Arc<Self>, account: &str, key: &str) -> Result<Operation> {
        let upstream = self.upstream.clone();
        self.begin_logout_operation(account, key, false, move |state| upstream.logout(state))
            .await
    }
    /// Explicit local forgetting does not submit a remote logout or delete history.
    pub async fn forget_credentials(
        self: &Arc<Self>,
        account: &str,
        key: &str,
    ) -> Result<Operation> {
        self.begin_logout_operation(account, key, true, |_| {
            unreachable!("forget never submits a native request")
        })
        .await
    }
    async fn begin_logout_operation<F>(
        self: &Arc<Self>,
        account: &str,
        key: &str,
        forget: bool,
        remote: F,
    ) -> Result<Operation>
    where
        F: FnOnce(&mut AccountState) -> Result<(bool, i32)> + Send + 'static,
    {
        if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
            bail!("invalid logout idempotency key");
        }
        let gate = self.gate(account)?;
        let was_authenticated = self.account_status(account)?["authenticated"] == true;
        let timestamp = now();
        let op = Operation {
            id: format!("logout-{:x}", Sha256::digest(key.as_bytes())),
            account_id: account.into(),
            kind: if forget {
                "forget_credentials"
            } else {
                "logout"
            }
            .into(),
            state: "queued".into(),
            result: result("not_attempted", "pending", true, None),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        let active_key = format!("logout-active:{account}");
        let cancel = Arc::new(AtomicBool::new(false));
        let operation = {
            let mut cancels = self
                .cancels
                .lock()
                .map_err(|_| anyhow::anyhow!("operation manager unavailable"))?;
            let (op, created) = self
                .store
                .begin_logout(&op, uuid::Uuid::new_v4().as_bytes())?;
            if !created {
                return Ok(op);
            }
            if let Some(login) = cancels.get(&format!("login-active:{account}")) {
                login.store(true, Ordering::SeqCst);
            }
            cancels.insert(active_key.clone(), cancel.clone());
            cancels.insert(op.id.clone(), cancel.clone());
            op
        };
        let core = self.clone();
        let worker = operation.clone();
        tokio::spawn(async move {
            let _guard = gate.lock().await;
            let outcome = core
                .run_logout(worker.clone(), forget, was_authenticated, cancel, remote)
                .await;
            if outcome.is_err() {
                let mut failed = worker.clone();
                failed.state = "needs_review".into();
                failed.result = result("unknown", "local_operation_interrupted", true, None);
                failed.updated_at = now();
                let _ = core.store.finish_logout(&failed);
            }
            if let Ok(mut map) = core.cancels.lock() {
                map.remove(&active_key);
                map.remove(&worker.id);
            }
        });
        Ok(operation)
    }
    async fn run_logout<F>(
        self: &Arc<Self>,
        mut op: Operation,
        forget: bool,
        authenticated: bool,
        cancel: Arc<AtomicBool>,
        remote: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut AccountState) -> Result<(bool, i32)> + Send + 'static,
    {
        let (outcome, reason, retained, code) = if forget {
            self.store.forget_authorization(&op.account_id)?;
            ("not_attempted", "local_forget", false, None)
        } else if !authenticated {
            ("not_attempted", "not_authenticated", true, None)
        } else if cancel.load(Ordering::SeqCst) {
            ("not_attempted", "cancelled_before_submission", true, None)
        } else {
            op.state = "submitting".into();
            op.updated_at = now();
            op.result = result("unknown", "submission_in_progress", true, None);
            self.store.advance_logout(&op)?;
            let core = self.clone();
            let account = op.account_id.clone();
            match blocking(move || {
                let mut state = core.state_unfenced(&account)?;
                remote(&mut state)
            })
            .await
            {
                Ok((true, code)) => ("confirmed", "server_accepted", true, Some(code)),
                Ok((false, code)) => ("failed", "server_rejected", true, Some(code)),
                Err(_) => ("unknown", "response_not_verified", true, None),
            }
        };
        op.state = match outcome {
            "unknown" => "needs_review",
            "failed" => "failed",
            _ => "succeeded",
        }
        .into();
        op.updated_at = now();
        op.result = result(outcome, reason, retained, code);
        self.store.finish_logout(&op)
    }
    /// Restart recovery never repeats a possibly submitted logout. Local
    /// inhibition/forgetting survives independently from the remote outcome.
    pub fn recover_logout(&self, account: &str) -> Result<()> {
        let Some(id) = self.store.runtime(account, "logout-pending")? else {
            return Ok(());
        };
        let id = std::str::from_utf8(&id)
            .map_err(|_| anyhow::anyhow!("invalid logout recovery record"))?;
        let mut op = self.store.operation(account, id)?;
        let forget = op.kind == "forget_credentials";
        if forget {
            self.store.forget_authorization(account)?;
        }
        let attempted = op.state == "submitting";
        op.state = if attempted {
            "needs_review"
        } else {
            "succeeded"
        }
        .into();
        op.updated_at = now();
        op.result = result(
            if attempted {
                "unknown"
            } else {
                "not_attempted"
            },
            "recovered_after_restart",
            !forget,
            None,
        );
        self.store.finish_logout(&op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    fn core() -> (tempfile::TempDir, Arc<Core>) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
        store.create_account("a").unwrap();
        store.create_account("b").unwrap();
        let core = Core::new(store, None).unwrap();
        (directory, core)
    }
    async fn terminal(core: &Core, id: &str) -> Operation {
        for _ in 0..100 {
            let op = core.store.operation("a", id).unwrap();
            if !matches!(op.state.as_str(), "queued" | "submitting") {
                return op;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("logout did not finish");
    }
    #[tokio::test]
    async fn unauthenticated_logout_commits_policy_without_native_request_and_repeats_same_operation(
    ) {
        let (_temp, core) = core();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let op = core
            .begin_logout_operation("a", "fixture-key", false, move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok((true, 0))
            })
            .await
            .unwrap();
        assert!(core.store.runtime("a", "logged-out").unwrap().is_some());
        assert!(core.store.runtime("b", "logged-out").unwrap().is_none());
        let done = terminal(&core, &op.id).await;
        assert_eq!(done.result["remoteOutcome"], "not_attempted");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(core.logout("a", "fixture-key").await.unwrap().id, op.id);
    }
    #[tokio::test]
    async fn forget_requires_its_own_intent_and_retains_local_device() {
        let (_temp, core) = core();
        core.store
            .put_runtime("a", "device", b"synthetic-device")
            .unwrap();
        let op = core.forget_credentials("a", "forget-key").await.unwrap();
        let done = terminal(&core, &op.id).await;
        assert_eq!(done.result["credentialsRetained"], false);
        assert!(core.store.runtime("a", "device").unwrap().is_some());
        assert!(core.logout("a", "forget-key").await.is_err());
    }
    fn authorize(core: &Core) {
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
        let sealed = core
            .vault
            .seal("a", "account-state", &serde_json::to_vec(&state).unwrap())
            .unwrap();
        core.store.save_active_authorization("a", &sealed).unwrap();
        core.store
            .put_runtime(
                "a",
                "validated-generation",
                core.ownership_generation.as_bytes(),
            )
            .unwrap();
        assert_eq!(core.account_status("a").unwrap()["authenticated"], true);
    }
    #[tokio::test]
    async fn authenticated_logout_submits_once_and_keeps_confirmed_rejected_and_unknown_outcomes_distinct(
    ) {
        for (outcome, expected, state) in [
            (Some((true, 0)), "confirmed", "succeeded"),
            (Some((false, -13)), "failed", "failed"),
            (None, "unknown", "needs_review"),
        ] {
            let (_temp, core) = core();
            authorize(&core);
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let op = core
                .begin_logout_operation("a", "same-key", false, move |state| {
                    assert_eq!(state.uin, 42);
                    count.fetch_add(1, Ordering::SeqCst);
                    outcome.context("synthetic response unavailable")
                })
                .await
                .unwrap();
            let done = terminal(&core, &op.id).await;
            assert_eq!(done.state, state);
            assert_eq!(done.result["remoteOutcome"], expected);
            assert_eq!(done.result["credentialsRetained"], true);
            assert_eq!(done.result["savedDeviceRevocation"], "unverified");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(core.store.runtime("a", "account-state").unwrap().is_some());
            assert!(core.account_state("a").is_err());
            assert_eq!(core.logout("a", "same-key").await.unwrap().id, op.id);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn submitting_logout_restart_is_unknown_and_never_resubmitted() {
        let (_temp, core) = core();
        let timestamp = now();
        let op = Operation {
            id: "logout_fixture".into(),
            account_id: "a".into(),
            kind: "logout".into(),
            state: "submitting".into(),
            result: Value::Null,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        };
        core.store.begin_logout(&op, b"epoch").unwrap();
        core.recover_logout("a").unwrap();
        let done = core.store.operation("a", &op.id).unwrap();
        assert_eq!(done.state, "needs_review");
        assert_eq!(done.result["remoteOutcome"], "unknown");
        assert!(core.store.runtime("a", "logout-pending").unwrap().is_none());
    }
}
