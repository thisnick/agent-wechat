use super::*;
use std::future::Future;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SessionFence {
    pub(super) authorization: [u8; 32],
    pub(super) logout_epoch: Option<Vec<u8>>,
}

pub(super) fn authentication_required(error: &anyhow::Error) -> bool {
    auth_rejected(error)
        || error
            .chain()
            .any(|cause| cause.to_string() == "auth_required")
}

impl Core {
    fn session_fence(&self, account: &str) -> Result<SessionFence> {
        Ok(SessionFence {
            authorization: authorization(&self.account_state(account)?)?,
            logout_epoch: self.store.runtime(account, "logout-epoch")?,
        })
    }

    pub(super) fn session_state(
        &self,
        account: &str,
        fence: &SessionFence,
    ) -> Result<AccountState> {
        let state = self.account_state(account)?;
        if self.store.runtime(account, "logout-epoch")? != fence.logout_epoch
            || authorization(&state)? != fence.authorization
        {
            bail!("session_superseded");
        }
        Ok(state)
    }

    /// Commit a response only to the exact credential state that issued it.
    pub(super) fn save_network_state(
        &self,
        account: &str,
        state: &AccountState,
        expected: &[u8],
        fence: &SessionFence,
        desired: Option<&[u8]>,
    ) -> Result<()> {
        self.session_state(account, fence)?;
        self.check_state_profile(account, state)?;
        let sealed = self
            .vault
            .seal(account, "account-state", &serde_json::to_vec(state)?)?;
        #[cfg(feature = "experimental-calls")]
        self.prepare_call_authorization(account, state)?;
        self.store.save_network_authorization(
            account,
            &sealed,
            expected,
            fence.logout_epoch.as_deref(),
            desired,
        )?;
        #[cfg(feature = "experimental-calls")]
        self.bind_call_authorization(account, state)?;
        let _ = self.project_saved_login(account, state);
        Ok(())
    }

    pub(super) async fn session_request<T, F, Fut>(
        self: &Arc<Self>,
        account: &str,
        source: &str,
        request: F,
    ) -> Result<T>
    where
        F: FnOnce(SessionFence) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let fence = {
            let _guard = self.lock_account(account).await?;
            self.session_fence(account)?
        };
        match request(fence.clone()).await {
            Err(error) if authentication_required(&error) => {
                let logged_out = self
                    .logout_rejected_session(account, &fence, &error, source)
                    .await?;
                if logged_out || self.store.runtime(account, "logged-out")?.is_some() {
                    Err(error.context("auth_required"))
                } else {
                    bail!("session_superseded")
                }
            }
            result => result,
        }
    }

    async fn logout_rejected_session(
        &self,
        account: &str,
        fence: &SessionFence,
        error: &anyhow::Error,
        source: &str,
    ) -> Result<bool> {
        if !authentication_required(error) {
            return Ok(false);
        }
        let _guard = self.lock_account(account).await?;
        // Network responses can arrive after logout, disconnect or a new login.
        // Only the still-current rejected session may install a logout fence.
        if self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            || self.session_fence(account).ok().as_ref() != Some(fence)
        {
            return Ok(false);
        }
        self.logout_local(account)?;
        self.store.append_event(
            account,
            "account.connection",
            &json!({"state":"auth_required","source":source,
                "serviceCode":if auth_rejected(error) { Some(-13) } else { None },
                "authentication":"logged_out"}),
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn core() -> (tempfile::TempDir, Arc<Core>) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
        for account in ["one", "other"] {
            store.create_account(account).unwrap();
        }
        let core = Core::new(store, None).unwrap();
        for account in ["one", "other"] {
            core.save_state(account, &super::super::tests::state())
                .unwrap();
            core.store
                .put_runtime(account, "desired", b"connected")
                .unwrap();
            core.store
                .mark_validated_if_active(account, &core.ownership_generation)
                .unwrap();
        }
        (directory, core)
    }

    #[tokio::test]
    async fn rejected_push_polling_and_sync_requests_log_out_once_without_resubmission() {
        for (source, boundary) in [
            ("push", "Native envelope"),
            ("polling", "Heartbeat"),
            ("sync", "Sync service"),
            ("contact_sync", "Contact sync service"),
        ] {
            let (_directory, core) = core();
            let (send, _) = core
                .store
                .create_send("one", "pending", "filehelper", "hash", b"synthetic", "1")
                .unwrap();
            let calls = AtomicUsize::new(0);
            let result: Result<()> = core
                .session_request("one", source, |_| async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::anyhow!(format!("{boundary} returned -13"))
                        .context("request failed"))
                })
                .await;
            assert!(result.is_err());
            let status = core.account_status("one").unwrap();
            assert_eq!(status["authenticated"], false);
            assert_eq!(status["connection"], "disconnected");
            assert_eq!(status["incomingTransport"], "disconnected");
            assert!(core.store.runtime("one", "logged-out").unwrap().is_some());
            assert!(
                core.state_unfenced("one").is_ok(),
                "credentials remain available for an explicit login"
            );
            assert_eq!(core.account_status("other").unwrap()["authenticated"], true);
            assert_eq!(
                core.store.send("one", &send.id).unwrap().state,
                "needs_review"
            );
            assert!(core.store.queued_sends("one").unwrap().is_empty());

            let events = core.store.events("one", 0, 200).unwrap();
            let diagnostic = events
                .iter()
                .find(|event| event.kind == "account.connection")
                .unwrap();
            assert_eq!(
                diagnostic.data,
                json!({"state":"auth_required","source":source,"serviceCode":-13,"authentication":"logged_out"})
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.kind == "account.updated"
                        && event.data["authentication"] == "logged_out")
                    .count(),
                1
            );
            let epoch = core.store.runtime("one", "logout-epoch").unwrap();
            let again: Result<()> = core
                .session_request("one", source, |_| async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await;
            assert!(again.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(core.store.runtime("one", "logout-epoch").unwrap(), epoch);
        }
    }

    #[tokio::test]
    async fn local_auth_required_context_is_recognized_without_guessing_from_substrings() {
        let (_directory, core) = core();
        let failure: Result<()> = core
            .session_request("one", "push", |_| async {
                Err(anyhow::anyhow!("auth_required").context("request failed"))
            })
            .await;
        assert!(failure.is_err());
        assert_eq!(core.account_status("one").unwrap()["authenticated"], false);
        let events = core.store.events("one", 0, 200).unwrap();
        let diagnostic = events
            .iter()
            .find(|event| event.kind == "account.connection")
            .unwrap();
        assert!(diagnostic.data["serviceCode"].is_null());

        for message in [
            "network timeout",
            "HTTP 503",
            "Native envelope returned -130",
            "path/auth_required/cache",
            "profile -13",
        ] {
            let (_directory, active) = self::core();
            let failure: Result<()> = active
                .session_request("one", "push", |_| async {
                    Err(anyhow::anyhow!(message.to_owned()))
                })
                .await;
            assert!(failure.is_err());
            assert_eq!(
                active.account_status("one").unwrap()["authenticated"],
                true,
                "{message}"
            );
            assert!(active
                .store
                .runtime("one", "logout-epoch")
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn late_rejection_cannot_log_out_replaced_credentials_or_a_new_login_epoch() {
        for new_login in [false, true] {
            let (_directory, core) = core();
            let (started, admitted) = tokio::sync::oneshot::channel();
            let (finish, completed) = tokio::sync::oneshot::channel();
            let worker = core.clone();
            let request = tokio::spawn(async move {
                worker
                    .session_request("one", "push", |_| async {
                        started.send(()).unwrap();
                        completed.await.unwrap();
                        Err::<(), _>(anyhow::anyhow!("Native envelope returned -13"))
                    })
                    .await
            });
            admitted.await.unwrap();
            {
                let _guard = core.lock_account("one").await.unwrap();
                let mut state = super::super::tests::state();
                if new_login {
                    core.logout_local("one").unwrap();
                    let epoch = core.store.runtime("one", "logout-epoch").unwrap().unwrap();
                    core.install_login_state("one", &state, Some(&epoch))
                        .unwrap();
                } else {
                    state.request_key[0] ^= 1;
                    core.save_state("one", &state).unwrap();
                }
            }
            finish.send(()).unwrap();
            assert!(request.await.unwrap().is_err());
            assert_eq!(core.account_status("one").unwrap()["authenticated"], true);
            assert!(!core
                .store
                .events("one", 0, 200)
                .unwrap()
                .iter()
                .any(|event| event.kind == "account.connection"
                    && event.data["state"] == "auth_required"));
        }
    }

    #[tokio::test]
    async fn a_sync_rejection_keeps_auth_required_through_its_polling_caller() {
        let (_directory, core) = core();
        let failure = core
            .session_request("one", "polling", |_| async {
                core.session_request("one", "sync", |_| async {
                    Err::<(), _>(anyhow::anyhow!("Sync service returned -13"))
                })
                .await
            })
            .await
            .unwrap_err();
        assert_eq!(failure.to_string(), "auth_required");
        let events = core.store.events("one", 0, 200).unwrap();
        let diagnostics: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "account.connection")
            .collect();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].data["source"], "sync");
    }

    #[tokio::test]
    async fn duplicate_failures_and_restart_keep_one_logout_fence_and_retained_data() {
        let (directory, core) = core();
        let fence = core.session_fence("one").unwrap();
        let error = anyhow::anyhow!("Heartbeat returned -13");
        let (one, two) = tokio::join!(
            core.logout_rejected_session("one", &fence, &error, "polling"),
            core.logout_rejected_session("one", &fence, &error, "push")
        );
        assert_eq!(usize::from(one.unwrap()) + usize::from(two.unwrap()), 1);
        let epoch = core.store.runtime("one", "logout-epoch").unwrap();
        drop(core);
        let store = Arc::new(Store::open_existing(&directory.path().join("data")).unwrap());
        let reopened = Core::new(store, None).unwrap();
        assert_eq!(
            reopened.account_status("one").unwrap()["authenticated"],
            false
        );
        assert_eq!(
            reopened.account_status("one").unwrap()["connection"],
            "disconnected"
        );
        assert_eq!(
            reopened.store.runtime("one", "logout-epoch").unwrap(),
            epoch
        );
        assert!(reopened.state_unfenced("one").is_ok());
        assert_eq!(reopened.store.accounts().unwrap().len(), 2);
    }
}
