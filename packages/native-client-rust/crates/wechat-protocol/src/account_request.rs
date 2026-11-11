//! Scoped operations for embedders that already execute on blocking workers.
use super::*;

/// A single logical operation borrowing this client's temporary request state.
/// It is not an account owner and cannot outlive request_blocking. No operation
/// accepts an independently mutable AccountState. Use ordinary async client
/// methods unless the embedding application already owns a blocking worker.
pub struct AccountRequest<'a> {
    transport: &'a NativeClient,
    state: &'a mut AccountState,
    spent: bool,
}

macro_rules! operation {
    ($name:ident($($arg:ident: $ty:ty),*) -> $result:ty => $raw:ident) => {
        pub fn $name(&mut self, $($arg: $ty),*) -> Result<$result> {
            self.admit()?;
            self.transport.$raw(self.state, $($arg),*)
        }
    };
}

impl AccountRequest<'_> {
    fn admit(&mut self) -> Result<()> {
        // A second operation must not label the first possibly accepted one
        // "not submitted". Each logical request has its own outcome/journal.
        ensure!(!self.spent, "Account request context already consumed");
        self.spent = true;
        Ok(())
    }

    /// Read-only request context for protocol preparation and journal binding.
    /// These secret bytes are not an independent account owner or a handoff.
    pub fn account(&self) -> &AccountState {
        self.state
    }

    /// Read one exact OpenIM contact using optional group context and a supplied ticket.
    pub fn lookup_openim_contact(
        &mut self,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<crate::OpenImContact> {
        self.admit()?;
        self.transport
            .openim_contact_snapshot(self.state, username, room, ticket)
    }

    /// Return the authenticated OpenIM reply bytes for caller-owned receipt storage and decoding.
    pub fn lookup_openim_contact_raw(
        &mut self,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<Vec<u8>> {
        self.admit()?;
        self.transport
            .openim_contact_raw_snapshot(self.state, username, room, ticket)
    }

    operation!(sync(cursor: &[u8]) -> crate::SyncBatch => sync_snapshot);
    operation!(sync_scene(cursor: &[u8], scene: u32) -> crate::SyncBatch => sync_scene_snapshot);
    operation!(heartbeat(cursor: &[u8]) -> crate::Heartbeat => heartbeat_snapshot);
    operation!(lookup_contacts(ids: &[String]) -> crate::ContactLookup => lookup_contacts_snapshot);
    operation!(lookup_room(room: &str, version: u32) -> crate::RoomRoster => lookup_room_snapshot);
    operation!(init_contacts(contact_sequence: u32, room_sequence: u32) -> crate::InitContactPage => init_contacts_snapshot);
    operation!(download_avatars(ids: &[String]) -> Vec<crate::AvatarBytes> => download_avatars_snapshot);
    operation!(download_voice_chunk(server_id: u64, length: u32, offset: u32) -> crate::VoiceChunk => download_voice_chunk_snapshot);
    operation!(cdn_config() -> crate::media::CdnConfig => cdn_config_snapshot);
    operation!(large_cdn_config(route: crate::media::LargeCdnRoute) -> crate::media::CdnConfig => large_cdn_config_snapshot);
    operation!(group_inventory(cursor: &[u8]) -> crate::GroupMetadataPage => group_inventory_snapshot);
    operation!(lookup_contacts_raw(ids: &[String]) -> Vec<u8> => lookup_contacts_raw_snapshot);
    operation!(lookup_contacts_receipt(ids: &[String]) -> crate::MetadataReply<crate::ContactLookup> => lookup_contacts_receipt_snapshot);
    operation!(lookup_room_raw(room: &str, version: u32) -> Vec<u8> => lookup_room_raw_snapshot);
    operation!(lookup_room_receipt(room: &str, version: u32) -> crate::MetadataReply<crate::RoomRoster> => lookup_room_receipt_snapshot);
    operation!(init_contacts_receipt(contacts: u32, rooms: u32) -> crate::MetadataReply<crate::InitContactPage> => init_contacts_receipt_snapshot);
    operation!(group_detail_raw(group: &str) -> Vec<u8> => group_detail_raw_snapshot);
    operation!(group_mutation_raw(request: &crate::GroupRpc) -> Vec<u8> => group_mutation_raw_snapshot);
    operation!(recall_message(request: &crate::MessageRecall) -> crate::MessageRecallReply => recall_message_snapshot);
    operation!(notify_read_session(request: &crate::SessionReadNotification) -> crate::SessionReadReply => notify_read_session_snapshot);
    operation!(contact_sync(cursor: &[u8], message_cursor: &[u8], continuation: bool) -> crate::contact_sync::ContactSyncBatch => contact_sync_snapshot);
    operation!(send_share(destination: &str, client_id: &str, created_at: u32, content: &str) -> crate::media_send::MediaSendResult => send_share_snapshot);
    operation!(send_quote(destination: &str, client_id: &str, created_at: u32, content: &str, source: Option<&str>) -> crate::media_send::MediaSendResult => send_quote_with_source_snapshot);
    operation!(reserve_file_upload(destination: &str, client_id: &str, name: &str, length: usize, md5: &str) -> crate::file_upload::FileUploadReservation => reserve_file_upload_snapshot);
    operation!(send_file(destination: &str, client_id: &str, created_at: u32, upload: &crate::media::UploadedMedia, reservation: Option<&crate::file_upload::FileUploadReservation>) -> crate::media_send::MediaSendResult => send_file_with_reservation_snapshot);
    operation!(send_image(destination: &str, client_id: &str, upload: &crate::media::UploadedImage) -> crate::media_send::MediaSendResult => send_image_snapshot);
    operation!(send_video(destination: &str, client_id: &str, upload: &crate::media::UploadedVideo, seconds: u32) -> crate::media_send::MediaSendResult => send_video_snapshot);
    operation!(send_voice(destination: &str, client_id: &str, created_at: u32, duration_ms: u32, encoded: &[u8]) -> crate::media_send::MediaSendResult => send_voice_snapshot);

    pub fn send_text(
        &mut self,
        destination: &str,
        content: &str,
        options: &TextSendOptions,
    ) -> Result<crate::TextSendResult> {
        self.send_text_guarded(destination, content, options, || Ok(()))
    }

    pub fn send_text_guarded(
        &mut self,
        destination: &str,
        content: &str,
        options: &TextSendOptions,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<crate::TextSendResult> {
        self.admit()?;
        let request = crate::wire::text_send_request_with_source(
            destination,
            content,
            options.created_at,
            options.client_message_id,
            options.source.as_deref(),
        )?;
        let reply = self.transport.authenticated_rpc_guarded(
            self.state,
            522,
            "/cgi-bin/micromsg-bin/newsendmsg",
            &request,
            before_submit,
        )?;
        crate::wire::decode_text_send(&reply, destination, options.client_message_id)
    }

    pub fn call_rpc(
        &mut self,
        route: crate::calls::host_policy::CallHostRoute,
        request: &[u8],
        before_submit: impl FnOnce() -> Result<()>,
        retain_reply: impl FnOnce(&[u8]) -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.admit()?;
        let cookie = Zeroizing::new(self.state.cookie.clone());
        let reply =
            self.transport
                .call_rpc_snapshot_guarded(self.state, route, request, before_submit)?;
        if let Err(error) = retain_reply(&reply) {
            // Retention failure cannot publish this request's new cookie. A
            // concurrent accepted account update is still protected by revision.
            zeroize::Zeroize::zeroize(&mut self.state.cookie);
            self.state.cookie = cookie.to_vec();
            return Err(error);
        }
        Ok(reply)
    }

    /// Read-only call polling on an existing blocking worker. The caller owns
    /// the async wait/cancellation mechanism; this context retains the account
    /// lease until that transport future is dropped or completes.
    pub fn call_live_sync(
        &mut self,
        request: &[u8],
        wait_timeout: Duration,
        before_submit: impl FnOnce() -> Result<()>,
        wait: impl for<'a> FnOnce(
            std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>,
        ) -> Result<Vec<u8>>,
        retain_reply: impl FnOnce(&[u8]) -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.admit()?;
        let cookie = Zeroizing::new(self.state.cookie.clone());
        let reply = self.transport.call_live_sync_snapshot_with_wait_timeout(
            self.state,
            request,
            wait_timeout,
            before_submit,
            wait,
        )?;
        if let Err(error) = retain_reply(&reply) {
            zeroize::Zeroize::zeroize(&mut self.state.cookie);
            self.state.cookie = cookie.to_vec();
            return Err(error);
        }
        Ok(reply)
    }
}

impl NativeClient {
    /// Change transport deadlines/budgets for a handle while retaining the same
    /// account owner and admission rules. This never imports credential state.
    pub fn with_request_options(&self, options: NativeOptions) -> Result<Self> {
        // Reuse constructor validation without replacing the account owner.
        let mut handle = Self::new(options)?;
        handle.account = Some(self.owned_account()?.clone());
        Ok(handle)
    }

    /// Execute one scoped account operation on the caller's blocking worker.
    /// Uses the same admission, generation, storage and handoff barriers as async
    /// operations. No executor or independent credential owner is constructed.
    pub fn request_blocking<T>(
        &self,
        operation: impl FnOnce(&mut AccountRequest<'_>) -> Result<T>,
    ) -> Result<T> {
        self.account_request_blocking(|transport, state, _| {
            operation(&mut AccountRequest {
                transport,
                state,
                spent: false,
            })
        })
    }

    /// Explicit authentication renewal on an existing blocking worker. This
    /// never retries the operation whose rejection prompted the caller to renew.
    pub fn renew_session_blocking(&self, computer_name: &str) -> Result<()> {
        let (exchange, state) = self
            .owned_account()?
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let exchange = Arc::new(exchange);
        let mut transport = Self::new(self.options.clone())?;
        let submission = exchange.clone();
        transport.submission_guard = Some(Arc::new(move || {
            submission
                .check()
                .map_err(|error| error.context(NativeRequestNotSubmitted))
        }));
        let accepted = transport.renew_session_snapshot(&state, computer_name)?;
        exchange.accept_credentials(accepted)
    }

    /// As [`Self::logout`], on the caller's blocking worker. No Tokio runtime
    /// is needed. Logout is submitted at most once and the owner stays stopped
    /// after rejection or uncertainty.
    pub fn logout_blocking(&self) -> Result<crate::LogoutReply> {
        self.prepare_logout()?.submit_blocking()
    }

    /// As [`Self::shutdown`], on the caller's blocking worker. Do not call it
    /// from inside an account request: it waits for every request to finish.
    pub fn shutdown_blocking(&self) -> Result<()> {
        self.owned_account()?.stop_and_drain_blocking()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_requests_share_owner_and_cannot_submit_after_stop() {
        let client = super::super::tests::client();
        let other = client
            .with_request_options(NativeOptions::default())
            .unwrap();
        other
            .request_blocking(|request| {
                assert_eq!(request.account().uin, 1);
                request.admit()?;
                request.state.cookie = vec![4];
                Ok(())
            })
            .unwrap();
        assert_eq!(client.export_account_state().unwrap().cookie, vec![4]);
        client.stop().unwrap();
        let entered = AtomicBool::new(false);
        let error = other
            .request_blocking(|_| {
                entered.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap_err();
        assert!(error.is::<NativeRequestNotSubmitted>());
        assert!(!entered.load(Ordering::Acquire));
    }

    #[test]
    fn scoped_context_cannot_turn_a_second_operation_into_an_unsent_retry() {
        let client = super::super::tests::client();
        let error = client
            .request_blocking(|request| {
                request.admit()?;
                request.state.cookie = vec![7];
                request.admit()
            })
            .unwrap_err();
        assert!(!error.is::<NativeRequestNotSubmitted>());
        assert_eq!(client.export_account_state().unwrap().cookie, vec![7]);
        assert_eq!(client.account_status().unwrap().in_flight, 0);
    }

    #[tokio::test]
    async fn blocking_workers_and_async_requests_share_one_response_order() {
        let client = super::super::tests::client();
        let old = client.clone();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let (finish, finish_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            old.request_blocking(|request| {
                request.admit()?;
                ready.send(()).unwrap();
                finish_rx.recv()?;
                request.state.cookie = vec![1];
                Ok(())
            })
        });
        ready_rx.await.unwrap();
        client
            .account_request(|_, state, _| {
                state.cookie = vec![2];
                Ok(())
            })
            .await
            .unwrap();
        finish.send(()).unwrap();
        worker.await.unwrap().unwrap();
        assert_eq!(client.export_account_state().unwrap().cookie, vec![2]);
    }
}
