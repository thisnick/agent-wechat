//! Account-owned asynchronous protocol operations.
use super::*;

impl NativeClient {
    /// Fetch one sync page using the owned account. The application still owns
    /// command retention, projection and committing the returned cursor.
    pub async fn sync(&self, cursor: &[u8]) -> Result<crate::SyncBatch> {
        let cursor = cursor.to_vec();
        self.account_request(move |transport, state, _| transport.sync_snapshot(state, &cursor))
            .await
    }

    pub async fn heartbeat(&self, cursor: &[u8]) -> Result<crate::Heartbeat> {
        let cursor = cursor.to_vec();
        self.account_request(move |transport, state, _| {
            transport.heartbeat_snapshot(state, &cursor)
        })
        .await
    }

    pub async fn sync_scene(&self, cursor: &[u8], scene: u32) -> Result<crate::SyncBatch> {
        let cursor = cursor.to_vec();
        self.account_request(move |transport, state, _| {
            transport.sync_scene_snapshot(state, &cursor, scene)
        })
        .await
    }

    pub async fn lookup_contacts(&self, usernames: &[String]) -> Result<crate::ContactLookup> {
        let usernames = usernames.to_vec();
        self.account_request(move |transport, state, _| {
            transport.lookup_contacts_snapshot(state, &usernames)
        })
        .await
    }

    pub async fn lookup_room(&self, room_id: &str, version: u32) -> Result<crate::RoomRoster> {
        let room_id = room_id.to_owned();
        self.account_request(move |transport, state, _| {
            transport.lookup_room_snapshot(state, &room_id, version)
        })
        .await
    }

    pub async fn init_contacts(
        &self,
        contact_sequence: u32,
        room_sequence: u32,
    ) -> Result<crate::InitContactPage> {
        self.account_request(move |transport, state, _| {
            transport.init_contacts_snapshot(state, contact_sequence, room_sequence)
        })
        .await
    }

    pub async fn download_avatars(&self, usernames: &[String]) -> Result<Vec<crate::AvatarBytes>> {
        let usernames = usernames.to_vec();
        self.account_request(move |transport, state, _| {
            transport.download_avatars_snapshot(state, &usernames)
        })
        .await
    }

    pub async fn download_voice_chunk(
        &self,
        server_message_id: u64,
        total_length: u32,
        offset: u32,
    ) -> Result<crate::VoiceChunk> {
        self.account_request(move |transport, state, _| {
            transport.download_voice_chunk_snapshot(state, server_message_id, total_length, offset)
        })
        .await
    }

    pub async fn cdn_config(&self) -> Result<crate::media::CdnConfig> {
        self.account_request(|transport, state, _| transport.cdn_config_snapshot(state))
            .await
    }

    pub async fn large_cdn_config(
        &self,
        route: crate::media::LargeCdnRoute,
    ) -> Result<crate::media::CdnConfig> {
        self.account_request(move |transport, state, _| {
            transport.large_cdn_config_snapshot(state, route)
        })
        .await
    }

    pub async fn group_inventory(&self, cursor: &[u8]) -> Result<crate::GroupMetadataPage> {
        let cursor = cursor.to_vec();
        self.account_request(move |transport, state, _| {
            transport.group_inventory_snapshot(state, &cursor)
        })
        .await
    }

    pub async fn lookup_contacts_raw(&self, usernames: &[String]) -> Result<Vec<u8>> {
        let usernames = usernames.to_vec();
        self.account_request(move |transport, state, _| {
            transport.lookup_contacts_raw_snapshot(state, &usernames)
        })
        .await
    }

    pub async fn lookup_contacts_receipt(
        &self,
        usernames: &[String],
    ) -> Result<crate::MetadataReply<crate::ContactLookup>> {
        let usernames = usernames.to_vec();
        self.account_request(move |transport, state, _| {
            transport.lookup_contacts_receipt_snapshot(state, &usernames)
        })
        .await
    }

    pub async fn lookup_room_receipt(
        &self,
        room_id: &str,
        version: u32,
    ) -> Result<crate::MetadataReply<crate::RoomRoster>> {
        let room_id = room_id.to_owned();
        self.account_request(move |transport, state, _| {
            transport.lookup_room_receipt_snapshot(state, &room_id, version)
        })
        .await
    }

    pub async fn lookup_room_raw(&self, room_id: &str, version: u32) -> Result<Vec<u8>> {
        let room_id = room_id.to_owned();
        self.account_request(move |transport, state, _| {
            transport.lookup_room_raw_snapshot(state, &room_id, version)
        })
        .await
    }

    pub async fn init_contacts_receipt(
        &self,
        contact_sequence: u32,
        room_sequence: u32,
    ) -> Result<crate::MetadataReply<crate::InitContactPage>> {
        self.account_request(move |transport, state, _| {
            transport.init_contacts_receipt_snapshot(state, contact_sequence, room_sequence)
        })
        .await
    }

    pub async fn group_detail_raw(&self, group_id: &str) -> Result<Vec<u8>> {
        let group_id = group_id.to_owned();
        self.account_request(move |transport, state, _| {
            transport.group_detail_raw_snapshot(state, &group_id)
        })
        .await
    }

    pub async fn group_mutation_raw(&self, request: &crate::GroupRpc) -> Result<Vec<u8>> {
        let request = request.clone();
        self.account_request(move |transport, state, _| {
            transport.group_mutation_raw_snapshot(state, &request)
        })
        .await
    }

    pub async fn recall_message(
        &self,
        request: &crate::MessageRecall,
    ) -> Result<crate::MessageRecallReply> {
        let request = request.clone();
        self.account_request(move |transport, state, _| {
            transport.recall_message_snapshot(state, &request)
        })
        .await
    }

    pub async fn notify_read_session(
        &self,
        notification: &crate::SessionReadNotification,
    ) -> Result<crate::SessionReadReply> {
        let notification = notification.clone();
        self.account_request(move |transport, state, _| {
            transport.notify_read_session_snapshot(state, &notification)
        })
        .await
    }

    pub async fn contact_sync(
        &self,
        cursor: &[u8],
        message_cursor: &[u8],
        continuation: bool,
    ) -> Result<crate::contact_sync::ContactSyncBatch> {
        let cursor = cursor.to_vec();
        let message_cursor = message_cursor.to_vec();
        self.account_request(move |transport, state, _| {
            transport.contact_sync_snapshot(state, &cursor, &message_cursor, continuation)
        })
        .await
    }

    pub async fn send_share(
        &self,
        destination: &str,
        client_id: &str,
        created_at: u32,
        content: &str,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id, content) = (
            destination.to_owned(),
            client_id.to_owned(),
            content.to_owned(),
        );
        self.account_request(move |transport, state, _| {
            transport.send_share_snapshot(state, &destination, &client_id, created_at, &content)
        })
        .await
    }

    pub async fn send_quote(
        &self,
        destination: &str,
        client_id: &str,
        created_at: u32,
        content: &str,
        source: Option<&str>,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id, content) = (
            destination.to_owned(),
            client_id.to_owned(),
            content.to_owned(),
        );
        let source = source.map(str::to_owned);
        self.account_request(move |transport, state, _| {
            transport.send_quote_with_source_snapshot(
                state,
                &destination,
                &client_id,
                created_at,
                &content,
                source.as_deref(),
            )
        })
        .await
    }

    pub async fn reserve_file_upload(
        &self,
        destination: &str,
        progress_client_id: &str,
        file_name: &str,
        plaintext_bytes: usize,
        md5: &str,
    ) -> Result<crate::file_upload::FileUploadReservation> {
        let (destination, progress_client_id, file_name, md5) = (
            destination.to_owned(),
            progress_client_id.to_owned(),
            file_name.to_owned(),
            md5.to_owned(),
        );
        self.account_request(move |transport, state, _| {
            transport.reserve_file_upload_snapshot(
                state,
                &destination,
                &progress_client_id,
                &file_name,
                plaintext_bytes,
                &md5,
            )
        })
        .await
    }

    pub async fn send_file(
        &self,
        destination: &str,
        client_id: &str,
        created_at: u32,
        upload: &crate::media::UploadedMedia,
        reservation: Option<&crate::file_upload::FileUploadReservation>,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
        let upload = upload.clone();
        let reservation = reservation.cloned();
        self.account_request(move |transport, state, _| {
            transport.send_file_with_reservation_snapshot(
                state,
                &destination,
                &client_id,
                created_at,
                &upload,
                reservation.as_ref(),
            )
        })
        .await
    }

    pub async fn send_image(
        &self,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedImage,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
        let upload = upload.clone();
        self.account_request(move |transport, state, _| {
            transport.send_image_snapshot(state, &destination, &client_id, &upload)
        })
        .await
    }

    /// Submit an image offering its verified standard and thumbnail copies without an original.
    pub async fn send_standard_image(
        &self,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedImage,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
        let upload = upload.clone();
        self.account_request(move |transport, state, _| {
            transport.send_standard_image_snapshot(state, &destination, &client_id, &upload)
        })
        .await
    }

    pub async fn send_video(
        &self,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedVideo,
        play_seconds: u32,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
        let upload = upload.clone();
        self.account_request(move |transport, state, _| {
            transport.send_video_snapshot(state, &destination, &client_id, &upload, play_seconds)
        })
        .await
    }

    pub async fn send_voice(
        &self,
        destination: &str,
        client_id: &str,
        created_at: u32,
        duration_ms: u32,
        encoded: &[u8],
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
        let encoded = encoded.to_vec();
        self.account_request(move |transport, state, _| {
            transport.send_voice_snapshot(
                state,
                &destination,
                &client_id,
                created_at,
                duration_ms,
                &encoded,
            )
        })
        .await
    }

    /// Explicitly renew this account once. Concurrent requests from the prior
    /// credential generation cannot submit or adopt responses after acceptance.
    /// There is no QR fallback and no retry after an uncertain auth outcome.
    pub async fn renew_session(&self, computer_name: &str) -> Result<()> {
        let (exchange, state) = self.owned_account()?.begin()?;
        let exchange = Arc::new(exchange);
        let computer_name = computer_name.to_owned();
        let options = self.options.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        tokio::task::spawn_blocking(move || {
            let mut transport = NativeClient::new(options)?;
            let submission_exchange = exchange.clone();
            transport.submission_guard = Some(Arc::new(move || {
                ensure!(
                    !cancellation.load(Ordering::Acquire),
                    "Account authentication cancelled before submission"
                );
                submission_exchange.check()
            }));
            let returned = transport.renew_session_snapshot(&state, &computer_name)?;
            exchange.accept_credentials(returned)
        })
        .await
        .context("Account authentication worker failed")?
    }

    /// Fence admission, drain prior requests, then submit logout at most once.
    /// The owner stays stopped after rejection, cancellation or uncertainty;
    /// this operation does not revoke remembered-device trust or delete profiles.
    pub async fn logout(&self) -> Result<crate::LogoutReply> {
        self.prepare_logout()?.submit().await
    }

    /// Submit one text message without automatic renewal or retry. Inspect both
    /// receipt correlation fields and acceptance before reporting success.
    pub async fn send_text(
        &self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
    ) -> Result<crate::TextSendResult> {
        self.send_text_guarded(destination, content, options, || Ok(()))
            .await
    }

    pub async fn send_text_guarded<F>(
        &self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
        before_submit: F,
    ) -> Result<crate::TextSendResult>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        let destination = destination.to_owned();
        let content = content.to_owned();
        self.account_request(move |transport, state, _| {
            let request = crate::wire::text_send_request_with_source(
                &destination,
                &content,
                options.created_at,
                options.client_message_id,
                options.source.as_deref(),
            )?;
            let reply = transport.authenticated_rpc_guarded(
                state,
                522,
                "/cgi-bin/micromsg-bin/newsendmsg",
                &request,
                before_submit,
            )?;
            crate::wire::decode_text_send(&reply, &destination, options.client_message_id)
        })
        .await
    }

    /// Execute one call request using this client's account. No automatic retry.
    pub async fn call_rpc(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
    ) -> Result<Vec<u8>> {
        self.call_rpc_guarded(route, request, || Ok(())).await
    }

    /// Recheck account lifetime and caller authority immediately before transport
    /// handoff. A returned error must not be interpreted as proof of no submission.
    pub async fn call_rpc_guarded<F>(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        self.call_rpc_observed(route, request, before_submit, |_| Ok(()))
            .await
    }

    /// Retain a verified call reply before adopting its cookie or returning it to
    /// the calling runtime. The callback must protect these account-specific
    /// bytes, must not log them, and must not reenter this client. Failure is an
    /// uncertain submitted operation, not permission to submit another request.
    pub async fn call_rpc_observed<F, O>(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
        O: Fn(&[u8]) -> Result<()> + Send + Sync + 'static,
    {
        self.account_request(move |transport, state, check| {
            let mut transport = transport.clone();
            transport.call_reply_observer = Some(Arc::new(retain_reply));
            transport.call_rpc_snapshot_guarded(state, route, &request, || {
                check()?;
                before_submit()
            })
        })
        .await
    }

    /// Poll call notifications with caller cancellation and account shutdown.
    /// Interruption is not proof that the read-only request was never submitted.
    pub async fn call_live_sync_guarded<F, C>(
        &self,
        request: Vec<u8>,
        before_submit: F,
        cancellation: C,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
        C: Future<Output = ()> + Send + 'static,
    {
        self.call_live_sync_with_wait_timeout(
            request,
            self.options.timeout,
            before_submit,
            cancellation,
        )
        .await
    }

    /// Keep the handshake deadline independent of the long read-only wait.
    /// The caller owns any retry/reconciliation policy; this submits at most once.
    pub async fn call_live_sync_with_wait_timeout<F, C>(
        &self,
        request: Vec<u8>,
        wait_timeout: Duration,
        before_submit: F,
        cancellation: C,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
        C: Future<Output = ()> + Send + 'static,
    {
        self.call_live_sync_observed(request, wait_timeout, before_submit, cancellation, |_| {
            Ok(())
        })
        .await
    }

    pub async fn call_live_sync_observed<F, C, O>(
        &self,
        request: Vec<u8>,
        wait_timeout: Duration,
        before_submit: F,
        cancellation: C,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
        C: Future<Output = ()> + Send + 'static,
        O: Fn(&[u8]) -> Result<()> + Send + Sync + 'static,
    {
        ensure!(
            !wait_timeout.is_zero() && wait_timeout <= Duration::from_secs(120),
            "Invalid call poll wait timeout"
        );
        let runtime = tokio::runtime::Handle::current();
        let account = self.owned_account()?.clone();
        self.account_request(move |transport, state, check| {
            let mut transport = transport.clone();
            transport.call_reply_observer = Some(Arc::new(retain_reply));
            transport.call_live_sync_snapshot_with_wait_timeout(
                state,
                &request,
                wait_timeout,
                || {
                    check()?;
                    before_submit()
                },
                |reply| runtime.block_on(wait_for_poll(reply, cancellation, account.stopped())),
            )
        })
        .await
    }
}
