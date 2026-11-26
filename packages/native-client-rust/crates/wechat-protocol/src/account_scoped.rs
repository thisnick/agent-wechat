//! Scoped async account requests: the async counterpart of `request_blocking`.
//!
//! [`NativeClient::request`] begins one account exchange and hands the caller an
//! [`AsyncAccountRequest`] for a single operation. The operation's own result
//! is returned to the closure before the account update is committed, so a
//! verified reply survives a later commit failure (for example a logout fence
//! or a persistence error), exactly as with [`super::AccountRequest`].
use super::*;
use crate::transport::{MaybeSend, MaybeSync};

/// One account exchange taken out of the owner: the snapshot state the
/// operation works on and the barrier that commits or discards its update.
pub(crate) struct ExchangeParts {
    pub(super) owner: Arc<OwnedAccount>,
    pub(super) exchange: Exchange,
    pub(super) state: AccountState,
    pub(super) cancellation: Arc<AtomicBool>,
    /// Whether this operation already handed bytes to transport, so a later
    /// fence cannot mark the whole operation as not submitted.
    pub(super) prior_admission: bool,
}

/// The exchange an [`AsyncAccountRequest`] lends to its single operation.
pub(crate) struct RequestScope {
    inner: Mutex<ScopeInner>,
}

struct ScopeInner {
    parts: Option<ExchangeParts>,
    spent: bool,
    closed: bool,
}

impl RequestScope {
    fn new(parts: ExchangeParts) -> Self {
        Self {
            inner: Mutex::new(ScopeInner {
                parts: Some(parts),
                spent: false,
                closed: false,
            }),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, ScopeInner>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("Account request context unavailable"))
    }

    /// Lend the exchange to one operation. A second operation is refused
    /// without the not-submitted marker: it must not relabel the first,
    /// possibly accepted, operation.
    fn acquire(&self) -> Result<ExchangeParts> {
        let mut inner = self.lock()?;
        ensure!(!inner.spent, "Account request context already consumed");
        inner.spent = true;
        inner
            .parts
            .take()
            .context("Account request context has ended")
    }

    fn release(&self, parts: ExchangeParts) {
        if let Ok(mut inner) = self.lock() {
            if !inner.closed {
                inner.parts = Some(parts);
            }
        }
    }

    fn close(&self) -> Option<ExchangeParts> {
        let mut inner = self.lock().ok()?;
        inner.closed = true;
        inner.parts.take()
    }

    fn account(&self) -> Result<AccountState> {
        self.lock()?
            .parts
            .as_ref()
            .map(|parts| parts.state.clone())
            .context("Account request context is in use or has ended")
    }
}

impl NativeClient {
    /// Run one account exchange on `transport`: inside a scoped request it
    /// borrows that request's exchange; otherwise it begins a new exchange,
    /// detaches it from the caller's future, and commits the update after
    /// `body` returns.
    pub(super) async fn run_account_exchange<T, F, Fut>(
        &self,
        transport: &crate::transport::ProtocolTransport,
        body: F,
    ) -> Result<T>
    where
        T: MaybeSend + 'static,
        F: FnOnce(ExchangeParts) -> Fut + MaybeSend + 'static,
        Fut: Future<Output = (ExchangeParts, Result<T>)> + MaybeSend + 'static,
    {
        if let Some(scope) = &self.scope {
            let parts = scope.acquire()?;
            let (parts, result) = body(parts).await;
            scope.release(parts);
            return result;
        }
        let owner = self.owned_account()?.clone();
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        run_detached_exchange(transport, move |cancellation| async move {
            let parts = ExchangeParts {
                owner,
                exchange,
                state,
                cancellation,
                prior_admission: false,
            };
            let (parts, result) = body(parts).await;
            finish_account_exchange(parts.exchange, &parts.state, result)
        })
        .await
    }

    /// Execute one scoped account operation on this client's
    /// [`crate::transport::ProtocolTransport`]: the async counterpart of
    /// `request_blocking`, with the same admission, credential-generation,
    /// storage and handoff barriers.
    ///
    /// `operation` is an async closure that receives an [`AsyncAccountRequest`]
    /// admitting a single operation, for example
    /// `client.request(async |request| request.sync(&cursor).await)`. Whatever that operation returned is visible inside the
    /// closure before the account update is committed, so a caller can keep a
    /// verified reply even when this method then fails to commit (for example
    /// with [`AccountPersistenceFailed`] or after a logout fence). Such a
    /// failure never authorizes resubmission.
    ///
    /// The exchange runs on the caller's task. Do not drop the returned future
    /// after the operation may have been submitted; use the ordinary async
    /// methods when the caller may cancel. Clients without a configured
    /// transport fail with [`NativeRequestNotSubmitted`].
    #[cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]
    pub async fn request<T, F>(&self, operation: F) -> Result<T>
    where
        F: AsyncFnOnce(&mut AsyncAccountRequest) -> Result<T>,
    {
        let transport = self.transport.clone().ok_or_else(|| {
            anyhow::anyhow!("Scoped async requests need a configured transport")
                .context(NativeRequestNotSubmitted)
        })?;
        let owner = self.owned_account()?.clone();
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let scope = Arc::new(RequestScope::new(ExchangeParts {
            owner,
            exchange,
            state,
            cancellation,
            prior_admission: false,
        }));
        // The scoped handle has no account of its own, so every entry point
        // that would begin another exchange fails closed.
        let mut client = Self::from_validated_options(self.options.clone(), Some(transport))?;
        client.scope = Some(scope.clone());
        let mut request = AsyncAccountRequest { client };
        let result = operation(&mut request).await;
        drop(request);
        let parts = scope
            .close()
            .context("Account request context was still in use")?;
        finish_account_exchange(parts.exchange, &parts.state, result)
    }
}

/// A single logical operation inside [`NativeClient::request`]. It borrows the
/// request's temporary account state; it is not an account owner, and only
/// one operation may run through it.
pub struct AsyncAccountRequest {
    client: NativeClient,
}

macro_rules! scoped {
    ($(#[$meta:meta])* $name:ident($($arg:ident: $ty:ty),*) -> $result:ty) => {
        $(#[$meta])*
        pub async fn $name(&mut self, $($arg: $ty),*) -> Result<$result> {
            self.client.$name($($arg),*).await
        }
    };
}

impl AsyncAccountRequest {
    /// A copy of the request's account state for protocol preparation and
    /// journal binding. It is secret, not an independent owner or a handoff.
    /// Unavailable while an operation is running.
    pub fn account(&self) -> Result<AccountState> {
        self.client
            .scope
            .as_ref()
            .context("Account request context has ended")?
            .account()
    }

    scoped!(
        /// Fetch one sync page.
        sync(cursor: &[u8]) -> crate::SyncBatch);
    scoped!(
        /// Fetch one sync page for `scene` (6 or 7).
        sync_scene(cursor: &[u8], scene: u32) -> crate::SyncBatch);
    scoped!(
        /// Send one heartbeat.
        heartbeat(cursor: &[u8]) -> crate::Heartbeat);
    scoped!(
        /// Discover a contact by WeChat ID; does not establish friendship.
        search_contact(query: &str) -> crate::ContactSearchResult);
    scoped!(
        /// Submit a friend request once, without profile writeback.
        send_contact_request(input: &crate::SendContactRequest) -> crate::ContactRequestReply);
    scoped!(
        /// Accept the supplied incoming request once, without profile writeback.
        accept_contact_request(input: &crate::AcceptContactRequest) -> crate::ContactRequestReply);
    scoped!(
        /// Remove this account's contact relationship once.
        remove_contact(username: &str) -> crate::GroupMutationReply);
    scoped!(
        /// Return protected authenticated contact reply bytes before interpretation.
        contact_rpc_raw(request: &crate::ContactRpc) -> Vec<u8>);
    scoped!(
        /// Look up contacts by username.
        lookup_contacts(ids: &[String]) -> crate::ContactLookup);
    scoped!(
        /// Look up contacts and return the decoded reply with its raw bytes.
        lookup_contacts_receipt(ids: &[String]) -> crate::MetadataReply<crate::ContactLookup>);
    scoped!(
        /// Look up contacts and return the raw reply bytes.
        lookup_contacts_raw(ids: &[String]) -> Vec<u8>);
    scoped!(
        /// Read one exact OpenIM contact using optional group context and a supplied ticket.
        lookup_openim_contact(username: &str, room: Option<&str>, ticket: Option<&str>) -> crate::OpenImContact);
    scoped!(
        /// Return the authenticated OpenIM reply bytes for caller-owned receipt storage.
        lookup_openim_contact_raw(username: &str, room: Option<&str>, ticket: Option<&str>) -> Vec<u8>);
    scoped!(
        /// Read one group's member roster.
        lookup_room(room: &str, version: u32) -> crate::RoomRoster);
    scoped!(
        /// Read one group's member roster with its raw reply bytes.
        lookup_room_receipt(room: &str, version: u32) -> crate::MetadataReply<crate::RoomRoster>);
    scoped!(
        /// Read one group's member roster as raw reply bytes.
        lookup_room_raw(room: &str, version: u32) -> Vec<u8>);
    scoped!(
        /// Read one contact bootstrap page.
        init_contacts(contact_sequence: u32, room_sequence: u32) -> crate::InitContactPage);
    scoped!(
        /// Read one contact bootstrap page with its raw reply bytes.
        init_contacts_receipt(contact_sequence: u32, room_sequence: u32) -> crate::MetadataReply<crate::InitContactPage>);
    scoped!(
        /// Download contact avatars by username.
        download_avatars(ids: &[String]) -> Vec<crate::AvatarBytes>);
    scoped!(
        /// Download one voice chunk.
        download_voice_chunk(server_id: u64, length: u32, offset: u32) -> crate::VoiceChunk);
    scoped!(
        /// Fetch one chunk of an oversized application-message attachment through CGI 221.
        download_app_attach_chunk(request: &crate::AppAttachRequest) -> crate::AppAttachChunk);
    scoped!(
        /// Read the account's CDN configuration.
        cdn_config() -> crate::media::CdnConfig);
    scoped!(
        /// Read the CDN configuration for one large-file route.
        large_cdn_config(route: crate::media::LargeCdnRoute) -> crate::media::CdnConfig);
    scoped!(
        /// Request a new H5 ticket for Channels post lookups. Submitted once.
        channels_ticket() -> crate::ChannelsTicket);
    scoped!(
        /// Request the authenticated page address for a Channels post. Submitted once.
        channels_page(post: &crate::ChannelsPost) -> crate::ChannelsPage);
    scoped!(
        /// Request a Channels post's detail and its single video's fresh playback material.
        channels_media(post: &crate::ChannelsPost, ticket: &crate::ChannelsTicket, page: &crate::ChannelsPage) -> crate::ChannelsMedia);
    scoped!(
        /// Read one group inventory page.
        group_inventory(cursor: &[u8]) -> crate::GroupMetadataPage);
    scoped!(
        /// Read one group's detail as raw reply bytes.
        group_detail_raw(group: &str) -> Vec<u8>);
    scoped!(
        /// Submit one group mutation and return the raw reply bytes.
        group_mutation_raw(request: &crate::GroupRpc) -> Vec<u8>);
    scoped!(
        /// Recall one sent message.
        recall_message(request: &crate::MessageRecall) -> crate::MessageRecallReply);
    scoped!(
        /// Tell the server a conversation was read.
        notify_read_session(request: &crate::SessionReadNotification) -> crate::SessionReadReply);
    scoped!(
        /// Read one contact-sync page.
        contact_sync(cursor: &[u8], message_cursor: &[u8], continuation: bool) -> crate::contact_sync::ContactSyncBatch);
    scoped!(
        /// Send one shared app message.
        send_share(destination: &str, client_id: &str, created_at: u32, content: &str) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Send one quoted reply.
        send_quote(destination: &str, client_id: &str, created_at: u32, content: &str, source: Option<&str>) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Reserve a file upload before sending a large file.
        reserve_file_upload(destination: &str, client_id: &str, name: &str, length: usize, md5: &str) -> crate::file_upload::FileUploadReservation);
    scoped!(
        /// Submit an uploaded file.
        send_file(destination: &str, client_id: &str, created_at: u32, upload: &crate::media::UploadedMedia, reservation: Option<&crate::file_upload::FileUploadReservation>) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Submit the uploaded standard and thumbnail copies, including an original when present.
        send_image(destination: &str, client_id: &str, upload: &crate::media::UploadedImage) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Submit an uploaded video.
        send_video(destination: &str, client_id: &str, upload: &crate::media::UploadedVideo, seconds: u32) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Upload and submit one encoded voice note.
        send_voice(destination: &str, client_id: &str, created_at: u32, duration_ms: u32, encoded: &[u8]) -> crate::media_send::MediaSendResult);
    scoped!(
        /// Submit one text message.
        send_text(destination: &str, content: &str, options: TextSendOptions) -> crate::TextSendResult);
    scoped!(
        /// Submit one direct sticker upload chunk. Journal the attempt before calling.
        upload_sticker_chunk(source: &[u8], offset: u32) -> crate::sticker_upload::StickerUploadAcknowledgement);
    scoped!(
        /// Submit one direct sticker upload chunk and return its raw acknowledgement.
        upload_sticker_chunk_raw(source: &[u8], offset: u32) -> Vec<u8>);
    scoped!(
        /// Return one custom-list or metadata reply for caller-owned receipt retention.
        custom_stickers_raw(request: &[u8]) -> Vec<u8>);
    scoped!(
        /// Submit one journaled Saved membership/placement intent and return the raw reply.
        mutate_custom_sticker(id: &str, action: &crate::custom_stickers::CustomStickerMutation) -> Vec<u8>);
    scoped!(
        /// Read a native pack-list page as raw reply bytes.
        sticker_pack_page_raw(feed: crate::sticker_packs::StickerPackFeed, cursor: &[u8]) -> Vec<u8>);
    scoped!(
        /// Read full metadata and content references for one product as raw reply bytes.
        sticker_pack_detail_raw(id: &str) -> Vec<u8>);
    scoped!(
        /// Read localized caption rows for one product as raw reply bytes.
        sticker_pack_captions_raw(id: &str) -> Vec<u8>);
    scoped!(
        /// Submit one journaled eligible-free membership change and return the raw reply.
        set_sticker_pack_membership_raw(id: &str, added: bool) -> Vec<u8>);
    scoped!(
        /// Acquire one archive locator without changing pack membership; return the raw reply.
        sticker_pack_acquisition_raw(id: &str) -> Vec<u8>);
    scoped!(
        /// Submit one journaled probe or byte continuation for a single sticker send.
        send_sticker_chunk_raw(chunk: &crate::sticker_send::StickerSendChunk<'_>) -> Vec<u8>);

    /// Submit one text message with a final check right before transport handoff.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn send_text_guarded<F>(
        &mut self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
        before_submit: F,
    ) -> Result<crate::TextSendResult>
    where
        F: FnOnce() -> Result<()> + Send + 'static,
    {
        self.client
            .send_text_guarded(destination, content, options, before_submit)
            .await
    }

    /// Submit one text message with a final check right before transport handoff.
    #[cfg(target_arch = "wasm32")]
    pub async fn send_text_guarded<F>(
        &mut self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
        before_submit: F,
    ) -> Result<crate::TextSendResult>
    where
        F: FnOnce() -> Result<()> + 'static,
    {
        self.client
            .send_text_guarded(destination, content, options, before_submit)
            .await
    }

    /// Execute one call request, retaining the verified reply before its
    /// cookie is adopted. See [`NativeClient::call_rpc_observed`].
    pub async fn call_rpc<F, O>(
        &mut self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + MaybeSend + 'static,
        O: Fn(&[u8]) -> Result<()> + MaybeSend + MaybeSync + 'static,
    {
        self.client
            .call_rpc_observed_impl(route, request, before_submit, retain_reply)
            .await
    }

    /// Poll call notifications until a reply, `wait_timeout`, account shutdown
    /// or `cancellation` resolves. See [`NativeClient::call_live_sync_observed`].
    pub async fn call_live_sync<F, C, O>(
        &mut self,
        request: Vec<u8>,
        wait_timeout: Duration,
        before_submit: F,
        cancellation: C,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + MaybeSend + 'static,
        C: Future<Output = ()> + MaybeSend + 'static,
        O: Fn(&[u8]) -> Result<()> + MaybeSend + MaybeSync + 'static,
    {
        self.client
            .call_live_sync_observed_impl(
                request,
                wait_timeout,
                before_submit,
                cancellation,
                retain_reply,
            )
            .await
    }
}
