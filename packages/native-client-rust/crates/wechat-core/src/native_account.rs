//! Core keeps persistence and policy; NativeClient is the sole live account owner.
use super::*;
use wechat_protocol::{AccountPersistence, AccountRequest, NativeClient};

pub(super) struct BoundUpstream {
    client: NativeClient,
}

struct PersistAccount {
    store: Arc<Store>,
    shared: std::sync::Weak<CoreShared>,
    account: String,
    epoch: Option<Vec<u8>>,
    desired: Option<Vec<u8>>,
}

impl AccountPersistence for PersistAccount {
    fn persist(&self, previous: &AccountState, accepted: &AccountState) -> Result<()> {
        let shared = self.shared.upgrade().context("account runtime stopped")?;
        anyhow::ensure!(
            !shared.stopping.load(Ordering::Acquire),
            "account runtime stopped"
        );
        // This view never reads/reenters the native account registry or owner.
        let core = Core {
            store: self.store.clone(),
            shared,
            snapshot_reader: None,
            live_store: None,
        };
        let expected = self
            .store
            .runtime(&self.account, "account-state")?
            .context("auth_required")?;
        let stored = core
            .vault
            .open_record(&self.account, "account-state", &expected)?;
        let previous = zeroize::Zeroizing::new(serde_json::to_vec(previous)?);
        anyhow::ensure!(
            stored.as_slice() == previous.as_slice(),
            "session_superseded"
        );
        core.check_state_profile(&self.account, accepted)?;
        core.prepare_call_authorization(&self.account, accepted)?;
        let sealed = core.vault.seal(
            &self.account,
            "account-state",
            &serde_json::to_vec(accepted)?,
        )?;
        // CAS includes the admitted ciphertext, logout epoch, accepted generation
        // and connection mode. Nothing that can fail runs after this commit.
        self.store.save_network_authorization(
            &self.account,
            &sealed,
            &expected,
            self.epoch.as_deref(),
            self.desired.as_deref(),
        )
    }
}

impl Core {
    pub(crate) fn native_account(
        &self,
        account: &str,
        options: NativeOptions,
    ) -> Result<NativeClient> {
        anyhow::ensure!(
            self.upstream.uses_account_owner(),
            "unsupported_simulator_operation"
        );
        let mut owners = self
            .native_accounts
            .lock()
            .map_err(|_| anyhow::anyhow!("account manager unavailable"))?;
        anyhow::ensure!(
            !self.stopping.load(Ordering::Acquire),
            "account runtime stopped"
        );
        if let Some(owner) = owners.get(account) {
            return owner
                .as_ref()
                .context("account runtime stopped")?
                .with_request_options(options);
        }
        anyhow::ensure!(
            self.store.runtime(account, "logged-out")?.is_none(),
            "auth_required"
        );
        let state = self.state_unfenced(account)?;
        self.check_state_profile(account, &state)?;
        let persistence = Arc::new(PersistAccount {
            store: self.store.clone(),
            shared: Arc::downgrade(&self.shared),
            account: account.into(),
            epoch: self.store.runtime(account, "logout-epoch")?,
            desired: self.store.runtime(account, "desired")?,
        });
        let client = NativeClient::from_account_with_persistence(
            state,
            NativeOptions::default(),
            persistence,
        )?;
        owners.insert(account.into(), Some(client.clone()));
        client.with_request_options(options)
    }

    pub(crate) fn account_upstream(
        &self,
        account: &str,
    ) -> Result<Arc<dyn crate::upstream::Upstream>> {
        if !self.upstream.uses_account_owner() {
            return Ok(self.upstream.clone());
        }
        Ok(Arc::new(BoundUpstream {
            client: self.native_account(account, NativeOptions::default())?,
        }))
    }

    /// Keep a stopped tombstone until an explicit login/replacement accepts a
    /// new generation. Lazy lookup must never reopen an account after a fence.
    pub(crate) fn stop_native_account(&self, account: &str) {
        if !self.upstream.uses_account_owner() {
            return;
        }
        if let Ok(mut owners) = self.native_accounts.lock() {
            if let Some(Some(client)) = owners.insert(account.into(), None) {
                let _ = client.stop();
            }
        }
    }

    pub(crate) fn reset_native_account(&self, account: &str) {
        if let Ok(mut owners) = self.native_accounts.lock() {
            if let Some(Some(client)) = owners.remove(account) {
                let _ = client.stop();
            }
        }
    }

    pub(super) fn stop_native_accounts(&self) {
        if let Ok(mut owners) = self.native_accounts.lock() {
            for owner in owners.values_mut() {
                if let Some(client) = owner.take() {
                    let _ = client.stop();
                }
            }
        }
    }

    pub(crate) fn native_account_lease(
        &self,
        account: &str,
    ) -> Result<Option<wechat_protocol::AccountLease>> {
        let owners = self
            .native_accounts
            .lock()
            .map_err(|_| anyhow::anyhow!("account manager unavailable"))?;
        match owners.get(account) {
            Some(Some(client)) => Ok(Some(client.account_lease()?)),
            Some(None) => bail!("account runtime stopped"),
            None => Ok(None),
        }
    }

    pub(super) fn native_clients(&self) -> Vec<NativeClient> {
        self.native_accounts
            .lock()
            .map(|owners| owners.values().filter_map(Clone::clone).collect())
            .unwrap_or_default()
    }

    pub(super) fn native_account_snapshot(&self, account: &str) -> Result<Option<AccountState>> {
        let owners = self
            .native_accounts
            .lock()
            .map_err(|_| anyhow::anyhow!("account manager unavailable"))?;
        owners
            .get(account)
            .and_then(Option::as_ref)
            .map(NativeClient::export_account_state)
            .transpose()
    }

    /// Core's post-response journal/projection phase must not write a stale
    /// cookie over a later accepted response. NativeClient already committed it.
    pub(super) fn persisted_native_state(
        &self,
        account: &str,
        returned: &AccountState,
    ) -> Result<Option<AccountState>> {
        {
            let owners = self
                .native_accounts
                .lock()
                .map_err(|_| anyhow::anyhow!("account manager unavailable"))?;
            if let Some(Some(client)) = owners.get(account) {
                anyhow::ensure!(
                    client.account_status()?.lifecycle == wechat_protocol::AccountLifecycle::Active,
                    "session_superseded"
                );
            }
        }
        let Some(current) = self.native_account_snapshot(account)? else {
            return Ok(None);
        };
        let stored = self.state_unfenced(account)?;
        let current_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&current)?);
        let stored_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&stored)?);
        anyhow::ensure!(
            current_bytes.as_slice() == stored_bytes.as_slice(),
            "session_superseded"
        );
        let mut current_without_cookie = current.clone();
        let mut returned = returned.clone();
        current_without_cookie.cookie.fill(0);
        current_without_cookie.cookie.clear();
        returned.cookie.fill(0);
        returned.cookie.clear();
        let current_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&current_without_cookie)?);
        let returned_bytes = zeroize::Zeroizing::new(serde_json::to_vec(&returned)?);
        anyhow::ensure!(
            current_bytes.as_slice() == returned_bytes.as_slice(),
            "session_superseded"
        );
        Ok(Some(current))
    }

    pub(crate) fn native_request<T>(
        &self,
        account: &str,
        snapshot: &mut AccountState,
        operation: impl FnOnce(&mut AccountRequest<'_>) -> Result<T>,
    ) -> Result<T> {
        mirror(
            &self.native_account(account, NativeOptions::default())?,
            snapshot,
            operation,
        )
    }
}

fn mirror<T>(
    client: &NativeClient,
    snapshot: &mut AccountState,
    operation: impl FnOnce(&mut AccountRequest<'_>) -> Result<T>,
) -> Result<T> {
    // Preserve verified operation receipts independently from credential
    // installation. A local fence cannot turn an accepted send into an unknown
    // send. Core still checks its own fence before journal/profile completion.
    let mut response = None;
    let committed = client.request_blocking(|request| {
        response = Some(operation(request));
        Ok(())
    });
    if committed
        .as_ref()
        .is_err_and(|error| error.is::<wechat_protocol::AccountPersistenceFailed>())
    {
        // Fail admission closed on storage failure, without renewing or retrying.
        client.stop()?;
    }
    if let Ok(accepted) = client.export_account_state() {
        *snapshot = accepted;
    }
    match response {
        Some(response) => response,
        None => {
            committed?;
            bail!("Account request produced no outcome")
        }
    }
}

macro_rules! forward {
    ($name:ident($($arg:ident: $ty:ty),*) -> $result:ty => $method:ident) => {
        fn $name(&self, state: &mut AccountState, $($arg: $ty),*) -> Result<$result> {
            mirror(&self.client, state, |request| request.$method($($arg),*))
        }
    };
}

impl crate::upstream::Upstream for BoundUpstream {
    fn kind(&self) -> crate::upstream::UpstreamKind {
        crate::upstream::UpstreamKind::Native
    }
    fn native(&self, options: NativeOptions) -> Result<NativeClient> {
        self.client.with_request_options(options)
    }
    forward!(sync(cursor: &[u8]) -> wechat_protocol::SyncBatch => sync);
    forward!(heartbeat(cursor: &[u8]) -> wechat_protocol::Heartbeat => heartbeat);
    forward!(group_mutation_raw(request: &wechat_protocol::GroupRpc) -> Vec<u8> => group_mutation_raw);
    forward!(group_detail_raw(room: &str) -> Vec<u8> => group_detail_raw);
    forward!(lookup_room_raw(room: &str, version: u32) -> Vec<u8> => lookup_room_raw);
    forward!(lookup_room_receipt(room: &str, version: u32) -> wechat_protocol::MetadataReply<wechat_protocol::RoomRoster> => lookup_room_receipt);
    forward!(lookup_contacts_raw(ids: &[String]) -> Vec<u8> => lookup_contacts_raw);
    forward!(lookup_openim_contact_raw(username: &str, room: Option<&str>, ticket: Option<&str>) -> Vec<u8> => lookup_openim_contact_raw);
    forward!(contact_sync(cursor: &[u8], message_cursor: &[u8], continuation: bool) -> wechat_protocol::contact_sync::ContactSyncBatch => contact_sync);
    forward!(download_voice(server_id: u64, length: u32, offset: u32) -> wechat_protocol::VoiceChunk => download_voice_chunk);
    forward!(notify_read(request: &wechat_protocol::SessionReadNotification) -> wechat_protocol::SessionReadReply => notify_read_session);
    forward!(send_share(destination: &str, client_id: &str, created_at: u32, content: &str) -> wechat_protocol::media_send::MediaSendResult => send_share);
    forward!(send_quote(destination: &str, client_id: &str, created_at: u32, content: &str, source: Option<&str>) -> wechat_protocol::media_send::MediaSendResult => send_quote);

    fn send_text(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
    ) -> Result<wechat_protocol::TextSendResult> {
        self.send_text_guarded(
            state,
            destination,
            content,
            created_at,
            client_message_id,
            &mut || Ok(()),
        )
    }
    fn send_text_with_source(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
        source: &str,
    ) -> Result<wechat_protocol::TextSendResult> {
        mirror(&self.client, state, |request| {
            request.send_text(
                destination,
                content,
                &wechat_protocol::TextSendOptions {
                    created_at,
                    client_message_id,
                    source: Some(source.into()),
                },
            )
        })
    }
    fn send_text_guarded(
        &self,
        state: &mut AccountState,
        destination: &str,
        content: &str,
        created_at: u32,
        client_message_id: u32,
        before_submit: &mut dyn FnMut() -> Result<()>,
    ) -> Result<wechat_protocol::TextSendResult> {
        mirror(&self.client, state, |request| {
            request.send_text_guarded(
                destination,
                content,
                &wechat_protocol::TextSendOptions {
                    created_at,
                    client_message_id,
                    source: None,
                },
                before_submit,
            )
        })
    }
    fn renew_session(&self, _state: &AccountState, computer_name: &str) -> Result<AccountState> {
        if let Err(error) = self.client.renew_session_blocking(computer_name) {
            if error.is::<wechat_protocol::AccountPersistenceFailed>() {
                self.client.stop()?;
            }
            return Err(error);
        }
        self.client.export_account_state()
    }
    fn media_transport_route(
        &self,
        state: &mut AccountState,
        route: Option<wechat_protocol::media::LargeCdnRoute>,
    ) -> Result<Box<dyn crate::upstream::media::MediaTransport>> {
        let load = |state: &mut AccountState| {
            mirror(&self.client, state, |request| match route {
                Some(route) => request.large_cdn_config(route),
                None => request.cdn_config(),
            })
        };
        let mut configuration = match load(state) {
            Ok(value) => value,
            Err(error) if crate::media::is_media_auth_rejected(&error) => {
                *state = self.renew_session(state, "agent-wechat media")?;
                load(state)?
            }
            Err(error) => return Err(error),
        };
        let transport =
            wechat_protocol::media::CdnClient::new(configuration.clone(), Duration::from_secs(25))?;
        configuration.auth_key.fill(0);
        Ok(Box::new(transport))
    }
    fn submit_media(
        &self,
        state: &mut AccountState,
        input: crate::upstream::media::MediaSubmission<'_>,
    ) -> Result<wechat_protocol::media_send::MediaSendResult> {
        use crate::upstream::media::MediaBody;
        mirror(&self.client, state, |request| match input.body {
            MediaBody::File {
                upload,
                reservation,
            } => request.send_file(
                input.destination,
                input.client_id,
                input.created_at,
                upload,
                reservation,
            ),
            MediaBody::Image(upload) => {
                request.send_image(input.destination, input.client_id, upload)
            }
            MediaBody::Video { upload, seconds } => {
                request.send_video(input.destination, input.client_id, upload, seconds)
            }
            MediaBody::Voice {
                encoded,
                duration_ms,
            } => request.send_voice(
                input.destination,
                input.client_id,
                input.created_at,
                duration_ms,
                encoded,
            ),
        })
    }
    fn push_connection(
        &self,
        _state: &AccountState,
    ) -> Result<Box<dyn crate::upstream::PushConnection>> {
        Ok(Box::new(BoundPush {
            client: self.client.clone(),
            connection: self.client.open_push_connection_blocking()?,
        }))
    }
}

struct BoundPush {
    client: NativeClient,
    connection: wechat_protocol::AccountPushConnection,
}
impl crate::upstream::PushConnection for BoundPush {
    fn heartbeat(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
    ) -> Result<wechat_protocol::Heartbeat> {
        let reply = self.connection.heartbeat(cursor);
        if reply
            .as_ref()
            .is_err_and(|error| error.is::<wechat_protocol::AccountPersistenceFailed>())
        {
            self.client.stop()?;
        }
        *state = self.client.export_account_state()?;
        reply
    }
    fn sync(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        scene: u32,
    ) -> Result<wechat_protocol::SyncBatch> {
        let reply = self.connection.sync(cursor, scene);
        if reply
            .as_ref()
            .is_err_and(|error| error.is::<wechat_protocol::AccountPersistenceFailed>())
        {
            self.client.stop()?;
        }
        *state = self.client.export_account_state()?;
        reply
    }
    fn poll(&mut self, timeout: Duration) -> Result<Option<wechat_protocol::push::Notification>> {
        self.connection.poll(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::AccountLifecycle;

    fn fixture() -> (tempfile::TempDir, Arc<Core>) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
        for account in ["one", "two"] {
            store.create_account(account).unwrap();
        }
        let core = Core::new(store, None).unwrap();
        // Equal protocol identities must still have separate Core account slots.
        for account in ["one", "two"] {
            core.save_state(account, &super::super::tests::state())
                .unwrap();
            core.store
                .put_runtime(account, "desired", b"connected")
                .unwrap();
            core.store
                .mark_validated_if_active(account, &core.ownership_generation)
                .unwrap();
            core.bind_call_authorization(account, &core.account_state(account).unwrap())
                .unwrap();
        }
        (directory, core)
    }

    fn persistence(core: &Core, account: &str) -> PersistAccount {
        PersistAccount {
            store: core.store.clone(),
            shared: Arc::downgrade(&core.shared),
            account: account.into(),
            epoch: core.store.runtime(account, "logout-epoch").unwrap(),
            desired: core.store.runtime(account, "desired").unwrap(),
        }
    }

    #[test]
    fn lookup_shares_one_owner_and_fencing_does_not_reopen_it_or_stop_another_account() {
        let (_directory, core) = fixture();
        let first = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let second = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let other = core
            .native_account("two", NativeOptions::default())
            .unwrap();
        first.stop().unwrap();
        assert_eq!(
            second.account_status().unwrap().lifecycle,
            AccountLifecycle::Stopped
        );
        assert_eq!(
            other.account_status().unwrap().lifecycle,
            AccountLifecycle::Active
        );
        core.fence_call_authority("one");
        assert!(core
            .native_account("one", NativeOptions::default())
            .is_err());
        assert!(core.account_upstream("one").is_err());
        assert_eq!(
            other.account_status().unwrap().lifecycle,
            AccountLifecycle::Active
        );
    }

    #[test]
    fn accepted_login_replaces_the_owner_without_reactivating_old_handles() {
        let (_directory, core) = fixture();
        let old = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        core.logout_local("one").unwrap();
        let epoch = core.store.runtime("one", "logout-epoch").unwrap().unwrap();
        let mut replacement = super::super::tests::state();
        replacement.request_key[0] ^= 1;
        core.install_login_state("one", &replacement, Some(&epoch))
            .unwrap();
        let current = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        assert_eq!(
            old.account_status().unwrap().lifecycle,
            AccountLifecycle::Stopped
        );
        assert_eq!(
            current.account_status().unwrap().lifecycle,
            AccountLifecycle::Active
        );
        assert_eq!(
            current.export_account_state().unwrap().request_key,
            replacement.request_key
        );
        assert_ne!(
            old.export_account_state().unwrap().request_key,
            replacement.request_key
        );
    }

    #[test]
    fn persistence_uses_the_exact_previous_state_and_rejects_stale_updates() {
        let (_directory, core) = fixture();
        let persistence = persistence(&core, "one");
        let initial = core.account_state("one").unwrap();
        let mut first = initial.clone();
        first.cookie = vec![1];
        persistence.persist(&initial, &first).unwrap();
        let mut stale = initial.clone();
        stale.cookie = vec![9];
        assert!(persistence.persist(&initial, &stale).is_err());
        assert_eq!(core.account_state("one").unwrap().cookie, vec![1]);
        let mut next = first.clone();
        next.cookie = vec![2];
        persistence.persist(&first, &next).unwrap();
        assert_eq!(core.account_state("one").unwrap().cookie, vec![2]);
    }

    #[test]
    fn persistence_failure_after_logout_or_mode_change_cannot_publish_a_cookie() {
        for logout in [false, true] {
            let (_directory, core) = fixture();
            let persistence = persistence(&core, "one");
            let initial = core.account_state("one").unwrap();
            let before = core.store.runtime("one", "account-state").unwrap();
            if logout {
                core.logout_local("one").unwrap();
            } else {
                core.disconnect("one").unwrap();
            }
            let mut returned = initial.clone();
            returned.cookie = vec![8];
            assert!(persistence.persist(&initial, &returned).is_err());
            assert_eq!(core.store.runtime("one", "account-state").unwrap(), before);
        }
    }

    #[test]
    fn projection_acknowledgement_cannot_replace_owned_credentials_or_hide_external_storage_changes(
    ) {
        let (_directory, core) = fixture();
        let client = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let initial = client.export_account_state().unwrap();
        let mut fabricated = initial.clone();
        fabricated.request_key[0] ^= 1;
        assert!(core.persisted_native_state("one", &fabricated).is_err());
        let mut changed = initial.clone();
        changed.cookie = vec![3];
        persistence(&core, "one")
            .persist(&initial, &changed)
            .unwrap();
        assert!(core.persisted_native_state("one", &initial).is_err());
        assert!(client.export_account_state().unwrap().cookie.is_empty());
        assert_eq!(core.state_unfenced("one").unwrap().cookie, vec![3]);
    }

    #[test]
    fn a_verified_operation_receipt_survives_later_account_inhibition() {
        let (_directory, core) = fixture();
        let client = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let mut snapshot = client.export_account_state().unwrap();
        let stop = client.clone();
        let receipt = mirror(&client, &mut snapshot, |_| {
            // The operation has decoded a verified reply; inhibition wins the
            // later account update without erasing this independent receipt.
            stop.stop()?;
            Ok("synthetic-accepted-receipt")
        })
        .unwrap();
        assert_eq!(receipt, "synthetic-accepted-receipt");
        assert!(core.persisted_native_state("one", &snapshot).is_err());
        assert_eq!(client.account_status().unwrap().in_flight, 0);
    }

    #[test]
    fn native_shutdown_revokes_media_authority_and_online_status_for_its_account() {
        let (_directory, core) = fixture();
        let client = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let media = core.call_authority("one").unwrap();
        assert!(media.current());
        assert_eq!(core.account_status("one").unwrap()["authenticated"], true);
        client.stop().unwrap();
        assert!(!media.current());
        assert_eq!(core.account_status("one").unwrap()["authenticated"], false);
        assert_eq!(core.account_status("two").unwrap()["authenticated"], true);
    }

    #[tokio::test]
    async fn cancelled_native_logout_commits_local_inhibition_before_any_submission() {
        let (_directory, core) = fixture();
        let client = core
            .native_account("one", NativeOptions::default())
            .unwrap();
        let guard = core.lock_account("one").await.unwrap();
        let operation = core
            .logout("one", "cancel-before-native-handoff")
            .await
            .unwrap();
        assert!(core.store.runtime("one", "logged-out").unwrap().is_some());
        assert_eq!(
            client.account_status().unwrap().lifecycle,
            AccountLifecycle::Stopped
        );
        core.cancel_operation("one", &operation.id).unwrap();
        drop(guard);
        for _ in 0..100 {
            let current = core.store.operation("one", &operation.id).unwrap();
            if current.state == "succeeded" {
                assert_eq!(current.result["remoteOutcome"], "not_attempted");
                assert_eq!(current.result["reason"], "cancelled_before_submission");
                assert_eq!(client.account_status().unwrap().in_flight, 0);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("cancelled native logout did not finish");
    }
}
