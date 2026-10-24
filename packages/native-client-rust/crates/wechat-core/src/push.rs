use super::*;
use wechat_protocol::wire::{Heartbeat, SyncBatch};

fn heartbeat_interval(heartbeat: &Heartbeat) -> Duration {
    Duration::from_secs((heartbeat.next_interval as u64).clamp(30, 180))
}
impl Core {
    pub(super) fn push_connected(&self, account: &str) -> bool {
        self.push_health
            .lock()
            .ok()
            .and_then(|m| m.get(account).cloned())
            .is_some_and(|ready| ready.load(Ordering::Acquire))
    }
    fn push_allowed(&self, account: &str) -> Result<bool> {
        Ok(!self.stopping.load(Ordering::SeqCst)
            && self.store.runtime(account, "desired")?.as_deref() == Some(b"connected")
            && self.store.runtime(account, "logged-out")?.is_none()
            && self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                == Some(self.ownership_generation.as_bytes()))
    }
    fn push_state(&self, account: &str, fence: &SessionFence) -> Result<AccountState> {
        if !self.push_allowed(account)?
            || self.store.runtime(account, "logout-epoch")? != fence.logout_epoch
        {
            bail!("push_session_superseded");
        }
        let state = self.account_state(account)?;
        if authorization(&state)? != fence.authorization {
            bail!("push_session_superseded");
        }
        Ok(state)
    }
    fn save_push_state(
        &self,
        account: &str,
        state: &AccountState,
        expected: &[u8],
        fence: &SessionFence,
    ) -> Result<()> {
        self.push_state(account, fence)?;
        self.check_state_profile(account, state)?;
        let sealed = self
            .vault
            .seal(account, "account-state", &serde_json::to_vec(state)?)?;
        self.store.save_push_authorization(
            account,
            &sealed,
            expected,
            fence.logout_epoch.as_deref(),
        )?;
        let _ = self.project_saved_login(account, state);
        Ok(())
    }
    pub(super) fn start_push_worker(self: &Arc<Self>, account: &str) -> Result<()> {
        let ready = Arc::new(AtomicBool::new(false));
        self.push_health
            .lock()
            .map_err(|_| anyhow::anyhow!("push manager unavailable"))?
            .insert(account.into(), ready.clone());
        let core = self.clone();
        let account = account.to_owned();
        tokio::spawn(async move {
            let mut backoff = 1;
            while !core.stopping.load(Ordering::SeqCst) && core.store.account(&account).is_ok() {
                if core.push_allowed(&account).unwrap_or(false) {
                    let result = core.push_session(&account, &ready).await;
                    let was_ready = ready.swap(false, Ordering::AcqRel);
                    if was_ready {
                        backoff = 1;
                    }
                    if let Err(error) = result {
                        if core.push_allowed(&account).unwrap_or(false) {
                            let _ = core.store.append_event_once(&account,
                                &format!("push:reconnecting:{}", wechat_store::unix_seconds() / 60),
                                "account.push", &json!({"state":"reconnecting","fallback":"heartbeat_polling","reason":if authentication_required(&error){"auth_required"}else{"connection_unavailable"}}));
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                } else {
                    ready.store(false, Ordering::Release);
                    backoff = 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            ready.store(false, Ordering::Release);
        });
        Ok(())
    }
    async fn push_session(self: &Arc<Self>, account: &str, ready: &AtomicBool) -> Result<()> {
        self.session_request(account, "push", |fence| {
            self.push_session_active(account, ready, fence)
        })
        .await
    }
    async fn push_session_active(
        self: &Arc<Self>,
        account: &str,
        ready: &AtomicBool,
        fence: SessionFence,
    ) -> Result<()> {
        let guard = self.lock_account(account).await?;
        let core = self.clone();
        let owned = account.to_owned();
        let (mut connection, fence, heartbeat) = blocking(move || {
            let _guard = guard;
            if !core.push_allowed(&owned)? {
                bail!("push_session_superseded");
            }
            let mut state = core.push_state(&owned, &fence)?;
            let expected = core
                .store
                .runtime(&owned, "account-state")?
                .context("auth_required")?;
            let cursor = core.cursor(&owned)?;
            let mut connection = core.upstream.push_connection(&state)?;
            let heartbeat = connection.heartbeat(&mut state, &cursor)?;
            core.save_push_state(&owned, &state, &expected, &fence)?;
            Ok((connection, fence, heartbeat))
        })
        .await?;
        // Catch-up uses the same durable staging/projection path as manual sync.
        // Only a fully drained baseline makes later commits live inbound events.
        connection = self
            .drain_push_sync(account, connection, &fence, 6, "catchup")
            .await?;
        ready.store(true, Ordering::Release);
        self.store.append_event(
            account,
            "account.push",
            &json!({"state":"connected","transport":if self.upstream.kind() == crate::upstream::UpstreamKind::Simulator {"simulator"} else {"native_longlink"}}),
        )?;
        let mut next_heartbeat = Instant::now() + heartbeat_interval(&heartbeat);
        loop {
            self.push_state(account, &fence)?;
            if Instant::now() >= next_heartbeat {
                let guard = self.lock_account(account).await?;
                let core = self.clone();
                let owned = account.to_owned();
                let active = fence.clone();
                let result = blocking(move || {
                    let _guard = guard;
                    let mut state = core.push_state(&owned, &active)?;
                    let expected = core
                        .store
                        .runtime(&owned, "account-state")?
                        .context("auth_required")?;
                    let beat = connection.heartbeat(&mut state, &core.cursor(&owned)?)?;
                    core.save_push_state(&owned, &state, &expected, &active)?;
                    Ok((connection, beat))
                })
                .await?;
                connection = result.0;
                next_heartbeat = Instant::now() + heartbeat_interval(&result.1);
                if result.1.selector & 7 != 0 {
                    connection = self
                        .drain_push_sync(account, connection, &fence, 7, "heartbeat")
                        .await?;
                }
            }
            let result = blocking(move || {
                let notification = connection.poll(Duration::from_secs(1))?;
                Ok((connection, notification))
            })
            .await?;
            connection = result.0;
            if let Some(notification) = result.1 {
                // Recheck after waiting; an old socket cannot act after logout,
                // disconnect or replacement of its authenticated key generation.
                self.push_state(account, &fence)?;
                self.store.append_event(account,"account.push",&json!({"state":"notified","command":notification.command,"selector":notification.selector}))?;
                connection = self
                    .drain_push_sync(account, connection, &fence, 1, "native_push")
                    .await?;
            }
        }
    }
    async fn drain_push_sync(
        self: &Arc<Self>,
        account: &str,
        mut connection: Box<dyn crate::upstream::PushConnection>,
        fence: &SessionFence,
        scene: u32,
        trigger: &str,
    ) -> Result<Box<dyn crate::upstream::PushConnection>> {
        for _ in 0..8 {
            let guard = self.lock_account(account).await?;
            let core = self.clone();
            let owned = account.to_owned();
            let active = fence.clone();
            let trigger = trigger.to_owned();
            let (next, continuation) = blocking(move || {
                let _guard = guard;
                let mut state = core.push_state(&owned, &active)?;
                let expected = core
                    .store
                    .runtime(&owned, "account-state")?
                    .context("auth_required")?;
                let batch = connection.sync(&mut state, &core.cursor(&owned)?, scene)?;
                core.save_push_state(&owned, &state, &expected, &active)?;
                let continuation = batch.continuation;
                core.commit_sync_batch(&owned, batch, "longlink", &trigger)?;
                Ok((connection, continuation))
            })
            .await?;
            connection = next;
            if !continuation {
                return Ok(connection);
            }
        }
        // Never leave a truncated catch-up falsely classified as live. The
        // fallback/reconnect resumes from the already durable received cursor.
        bail!("Native push sync page limit")
    }
    pub(super) fn commit_sync_batch(
        &self,
        account: &str,
        batch: SyncBatch,
        transport: &str,
        trigger: &str,
    ) -> Result<Value> {
        let origin = if self.store.runtime(account, "history-caught-up")?.is_some() {
            "live"
        } else {
            "catchup"
        };
        let raw = serde_json::to_vec(&batch)?;
        let id = format!("{:x}", Sha256::digest(&raw));
        self.store.stage_sync(
            account,
            &id,
            &self.vault.seal(account, &format!("apply:{id}"), &raw)?,
            &self.vault.seal(account, "received-cursor", &batch.cursor)?,
        )?;
        self.store
            .put_runtime(account, &format!("apply-origin:{id}"), origin.as_bytes())?;
        let projection = self.project_pending_with_media(account, true)?;
        if !batch.continuation {
            self.store.put_runtime(account, "history-caught-up", b"1")?;
        }
        self.store.append_event(account,"sync.received",&json!({"batchId":id,"commandCount":batch.commands.len(),"profileProjection":"pending","continuation":batch.continuation,"transport":transport,"trigger":trigger}))?;
        Ok(
            json!({"batchId":id,"commandCount":batch.commands.len(),"continuation":batch.continuation,"projection":projection,"profileProjection":if self.store.pending_sync_count(account)?==0{"complete"}else{"pending"},"checkpointProjection":if self.store.pending_cursor_projection(account)?{"pending"}else{"complete"}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookie_rotation_keeps_socket_but_key_routing_or_identity_changes_do_not() {
        let original = super::super::tests::state();
        let mut changed = original.clone();
        changed.cookie = vec![1, 2, 3];
        assert_eq!(
            authorization(&original).unwrap(),
            authorization(&changed).unwrap()
        );
        for which in 0..5 {
            let mut changed = original.clone();
            match which {
                0 => changed.request_key[0] ^= 1,
                1 => changed.uin += 1,
                2 => changed.device.device_id[0] ^= 1,
                3 => changed.long_host = "sglong.wechat.com".into(),
                _ => changed.auto_auth_key = vec![9; 16],
            }
            assert_ne!(
                authorization(&original).unwrap(),
                authorization(&changed).unwrap()
            );
        }
    }
    #[test]
    fn push_admission_and_notifications_cannot_cross_disconnect_logout_or_renewal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&tmp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store.clone(), None).unwrap();
        let state = super::super::tests::state();
        core.save_state("one", &state).unwrap();
        store.put_runtime("one", "desired", b"connected").unwrap();
        let fence = SessionFence {
            authorization: authorization(&state).unwrap(),
            logout_epoch: None,
        };
        assert!(core.push_state("one", &fence).is_err());
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        assert!(core.push_state("one", &fence).is_ok());
        core.disconnect("one").unwrap();
        assert!(core.push_state("one", &fence).is_err());
        store.put_runtime("one", "desired", b"connected").unwrap();
        store
            .mark_validated_if_active("one", &core.ownership_generation)
            .unwrap();
        let mut renewed = state.clone();
        renewed.request_key[0] ^= 1;
        core.save_state("one", &renewed).unwrap();
        assert!(core.push_state("one", &fence).is_err());
        core.save_state("one", &state).unwrap();
        store.put_runtime("one", "logout-epoch", b"new").unwrap();
        assert!(core.push_state("one", &fence).is_err());
        assert_eq!(
            heartbeat_interval(&Heartbeat {
                next_interval: 149,
                selector: 6
            }),
            Duration::from_secs(149)
        );
    }
}
