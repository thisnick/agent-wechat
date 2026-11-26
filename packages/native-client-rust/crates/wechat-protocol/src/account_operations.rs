//! Account-owned asynchronous protocol operations.
use super::*;
use crate::transport::{MaybeSend, MaybeSync};

impl NativeClient {
    /// Fetch one sync page using the owned account. The application still owns
    /// command retention, projection and committing the returned cursor.
    pub async fn sync(&self, cursor: &[u8]) -> Result<crate::SyncBatch> {
        self.sync_scene(cursor, 6).await
    }

    pub async fn heartbeat(&self, cursor: &[u8]) -> Result<crate::Heartbeat> {
        let cursor = cursor.to_vec();
        let now_secs = if let Some(transport) = &self.transport {
            transport.clock.unix_time_secs()?
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::wire::unix_time()?
            }
            #[cfg(target_arch = "wasm32")]
            {
                anyhow::bail!("Native client has no async transport configured")
            }
        };
        self.account_single_rpc(
            move |state| {
                Ok((
                    518,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/heartbeat"),
                    crate::wire::heartbeat_request_at(state, &cursor, now_secs)?,
                ))
            },
            move |_, reply| crate::wire::decode_heartbeat(&reply),
        )
        .await
    }

    pub async fn sync_scene(&self, cursor: &[u8], scene: u32) -> Result<crate::SyncBatch> {
        let cursor_req = cursor.to_vec();
        let cursor_dec = cursor.to_vec();
        self.account_single_rpc(
            move |state| {
                ensure!([6, 7].contains(&scene), "Unsupported sync scene");
                let wire_cursor = if cursor_req.is_empty() {
                    &[0][..]
                } else {
                    &cursor_req[..]
                };
                Ok((
                    138,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/newsync"),
                    crate::wire::sync_request(wire_cursor, &state.device.device_type, scene)?,
                ))
            },
            move |_, reply| crate::wire::decode_sync(&reply, &cursor_dec),
        )
        .await
    }

    pub async fn lookup_contacts(&self, usernames: &[String]) -> Result<crate::ContactLookup> {
        let usernames_req = usernames.to_vec();
        let usernames_dec = usernames.to_vec();
        self.account_single_rpc(
            move |state| {
                Ok((
                    182,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getcontact"),
                    crate::wire::contact_request(state, &usernames_req)?,
                ))
            },
            move |_, reply| crate::wire::decode_contacts(&reply, &usernames_dec),
        )
        .await
    }

    pub async fn lookup_room(&self, room_id: &str, version: u32) -> Result<crate::RoomRoster> {
        let room_req = room_id.to_owned();
        let room_dec = room_id.to_owned();
        self.account_single_rpc(
            move |state| {
                Ok((
                    551,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getchatroommemberdetail"),
                    crate::wire::room_request(state, &room_req, version)?,
                ))
            },
            move |_, reply| crate::wire::decode_room(&reply, &room_dec),
        )
        .await
    }

    pub async fn init_contacts(
        &self,
        contact_sequence: u32,
        room_sequence: u32,
    ) -> Result<crate::InitContactPage> {
        self.account_single_rpc(
            move |state| {
                Ok((
                    851,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/initcontact"),
                    crate::wire::init_contacts_request(
                        &state.own_username,
                        contact_sequence,
                        room_sequence,
                    )?,
                ))
            },
            move |_, reply| {
                let page = crate::wire::decode_init_contacts(&reply)?;
                ensure!(
                    !page.more
                        || page.contact_sequence != contact_sequence
                        || page.room_sequence != room_sequence,
                    "Contact bootstrap made no cursor progress"
                );
                Ok(page)
            },
        )
        .await
    }

    pub async fn download_avatars(&self, usernames: &[String]) -> Result<Vec<crate::AvatarBytes>> {
        let usernames_req = usernames.to_vec();
        let usernames_dec = usernames.to_vec();
        self.account_single_rpc(
            move |state| {
                Ok((
                    123,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/batchgetheadimg"),
                    crate::wire::avatar_request(state, &usernames_req)?,
                ))
            },
            move |_, reply| crate::wire::decode_avatars(&reply, &usernames_dec),
        )
        .await
    }

    pub async fn download_voice_chunk(
        &self,
        server_message_id: u64,
        total_length: u32,
        offset: u32,
    ) -> Result<crate::VoiceChunk> {
        self.account_single_rpc(
            move |state| {
                Ok((
                    128,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/downloadvoice"),
                    crate::wire::voice_request(state, server_message_id, total_length, offset)?,
                ))
            },
            move |_, reply| {
                crate::wire::decode_voice(&reply, server_message_id, total_length, offset)
            },
        )
        .await
    }

    /// Fetch one chunk of an oversized application-message attachment through CGI 221.
    pub async fn download_app_attach_chunk(
        &self,
        request: &crate::AppAttachRequest,
    ) -> Result<crate::AppAttachChunk> {
        let req_build = request.clone();
        let req_dec = request.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    221,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/downloadappattach"),
                    crate::wire::app_attach_request(state, &req_build)?,
                ))
            },
            move |_, reply| crate::wire::decode_app_attach(&reply, &req_dec),
        )
        .await
    }

    pub async fn cdn_config(&self) -> Result<crate::media::CdnConfig> {
        self.account_single_rpc(
            |state| {
                Ok((
                    379,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getcdndns"),
                    crate::protobuf::bytes(
                        1,
                        &crate::wire::base_request(&state.device, state.uin, 0)?,
                    ),
                ))
            },
            |state, reply| crate::media::decode_cdn_config(&reply, state),
        )
        .await
    }

    pub async fn large_cdn_config(
        &self,
        route: crate::media::LargeCdnRoute,
    ) -> Result<crate::media::CdnConfig> {
        self.account_single_rpc(
            |state| {
                Ok((
                    379,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getcdndns"),
                    crate::protobuf::bytes(
                        1,
                        &crate::wire::base_request(&state.device, state.uin, 0)?,
                    ),
                ))
            },
            move |state, reply| crate::media::decode_large_cdn_config(&reply, state, route),
        )
        .await
    }

    pub async fn group_inventory(&self, cursor: &[u8]) -> Result<crate::GroupMetadataPage> {
        let cursor_req = cursor.to_vec();
        let cursor_dec = cursor.to_vec();
        self.account_single_rpc(
            move |state| {
                Ok((
                    15595,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/syncsimplechatroomdata"),
                    crate::group_inventory_request(state, &cursor_req)?,
                ))
            },
            move |_, reply| {
                let page = crate::decode_group_inventory(&reply)?;
                ensure!(
                    !page.more || page.next_cursor.as_deref() != Some(cursor_dec.as_slice()),
                    "Group metadata made no cursor progress"
                );
                Ok(page)
            },
        )
        .await
    }

    pub async fn lookup_contacts_raw(&self, usernames: &[String]) -> Result<Vec<u8>> {
        let usernames = usernames.to_vec();
        self.account_single_rpc(
            move |state| {
                Ok((
                    182,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getcontact"),
                    crate::wire::contact_request(state, &usernames)?,
                ))
            },
            |_, reply| {
                ensure!(
                    reply.len() <= crate::MAX_MESSAGE_BYTES,
                    "Contact response exceeds bound"
                );
                Ok(reply)
            },
        )
        .await
    }

    pub async fn lookup_contacts_receipt(
        &self,
        usernames: &[String],
    ) -> Result<crate::MetadataReply<crate::ContactLookup>> {
        let usernames_req = usernames.to_vec();
        let usernames_dec = usernames.to_vec();
        self.account_single_rpc(
            move |state| {
                Ok((
                    182,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getcontact"),
                    crate::wire::contact_request(state, &usernames_req)?,
                ))
            },
            move |_, raw| {
                ensure!(
                    raw.len() <= crate::MAX_MESSAGE_BYTES,
                    "Contact response exceeds bound"
                );
                Ok(crate::MetadataReply {
                    value: crate::wire::decode_contacts(&raw, &usernames_dec)?,
                    raw,
                })
            },
        )
        .await
    }

    pub async fn lookup_room_receipt(
        &self,
        room_id: &str,
        version: u32,
    ) -> Result<crate::MetadataReply<crate::RoomRoster>> {
        let room_req = room_id.to_owned();
        let room_dec = room_id.to_owned();
        self.account_single_rpc(
            move |state| {
                Ok((
                    551,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getchatroommemberdetail"),
                    crate::wire::room_request(state, &room_req, version)?,
                ))
            },
            move |_, raw| {
                Ok(crate::MetadataReply {
                    value: crate::wire::decode_room(&raw, &room_dec)?,
                    raw,
                })
            },
        )
        .await
    }

    pub async fn lookup_room_raw(&self, room_id: &str, version: u32) -> Result<Vec<u8>> {
        let room_id = room_id.to_owned();
        self.account_single_rpc(
            move |state| {
                Ok((
                    551,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getchatroommemberdetail"),
                    crate::wire::room_request(state, &room_id, version)?,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }

    pub async fn init_contacts_receipt(
        &self,
        contact_sequence: u32,
        room_sequence: u32,
    ) -> Result<crate::MetadataReply<crate::InitContactPage>> {
        self.account_single_rpc(
            move |state| {
                Ok((
                    851,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/initcontact"),
                    crate::wire::init_contacts_request(
                        &state.own_username,
                        contact_sequence,
                        room_sequence,
                    )?,
                ))
            },
            move |_, raw| {
                let value = crate::wire::decode_init_contacts(&raw)?;
                ensure!(
                    !value.more
                        || value.contact_sequence != contact_sequence
                        || value.room_sequence != room_sequence,
                    "Contact bootstrap made no cursor progress"
                );
                Ok(crate::MetadataReply { value, raw })
            },
        )
        .await
    }

    pub async fn group_detail_raw(&self, group_id: &str) -> Result<Vec<u8>> {
        let group_id = group_id.to_owned();
        self.account_single_rpc(
            move |state| {
                Ok((
                    223,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/getchatroominfodetail"),
                    crate::group_detail_request(state, &group_id)?,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }

    pub async fn group_mutation_raw(&self, request: &crate::GroupRpc) -> Result<Vec<u8>> {
        let request = request.clone();
        self.account_single_rpc(
            move |_| {
                super::super::group_mutation::validate_group_rpc(&request)?;
                Ok((request.cgi, Cow::Owned(request.path), request.body))
            },
            |_, raw| Ok(raw),
        )
        .await
    }

    pub async fn recall_message(
        &self,
        request: &crate::MessageRecall,
    ) -> Result<crate::MessageRecallReply> {
        let request = request.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    super::super::revoke::CGI,
                    Cow::Borrowed(super::super::revoke::PATH),
                    super::super::revoke::request(state, &request)?,
                ))
            },
            |_, raw| super::super::revoke::response(&raw),
        )
        .await
    }

    pub async fn notify_read_session(
        &self,
        notification: &crate::SessionReadNotification,
    ) -> Result<crate::SessionReadReply> {
        let notification = notification.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    super::super::status_notify::CGI,
                    Cow::Borrowed(super::super::status_notify::PATH),
                    super::super::status_notify::request(state, &notification)?,
                ))
            },
            |_, raw| super::super::status_notify::response(&raw),
        )
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
        self.account_single_rpc(
            move |state| {
                Ok((
                    8674,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/contactsync"),
                    crate::contact_sync::request(state, &cursor, &message_cursor, continuation)?,
                ))
            },
            |_, raw| crate::contact_sync::decode(&raw),
        )
        .await
    }

    pub async fn send_share(
        &self,
        destination: &str,
        client_id: &str,
        created_at: u32,
        content: &str,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id_req, client_id_dec, content) = (
            destination.to_owned(),
            client_id.to_owned(),
            client_id.to_owned(),
            content.to_owned(),
        );
        self.account_single_rpc(
            move |state| {
                Ok((
                    222,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/sendappmsg"),
                    crate::media_send::share_send_request(
                        state,
                        &destination,
                        &client_id_req,
                        created_at,
                        &content,
                    )?,
                ))
            },
            move |_, raw| crate::media_send::decode_file_send(&raw, &client_id_dec),
        )
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
        let (destination, client_id_req, client_id_dec, content) = (
            destination.to_owned(),
            client_id.to_owned(),
            client_id.to_owned(),
            content.to_owned(),
        );
        let source = source.map(str::to_owned);
        self.account_single_rpc(
            move |state| {
                Ok((
                    222,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/sendappmsg"),
                    crate::quote_send::quote_send_request_with_source(
                        state,
                        &destination,
                        &client_id_req,
                        created_at,
                        &content,
                        source.as_deref(),
                    )?,
                ))
            },
            move |_, raw| crate::media_send::decode_file_send(&raw, &client_id_dec),
        )
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
        self.account_single_rpc(
            move |state| {
                Ok((
                    6691,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/sendfileuploadmsg"),
                    crate::file_upload::file_upload_request(
                        state,
                        &destination,
                        &progress_client_id,
                        &file_name,
                        plaintext_bytes,
                        &md5,
                    )?,
                ))
            },
            |_, raw| crate::file_upload::decode_file_upload_reservation(&raw),
        )
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
        let (destination_req, destination_dec, client_id_req, client_id_dec) = (
            destination.to_owned(),
            destination.to_owned(),
            client_id.to_owned(),
            client_id.to_owned(),
        );
        let upload_req = upload.clone();
        let aes_key = upload.descriptor.aes_key.clone();
        let has_reservation = reservation.is_some();
        let reservation = reservation.cloned();
        self.account_single_rpc(
            move |state| {
                Ok((
                    222,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/sendappmsg"),
                    crate::media_send::file_send_request_with_reservation(
                        state,
                        &destination_req,
                        &client_id_req,
                        created_at,
                        &upload_req,
                        reservation.as_ref(),
                    )?,
                ))
            },
            move |state, raw| {
                if has_reservation {
                    crate::media_send::decode_file_send_correlated(
                        &raw,
                        &client_id_dec,
                        &state.own_username,
                        &destination_dec,
                        &aes_key,
                    )
                } else {
                    crate::media_send::decode_file_send(&raw, &client_id_dec)
                }
            },
        )
        .await
    }

    /// Submit the uploaded standard and thumbnail copies, including an original when present.
    pub async fn send_image(
        &self,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedImage,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination_req, destination_dec, client_id_req, client_id_dec) = (
            destination.to_owned(),
            destination.to_owned(),
            client_id.to_owned(),
            client_id.to_owned(),
        );
        let upload = upload.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    110,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/uploadmsgimg"),
                    crate::media_send::image_send_request(
                        state,
                        &destination_req,
                        &client_id_req,
                        &upload,
                    )?,
                ))
            },
            move |state, raw| {
                crate::media_send::decode_image_send(
                    &raw,
                    &client_id_dec,
                    &state.own_username,
                    &destination_dec,
                )
            },
        )
        .await
    }

    pub async fn send_video(
        &self,
        destination: &str,
        client_id: &str,
        upload: &crate::media::UploadedVideo,
        play_seconds: u32,
    ) -> Result<crate::media_send::MediaSendResult> {
        let (destination, client_id_req, client_id_dec) = (
            destination.to_owned(),
            client_id.to_owned(),
            client_id.to_owned(),
        );
        let upload = upload.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    149,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/uploadvideo"),
                    crate::media_send::video_send_request(
                        state,
                        &destination,
                        &client_id_req,
                        &upload,
                        play_seconds,
                    )?,
                ))
            },
            move |_, raw| crate::media_send::decode_video_send(&raw, &client_id_dec),
        )
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
        if let Some(transport) = self.transport.clone() {
            let options = self.options.clone();
            let (destination, client_id) = (destination.to_owned(), client_id.to_owned());
            let encoded = encoded.to_vec();
            let runner = transport.clone();
            return self
                .run_account_exchange(&runner, move |mut parts| async move {
                    let result = async {
                        let mut upload = crate::media_send::VoiceUpload::new(
                            &destination,
                            &client_id,
                            created_at,
                            duration_ms,
                            &encoded,
                        )?;
                        loop {
                            let request = Zeroizing::new(upload.request(&parts.state)?);
                            let reply = Self::async_authenticated_rpc_step(
                                &options,
                                &transport,
                                &parts.owner.short_link,
                                &parts.exchange,
                                &parts.cancellation,
                                &mut parts.state,
                                &mut parts.prior_admission,
                                127,
                                "/cgi-bin/micromsg-bin/uploadvoice",
                                &request,
                                || Ok(()),
                            )
                            .await?;
                            if let Some(done) = upload.accept_reply(&reply)? {
                                return Ok(done);
                            }
                        }
                    }
                    .await;
                    (parts, result)
                })
                .await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
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
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (destination, client_id, created_at, duration_ms, encoded);
            anyhow::bail!("Native client has no async transport configured")
        }
    }

    /// Explicitly renew this account once. Concurrent requests from the prior
    /// credential generation cannot submit or adopt responses after acceptance.
    /// There is no QR fallback and no retry after an uncertain auth outcome.
    pub async fn renew_session(&self, computer_name: &str) -> Result<()> {
        let (exchange, state) = self.owned_account()?.begin()?;
        if let Some(transport) = self.transport.clone() {
            let options = self.options.clone();
            let computer_name = computer_name.to_owned();
            return run_detached_exchange(&transport.clone(), move |cancellation| async move {
                state.validate()?;
                let inner = crate::crypto::Ecdh::generate_for(crate::crypto::Curve::P224)?;
                let request = Zeroizing::new(crate::wire::auto_auth(
                    &state,
                    &inner.public()?,
                    &computer_name,
                )?);
                let mut long = state.long_host.clone();
                let mut short = state.short_host.clone();
                let mut cookie = state.cookie.clone();
                for redirected in 0..=1 {
                    let (reply, next_cookie, _) = Self::prelogin_rpc_async_options(
                        &options,
                        &transport,
                        &long,
                        &short,
                        763,
                        "/cgi-bin/micromsg-bin/secautoauth",
                        &request,
                        &cookie,
                        &state.auto_auth_key,
                        None,
                        || {
                            let check = (|| {
                                ensure!(
                                    !cancellation.load(Ordering::Acquire),
                                    "Account authentication cancelled before submission"
                                );
                                exchange.check()
                            })();
                            if redirected == 0 {
                                check.map_err(|error| error.context(NativeRequestNotSubmitted))
                            } else {
                                check
                            }
                        },
                    )
                    .await?;
                    let fields = crate::protobuf::decode(&reply)?;
                    let code = crate::protobuf::base_code(&fields)?;
                    if code == -301 && redirected == 0 {
                        (long, short) = super::super::redirect(&fields)?;
                        cookie = next_cookie;
                        continue;
                    }
                    crate::errors::ServiceRejected::check(
                        crate::errors::Service::AutomaticAuthentication,
                        code,
                    )?;
                    let mut next = super::super::reduce_auth(
                        &reply,
                        Some(&state),
                        &inner,
                        &state.device,
                        &next_cookie,
                    )?;
                    next.long_host = long;
                    next.short_host = short;
                    next.validate()?;
                    return exchange.accept_credentials(next);
                }
                anyhow::bail!("AutoAuth redirect limit reached");
            })
            .await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
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
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (exchange, state, computer_name);
            anyhow::bail!("Native client has no async transport configured")
        }
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

    /// Submit one text message with a final pre-submission guard invoked right
    /// before transport handoff. No automatic renewal or retry is performed.
    #[cfg(not(target_arch = "wasm32"))]
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
        self.send_text_guarded_impl(destination, content, options, before_submit)
            .await
    }

    /// Submit one text message with a final pre-submission guard invoked right
    /// before transport handoff. No automatic renewal or retry is performed.
    #[cfg(target_arch = "wasm32")]
    pub async fn send_text_guarded<F>(
        &self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
        before_submit: F,
    ) -> Result<crate::TextSendResult>
    where
        F: FnOnce() -> Result<()> + 'static,
    {
        self.send_text_guarded_impl(destination, content, options, before_submit)
            .await
    }

    pub(super) async fn send_text_guarded_impl<F>(
        &self,
        destination: &str,
        content: &str,
        options: TextSendOptions,
        before_submit: F,
    ) -> Result<crate::TextSendResult>
    where
        F: FnOnce() -> Result<()> + MaybeSend + 'static,
    {
        let destination_req = destination.to_owned();
        let destination_dec = destination.to_owned();
        let content = content.to_owned();
        let client_message_id = options.client_message_id;
        self.account_single_rpc_guarded(
            move |_| {
                Ok((
                    522,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/newsendmsg"),
                    crate::wire::text_send_request_with_source(
                        &destination_req,
                        &content,
                        options.created_at,
                        options.client_message_id,
                        options.source.as_deref(),
                    )?,
                ))
            },
            move |_, reply| {
                crate::wire::decode_text_send(&reply, &destination_dec, client_message_id)
            },
            before_submit,
        )
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
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Recheck account lifetime and caller authority immediately before transport
    /// handoff. A returned error must not be interpreted as proof of no submission.
    #[cfg(target_arch = "wasm32")]
    pub async fn call_rpc_guarded<F>(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + 'static,
    {
        self.call_rpc_observed(route, request, before_submit, |_| Ok(()))
            .await
    }

    /// Retain a verified call reply before adopting its cookie or returning it to
    /// the calling runtime. The callback must protect these account-specific
    /// bytes, must not log them, and must not reenter this client. Failure is an
    /// uncertain submitted operation, not permission to submit another request.
    #[cfg(not(target_arch = "wasm32"))]
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
        self.call_rpc_observed_impl(route, request, before_submit, retain_reply)
            .await
    }

    /// Retain a verified call reply before adopting its cookie or returning it to
    /// the calling runtime. The callback must protect these account-specific
    /// bytes, must not log them, and must not reenter this client. Failure is an
    /// uncertain submitted operation, not permission to submit another request.
    #[cfg(target_arch = "wasm32")]
    pub async fn call_rpc_observed<F, O>(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + 'static,
        O: Fn(&[u8]) -> Result<()> + 'static,
    {
        self.call_rpc_observed_impl(route, request, before_submit, retain_reply)
            .await
    }

    pub(super) async fn call_rpc_observed_impl<F, O>(
        &self,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        before_submit: F,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + MaybeSend + 'static,
        O: Fn(&[u8]) -> Result<()> + MaybeSend + MaybeSync + 'static,
    {
        if let Some(transport) = self.transport.clone() {
            return self
                .call_rpc_async(
                    transport,
                    route,
                    request,
                    self.options.timeout,
                    before_submit,
                    retain_reply,
                    None::<std::future::Pending<()>>,
                )
                .await;
        }
        #[cfg(not(target_arch = "wasm32"))]
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
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (route, request, before_submit, retain_reply);
            anyhow::bail!("Native client has no async transport configured")
        }
    }

    /// Poll call notifications with caller cancellation and account shutdown.
    /// Interruption is not proof that the read-only request was never submitted.
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Poll call notifications with caller cancellation and account shutdown.
    /// Interruption is not proof that the read-only request was never submitted.
    #[cfg(target_arch = "wasm32")]
    pub async fn call_live_sync_guarded<F, C>(
        &self,
        request: Vec<u8>,
        before_submit: F,
        cancellation: C,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + 'static,
        C: Future<Output = ()> + 'static,
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
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Keep the handshake deadline independent of the long read-only wait.
    /// The caller owns any retry/reconciliation policy; this submits at most once.
    #[cfg(target_arch = "wasm32")]
    pub async fn call_live_sync_with_wait_timeout<F, C>(
        &self,
        request: Vec<u8>,
        wait_timeout: Duration,
        before_submit: F,
        cancellation: C,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + 'static,
        C: Future<Output = ()> + 'static,
    {
        self.call_live_sync_observed(request, wait_timeout, before_submit, cancellation, |_| {
            Ok(())
        })
        .await
    }

    /// Poll call notifications with independent wait timeout, cancellation, and
    /// a reply-retention observer invoked before adopting any updated cookie.
    #[cfg(not(target_arch = "wasm32"))]
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
        self.call_live_sync_observed_impl(
            request,
            wait_timeout,
            before_submit,
            cancellation,
            retain_reply,
        )
        .await
    }

    /// Poll call notifications with independent wait timeout, cancellation, and
    /// a reply-retention observer invoked before adopting any updated cookie.
    #[cfg(target_arch = "wasm32")]
    pub async fn call_live_sync_observed<F, C, O>(
        &self,
        request: Vec<u8>,
        wait_timeout: Duration,
        before_submit: F,
        cancellation: C,
        retain_reply: O,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + 'static,
        C: Future<Output = ()> + 'static,
        O: Fn(&[u8]) -> Result<()> + 'static,
    {
        self.call_live_sync_observed_impl(
            request,
            wait_timeout,
            before_submit,
            cancellation,
            retain_reply,
        )
        .await
    }

    pub(super) async fn call_live_sync_observed_impl<F, C, O>(
        &self,
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
        ensure!(
            !wait_timeout.is_zero() && wait_timeout <= Duration::from_secs(120),
            "Invalid call poll wait timeout"
        );
        if let Some(transport) = self.transport.clone() {
            return self
                .call_rpc_async(
                    transport,
                    crate::calls::host_policy::CallHostRoute::LiveSync,
                    request,
                    wait_timeout,
                    before_submit,
                    retain_reply,
                    Some(cancellation),
                )
                .await;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
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
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (
                request,
                wait_timeout,
                before_submit,
                cancellation,
                retain_reply,
            );
            anyhow::bail!("Native client has no async transport configured")
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_rpc_async<F, O, C>(
        &self,
        transport: crate::transport::ProtocolTransport,
        route: crate::calls::host_policy::CallHostRoute,
        request: Vec<u8>,
        exchange_timeout: Duration,
        before_submit: F,
        retain_reply: O,
        cancellation: Option<C>,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce() -> Result<()> + MaybeSend + 'static,
        O: Fn(&[u8]) -> Result<()> + MaybeSend + MaybeSync + 'static,
        C: Future<Output = ()> + MaybeSend + 'static,
    {
        use crate::calls::{
            host_policy::{selected_policy, CallHostPolicy},
            host_transport::{CallHostRpcError, CallHostStage},
        };
        let options = self.options.clone();
        let runner = transport.clone();
        self.run_account_exchange(&runner, move |mut parts| async move {
            let request = Zeroizing::new(request);
            let result = async {
                let state = &mut parts.state;
                crate::mmtls::validate_short_route(route.path())
                    .map_err(|error| CallHostRpcError::from_cause(CallHostStage::Route, &error))?;
                let policy = selected_policy(&state.auth_snapshot, route.command())
                    .map_err(|_| CallHostRpcError::new(CallHostStage::Route, "policy", None))?;
                if policy != CallHostPolicy::Ordinary {
                    return Err(CallHostRpcError::new(
                        CallHostStage::Route,
                        "ax_unsupported",
                        None,
                    )
                    .into());
                }
                let session = crate::mmtls::AsyncSession::connect(
                    &state.long_host,
                    options.timeout,
                    options.receive_budget,
                    &*transport.streams,
                    transport.clock.clone(),
                )
                .await
                .map_err(|error| CallHostRpcError::from_cause(CallHostStage::Handshake, &error))?;
                let short = session.short.clone();
                let _ = session.close().await;
                let packet =
                    crate::business::AuthenticatedPacket::prepare(state, route.command(), &request)
                        .map_err(|error| {
                            CallHostRpcError::from_cause(CallHostStage::Prepare, &error)
                        })?;
                if parts.cancellation.load(Ordering::Acquire) {
                    return Err(
                        anyhow::anyhow!("Account request cancelled before submission")
                            .context(NativeRequestNotSubmitted),
                    );
                }
                parts
                    .exchange
                    .check()
                    .map_err(|error| error.context(NativeRequestNotSubmitted))?;
                before_submit()?;
                parts.prior_admission = true;
                let exchange_future = crate::mmtls::short_exchange_async(
                    &short,
                    &state.short_host,
                    route.path(),
                    &packet.packet,
                    exchange_timeout,
                    &*transport.http,
                    &*transport.clock,
                );
                let reply = match cancellation {
                    Some(cancel) => {
                        wait_for_poll(exchange_future, cancel, parts.owner.stopped()).await
                    }
                    None => exchange_future.await,
                }
                .map_err(|error| {
                    if error.is::<crate::calls::host_transport::CallLiveSyncInterrupted>() {
                        error
                    } else {
                        CallHostRpcError::from_cause(CallHostStage::Exchange, &error).into()
                    }
                })?;
                let (plain, cookie) = crate::calls::host_transport::decode_verified_reply(
                    state,
                    route.command(),
                    &reply,
                )?;
                retain_reply(&plain)?;
                if !cookie.is_empty() {
                    state.cookie = cookie;
                }
                Ok(plain)
            }
            .await;
            (parts, result)
        })
        .await
    }
}

impl NativeClient {
    /// Upload one direct custom-sticker chunk. Journal before submission and
    /// retain the receipt; failure does not authorize an automatic resend.
    pub async fn upload_sticker_chunk(
        &self,
        source: &[u8],
        offset: u32,
    ) -> Result<crate::sticker_upload::StickerUploadAcknowledgement> {
        let source_req = source.to_vec();
        let source_len = u32::try_from(source.len())?;
        let source_md5 = hex::encode(crate::crypto::md5(source));
        self.account_single_rpc(
            move |_| {
                Ok((
                    703,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmemojiupload"),
                    crate::sticker_upload::sticker_upload_request(&source_req, offset)?,
                ))
            },
            move |_, reply| {
                crate::sticker_upload::decode_sticker_upload(
                    &reply,
                    offset,
                    source_len,
                    &source_md5,
                )
            },
        )
        .await
    }
    /// Read one custom-list page. The caller checks advancing cursors, retains
    /// the receipt and reconciles official-profile state after a complete list.
    pub async fn custom_sticker_page(
        &self,
        index: u32,
        data: &[u8],
    ) -> Result<crate::custom_stickers::CustomStickerPage> {
        let request = crate::custom_stickers::custom_sticker_list_request(index, data)?;
        self.account_single_rpc(
            move |_| {
                Ok((
                    697,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmbatchemojidownload"),
                    request,
                ))
            },
            |_, raw| crate::custom_stickers::decode_custom_stickers(&raw),
        )
        .await
    }
    /// Read transport metadata for known custom sticker identities.
    pub async fn custom_sticker_metadata(
        &self,
        ids: &[String],
    ) -> Result<crate::custom_stickers::CustomStickerPage> {
        let request = crate::custom_stickers::custom_sticker_metadata_request(ids)?;
        self.account_single_rpc(
            move |_| {
                Ok((
                    697,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmbatchemojidownload"),
                    request,
                ))
            },
            |_, raw| crate::custom_stickers::decode_custom_stickers(&raw),
        )
        .await
    }
    /// Submit one journaled Saved mutation and return the original response even
    /// on service rejection. Retain it before decoding and then refresh the list.
    /// A transport error after submission leaves an uncertain outcome.
    pub async fn mutate_custom_sticker(
        &self,
        id: &str,
        action: &crate::custom_stickers::CustomStickerMutation,
    ) -> Result<Vec<u8>> {
        let request = crate::custom_stickers::custom_sticker_mutation_request(id, action)?;
        self.account_single_rpc(
            move |_| {
                Ok((
                    698,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmbackupemojioperate"),
                    request,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
}

impl NativeClient {
    /// Submit one direct custom-sticker upload chunk and return its raw
    /// acknowledgement for retention before validation. Journal each attempted
    /// offset; an error after submission has an uncertain outcome.
    pub async fn upload_sticker_chunk_raw(&self, source: &[u8], offset: u32) -> Result<Vec<u8>> {
        let request = crate::sticker_upload::sticker_upload_request(source, offset)?;
        self.account_single_rpc(
            move |_| {
                Ok((
                    703,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmemojiupload"),
                    request,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
    /// Return one custom-list or metadata reply for caller-owned receipt
    /// retention. The request must come from the bounded custom-sticker builders.
    pub async fn custom_stickers_raw(&self, request: &[u8]) -> Result<Vec<u8>> {
        ensure!(request.len() <= 65536, "sticker_request_limit");
        let request = request.to_vec();
        self.account_single_rpc(
            move |_| {
                Ok((
                    697,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/mmbatchemojidownload"),
                    request,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
    /// Submit one journaled probe or byte continuation for a single sticker
    /// send on the short link. Retain the raw reply before validating it.
    pub async fn send_sticker_chunk_raw(
        &self,
        chunk: &crate::sticker_send::StickerSendChunk<'_>,
    ) -> Result<Vec<u8>> {
        let (id, recipient, client_id) = (
            chunk.id.to_owned(),
            chunk.recipient.to_owned(),
            chunk.client_id.to_owned(),
        );
        let source = Zeroizing::new(chunk.source.to_vec());
        let (offset, probe) = (chunk.offset, chunk.probe);
        self.account_single_rpc(
            move |state| {
                let chunk = crate::sticker_send::StickerSendChunk {
                    id: &id,
                    recipient: &recipient,
                    client_id: &client_id,
                    source: &source,
                    offset,
                    probe,
                };
                Ok((
                    175,
                    Cow::Borrowed("/cgi-bin/micromsg-bin/sendemoji"),
                    crate::sticker_send::sticker_send_request(state, &chunk)?,
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
    /// Read a native pack-list page and return the raw reply for durable
    /// caller-owned receipt storage.
    pub async fn sticker_pack_page_raw(
        &self,
        feed: crate::sticker_packs::StickerPackFeed,
        cursor: &[u8],
    ) -> Result<Vec<u8>> {
        let cursor = cursor.to_vec();
        self.sticker_pack_raw(411, move |state| {
            crate::sticker_packs::sticker_pack_list_request(state, feed, &cursor)
        })
        .await
    }
    /// Read full metadata and content references for one product without
    /// acquiring media, as raw reply bytes.
    pub async fn sticker_pack_detail_raw(&self, id: &str) -> Result<Vec<u8>> {
        let id = id.to_owned();
        self.sticker_pack_raw(412, move |state| {
            crate::sticker_packs::sticker_pack_detail_request(state, &id)
        })
        .await
    }
    /// Read localized caption rows for one product, retaining the complete
    /// reply.
    pub async fn sticker_pack_captions_raw(&self, id: &str) -> Result<Vec<u8>> {
        let id = id.to_owned();
        self.sticker_pack_raw(521, move |state| {
            crate::sticker_packs::sticker_pack_captions_request(state, &id)
        })
        .await
    }
    /// Acquire one archive locator without changing pack membership. Journal
    /// the attempt before calling and retain the raw reply before decoding it.
    pub async fn sticker_pack_acquisition_raw(&self, id: &str) -> Result<Vec<u8>> {
        let id = id.to_owned();
        self.sticker_pack_raw(423, move |state| {
            crate::sticker_packs::sticker_pack_acquisition_request(state, &id)
        })
        .await
    }
    async fn sticker_pack_raw(
        &self,
        cgi: u16,
        build: impl FnOnce(&AccountState) -> Result<Vec<u8>> + MaybeSend + 'static,
    ) -> Result<Vec<u8>> {
        self.account_single_rpc(
            move |state| {
                let path = crate::sticker_packs::sticker_pack_rpc_path(cgi)?;
                Ok((cgi, Cow::Borrowed(path), build(state)?))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
}

impl NativeClient {
    /// Read one public or account-added feed page. Check advancing cursors and
    /// consume the complete added feed before replacing membership in a profile.
    pub async fn sticker_pack_page(
        &self,
        feed: crate::sticker_packs::StickerPackFeed,
        cursor: &[u8],
    ) -> Result<crate::sticker_packs::StickerPackPage> {
        let cursor = cursor.to_vec();
        self.account_single_rpc(
            move |state| {
                let path = crate::sticker_packs::sticker_pack_rpc_path(411)?;
                let request =
                    crate::sticker_packs::sticker_pack_list_request(state, feed, &cursor)?;
                Ok((411, Cow::Borrowed(path), request))
            },
            |_, raw| crate::sticker_packs::decode_sticker_pack_page(&raw),
        )
        .await
    }
    /// Read product details and content references without adding or downloading it.
    pub async fn sticker_pack_detail(
        &self,
        id: &str,
    ) -> Result<crate::sticker_packs::StickerPackDetail> {
        let id_req = id.to_owned();
        let id_dec = id.to_owned();
        self.account_single_rpc(
            move |state| {
                let path = crate::sticker_packs::sticker_pack_rpc_path(412)?;
                let request = crate::sticker_packs::sticker_pack_detail_request(state, &id_req)?;
                Ok((412, Cow::Borrowed(path), request))
            },
            move |_, raw| crate::sticker_packs::decode_sticker_pack_detail(&raw, &id_dec),
        )
        .await
    }
    /// Read localized captions; their presence establishes neither membership nor entitlement.
    pub async fn sticker_pack_captions(
        &self,
        id: &str,
    ) -> Result<crate::sticker_packs::StickerPackCaptions> {
        let id = id.to_owned();
        self.account_single_rpc(
            move |state| {
                let path = crate::sticker_packs::sticker_pack_rpc_path(521)?;
                let request = crate::sticker_packs::sticker_pack_captions_request(state, &id)?;
                Ok((521, Cow::Borrowed(path), request))
            },
            |_, raw| crate::sticker_packs::decode_sticker_pack_captions(&raw),
        )
        .await
    }
    /// Submit a single journaled eligible-free membership intent and retain the
    /// raw acknowledgement before interpreting acceptance. No automatic retry.
    pub async fn set_sticker_pack_membership_raw(&self, id: &str, added: bool) -> Result<Vec<u8>> {
        let id = id.to_owned();
        self.account_single_rpc(
            move |state| {
                let path = crate::sticker_packs::sticker_pack_rpc_path(413)?;
                let request =
                    crate::sticker_packs::sticker_pack_membership_request(state, &id, added)?;
                Ok((413, Cow::Borrowed(path), request))
            },
            |_, raw| Ok(raw),
        )
        .await
    }
}

impl NativeClient {
    /// Discover a contact without saving or requesting friendship.
    pub async fn search_contact(&self, query: &str) -> Result<crate::ContactSearchResult> {
        let query_req = query.to_owned();
        let query_dec = query.to_owned();
        self.account_single_rpc(
            move |state| {
                let rpc = crate::contact_search_request(state, &query_req)?;
                super::super::contact_requests::validate_contact_rpc(&rpc)?;
                Ok((rpc.cgi, Cow::Owned(rpc.path), rpc.body))
            },
            move |_, raw| crate::decode_contact_search(&raw, &query_dec),
        )
        .await
    }
    /// Submit one request; the embedding application owns journals and profile writeback.
    pub async fn send_contact_request(
        &self,
        input: &crate::SendContactRequest,
    ) -> Result<crate::ContactRequestReply> {
        let input = input.clone();
        self.account_single_rpc(
            move |state| {
                let rpc = crate::send_contact_request_rpc(state, &input)?;
                super::super::contact_requests::validate_contact_rpc(&rpc)?;
                Ok((rpc.cgi, Cow::Owned(rpc.path), rpc.body))
            },
            |_, raw| crate::decode_contact_request(&raw),
        )
        .await
    }
    /// Submit one acceptance with the exact received ticket.
    pub async fn accept_contact_request(
        &self,
        input: &crate::AcceptContactRequest,
    ) -> Result<crate::ContactRequestReply> {
        let input = input.clone();
        self.account_single_rpc(
            move |state| {
                let rpc = crate::accept_contact_request_rpc(state, &input)?;
                super::super::contact_requests::validate_contact_rpc(&rpc)?;
                Ok((rpc.cgi, Cow::Owned(rpc.path), rpc.body))
            },
            |_, raw| crate::decode_contact_request(&raw),
        )
        .await
    }
    /// Remove this account's relationship, preserving remote and local chat history.
    pub async fn remove_contact(&self, username: &str) -> Result<crate::GroupMutationReply> {
        let rpc = crate::remove_contact_request(username)?;
        let group_rpc = crate::GroupRpc {
            cgi: rpc.cgi,
            path: rpc.path.clone(),
            opcode: Some(4),
            body: rpc.body.clone(),
        };
        self.account_single_rpc(
            move |_| {
                super::super::contact_requests::validate_contact_rpc(&rpc)?;
                Ok((rpc.cgi, Cow::Owned(rpc.path), rpc.body))
            },
            move |_, raw| crate::decode_group_mutation(&group_rpc, &raw, &[]),
        )
        .await
    }
    /// Return authenticated contact reply bytes for protected caller-owned receipt storage.
    pub async fn contact_rpc_raw(&self, request: &crate::ContactRpc) -> Result<Vec<u8>> {
        let request = request.clone();
        self.account_single_rpc(
            move |_| {
                super::super::contact_requests::validate_contact_rpc(&request)?;
                Ok((request.cgi, Cow::Owned(request.path), request.body))
            },
            |_, raw| Ok(raw),
        )
        .await
    }

    /// Read one exact OpenIM contact using optional group context and a supplied ticket.
    pub async fn lookup_openim_contact(
        &self,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<crate::OpenImContact> {
        let raw = self
            .lookup_openim_contact_raw(username, room, ticket)
            .await?;
        crate::openim::decode_response(&raw, username)
    }

    /// Return the authenticated OpenIM reply bytes for caller-owned receipt storage and decoding.
    pub async fn lookup_openim_contact_raw(
        &self,
        username: &str,
        room: Option<&str>,
        ticket: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut request = Zeroizing::new(crate::openim::request(username, room, ticket)?);
        self.account_single_rpc(
            move |_| {
                Ok((
                    crate::openim::CGI,
                    Cow::Borrowed(crate::openim::PATH),
                    std::mem::take(&mut *request),
                ))
            },
            |_, raw| Ok(raw),
        )
        .await
    }

    /// Request a new H5 ticket for Channels post lookups. Submitted once, without renewal or retry.
    pub async fn channels_ticket(&self) -> Result<crate::ChannelsTicket> {
        self.account_single_rpc(
            |state| {
                Ok((
                    super::super::channels::H5_AUTH_CGI,
                    Cow::Borrowed(super::super::channels::H5_AUTH_PATH),
                    super::super::channels::h5_auth_request(state)?,
                ))
            },
            |_, raw| super::super::channels::h5_auth_response(&raw),
        )
        .await
    }

    /// Request the authenticated page address for a Channels post. Submitted once.
    pub async fn channels_page(&self, post: &crate::ChannelsPost) -> Result<crate::ChannelsPage> {
        let post = post.clone();
        self.account_single_rpc(
            move |state| {
                Ok((
                    super::super::channels::PAGE_CGI,
                    Cow::Borrowed(super::super::channels::PAGE_PATH),
                    super::super::channels::page_request(state, &post)?,
                ))
            },
            |_, raw| super::super::channels::page_response(&raw),
        )
        .await
    }

    /// Request a Channels post's detail and return its single video's fresh playback material.
    /// Submitted once.
    pub async fn channels_media(
        &self,
        post: &crate::ChannelsPost,
        ticket: &crate::ChannelsTicket,
        page: &crate::ChannelsPage,
    ) -> Result<crate::ChannelsMedia> {
        let millis = if let Some(transport) = &self.transport {
            transport.clock.unix_time_ms()?
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| anyhow::anyhow!("channels_unsupported_media"))?
                    .as_millis() as u64
            }
            #[cfg(target_arch = "wasm32")]
            {
                anyhow::bail!("Native client has no async transport configured")
            }
        };
        let post_req = post.clone();
        let post_dec = post.clone();
        let ticket = ticket.clone();
        let page = page.clone_inner();
        self.account_single_rpc(
            move |state| {
                let json =
                    Zeroizing::new(super::super::channels::detail_json_at(&post_req, millis)?);
                let mut body =
                    super::super::channels::transfer_request(state, &ticket, &page, &json)?;
                Ok((
                    super::super::channels::TRANSFER_CGI,
                    Cow::Borrowed(super::super::channels::TRANSFER_PATH),
                    std::mem::take(&mut *body),
                ))
            },
            move |_, raw| {
                let reply = super::super::channels::transfer_response(&Zeroizing::new(raw))?;
                super::super::channels::parse_detail(&reply, &post_dec)
            },
        )
        .await
    }
}
