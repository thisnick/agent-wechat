//! Persistent notifications share the account owner, not a saved credential copy.
use super::*;
use crate::{errors::AccountInterrupted, push, wire};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// Persistent account connection for callers that already own blocking workers.
/// Requests and notification waits use the same account admission and handoff
/// barriers as ordinary operations. Reconnect after a credential generation
/// changes; an old authenticated socket cannot adopt newer credentials.
#[cfg(not(target_arch = "wasm32"))]
pub struct AccountPushConnection {
    client: NativeClient,
    connection: push::Connection,
    credential_generation: u64,
}

/// Persistent asynchronous account notification connection backed by a
/// pluggable [`crate::transport::ProtocolTransport`].
///
/// Requests and notification waits enforce the same account admission,
/// credential-generation, and handoff barriers as [`AccountPushConnection`].
/// Reconnect after a credential renewal supersedes the connection's generation.
/// Methods take `&mut self` and run inline on the connection stream; do not
/// cancel an in-flight `heartbeat`, `sync`, or `send_sticker_chunk_raw` call
/// after submission (close and reconnect if dropped).
pub struct AsyncPushConnection {
    client: NativeClient,
    connection: push::AsyncConnection,
    clock: Arc<dyn crate::transport::Clock>,
    credential_generation: u64,
}

impl NativeClient {
    /// Open an authenticated notification transport on the caller's blocking
    /// worker. Creating the transport does not start a hidden executor or loop.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_push_connection_blocking(&self) -> Result<AccountPushConnection> {
        if self.transport.is_some() {
            return Err(anyhow::anyhow!(
                "Blocking push connection cannot bypass a configured async transport"
            )
            .context(NativeRequestNotSubmitted));
        }
        let owner = self.owned_account()?;
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let credential_generation = exchange.credential_generation;
        let options = self.options.clone();
        let connection = Self::execute_account_request(
            options.clone(),
            exchange,
            state,
            owner.short_link.clone(),
            Arc::new(AtomicBool::new(false)),
            |_, state, check| {
                check()?;
                let connection =
                    push::Connection::open(state, options.timeout, options.receive_budget)?;
                // No business operation has been submitted by opening a socket.
                check()?;
                Ok(connection)
            },
        )?;
        Ok(AccountPushConnection {
            client: self.clone(),
            connection,
            credential_generation,
        })
    }

    /// Open an authenticated asynchronous notification connection using this
    /// client's [`crate::transport::ProtocolTransport`].
    ///
    /// Creating the connection does not start a background task or loop; call
    /// [`AsyncPushConnection::poll`] to wait for incoming notifications.
    pub async fn open_push_connection(&self) -> Result<AsyncPushConnection> {
        let transport = self.require_transport()?.clone();
        let owner = self.owned_account()?;
        let (exchange, state) = owner
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let credential_generation = exchange.credential_generation;
        exchange
            .check()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let connection = push::AsyncConnection::open(
            &state,
            self.options.timeout,
            self.options.receive_budget,
            &*transport.streams,
            transport.clock.clone(),
        )
        .await?;
        // No business operation has been submitted by opening a socket.
        if let Err(error) = exchange.check() {
            let _ = connection.close().await;
            return Err(error.context(NativeRequestNotSubmitted));
        }
        Ok(AsyncPushConnection {
            client: self.clone(),
            connection,
            clock: transport.clock,
            credential_generation,
        })
    }
}

impl AsyncPushConnection {
    /// Send a long-link heartbeat (CGI 518 / command 238) and associate the
    /// authenticated account with this persistent connection.
    pub async fn heartbeat(&mut self, cursor: &[u8]) -> Result<wire::Heartbeat> {
        let (exchange, mut state) = begin_push_request(&self.client, self.credential_generation)?;
        let result = self
            .connection
            .heartbeat_guarded(&mut state, cursor, || {
                exchange
                    .check()
                    .map_err(|error| error.context(NativeRequestNotSubmitted))
            })
            .await;
        finish_push_exchange(exchange, &state, result)
    }

    /// Submit one journaled sticker probe or byte continuation on this long
    /// link, returning raw bytes for durable retention before validating.
    pub async fn send_sticker_chunk_raw(
        &mut self,
        chunk: &crate::sticker_send::StickerSendChunk<'_>,
    ) -> Result<Vec<u8>> {
        let (exchange, mut state) = begin_push_request(&self.client, self.credential_generation)?;
        let result = self
            .connection
            .sticker_chunk_guarded(&mut state, chunk, || {
                exchange
                    .check()
                    .map_err(|error| error.context(NativeRequestNotSubmitted))
            })
            .await;
        finish_push_exchange(exchange, &state, result)
    }

    /// Fetch one NewSync page over this long-link connection (CGI 138 / command 121).
    pub async fn sync(&mut self, cursor: &[u8], scene: u32) -> Result<wire::SyncBatch> {
        let (exchange, mut state) = begin_push_request(&self.client, self.credential_generation)?;
        let result = self
            .connection
            .sync_guarded(&mut state, cursor, scene, || {
                exchange
                    .check()
                    .map_err(|error| error.context(NativeRequestNotSubmitted))
            })
            .await;
        finish_push_exchange(exchange, &state, result)
    }

    /// Wait up to `timeout` (at most 300s) for the next push notification while
    /// observing account shutdown and credential renewals promptly.
    pub async fn poll(&mut self, timeout: Duration) -> Result<Option<push::Notification>> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(300),
            "Invalid account notification timeout"
        );
        let (exchange, state) = begin_push_request(&self.client, self.credential_generation)?;
        let owner = self.client.owned_account()?.clone();
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline_ms = self.clock.monotonic_ms().saturating_add(timeout_ms);
        let mut waited = false;
        let result = loop {
            if let Err(error) = exchange.check() {
                if !waited {
                    break Err(error.context(NativeRequestNotSubmitted));
                }
                let message = "Account notification session superseded";
                let err = match AccountInterrupted::find(&error) {
                    Some(reason) => reason.because(message),
                    None => anyhow::anyhow!(message),
                };
                break Err(err);
            }
            let now = self.clock.monotonic_ms();
            if now >= deadline_ms {
                break Ok(None);
            }
            let slice = Duration::from_millis(deadline_ms - now).min(Duration::from_secs(1));
            waited = true;
            let step = tokio::select! {
                biased;
                () = owner.stopped() => {
                    let message = "Account notification session superseded";
                    let reason = exchange
                        .check()
                        .err()
                        .and_then(|e| AccountInterrupted::find(&e))
                        .unwrap_or(AccountInterrupted::Stopped);
                    break Err(reason.because(message));
                }
                res = self.connection.poll(slice) => res,
            };
            let notification = match step {
                Ok(n) => n,
                Err(error) => break Err(error),
            };
            if let Err(error) = exchange.check() {
                let message = "Account notification session superseded";
                let err = match AccountInterrupted::find(&error) {
                    Some(reason) => reason.because(message),
                    None => anyhow::anyhow!(message),
                };
                break Err(err);
            }
            if notification.is_some() {
                break Ok(notification);
            }
        };
        finish_push_exchange(exchange, &state, result)
    }

    /// Best-effort graceful close of the underlying stream.
    pub async fn close(self) -> Result<()> {
        self.connection.close().await
    }
}

fn finish_push_exchange<T>(
    exchange: Exchange,
    state: &AccountState,
    result: Result<T>,
) -> Result<T> {
    let accepted = exchange.accept_cookie(state);
    match result {
        Ok(value) => {
            accepted?;
            Ok(value)
        }
        Err(error) => match accepted {
            Ok(()) => Err(error),
            Err(account_error) => Err(error.context(account_error)),
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AccountPushConnection {
    fn request<T>(
        &mut self,
        operation: impl FnOnce(
            &mut push::Connection,
            &mut AccountState,
            &dyn Fn() -> Result<()>,
        ) -> Result<T>,
    ) -> Result<T> {
        let (exchange, state) = begin_push_request(&self.client, self.credential_generation)?;
        NativeClient::execute_account_request(
            self.client.options.clone(),
            exchange,
            state,
            self.client.owned_account()?.short_link.clone(),
            Arc::new(AtomicBool::new(false)),
            |_, state, check| operation(&mut self.connection, state, check),
        )
    }

    pub fn heartbeat(&mut self, cursor: &[u8]) -> Result<wire::Heartbeat> {
        self.request(|connection, state, check| connection.heartbeat_guarded(state, cursor, check))
    }

    /// Submit one journaled sticker probe or byte continuation on this long
    /// link. Same contract as `AccountRequest::send_sticker_chunk_raw`: return
    /// raw bytes for durable retention before validating the acknowledgement.
    /// A transport error after submission leaves the outcome uncertain.
    pub fn send_sticker_chunk_raw(
        &mut self,
        chunk: &crate::sticker_send::StickerSendChunk<'_>,
    ) -> Result<Vec<u8>> {
        self.request(|connection, state, check| {
            connection.sticker_chunk_guarded(state, chunk, check)
        })
    }

    pub fn sync(&mut self, cursor: &[u8], scene: u32) -> Result<wire::SyncBatch> {
        self.request(|connection, state, check| {
            connection.sync_guarded(state, cursor, scene, check)
        })
    }

    /// Poll without an account lock. One-second slices observe account shutdown
    /// during long waits. A notification is returned only while its socket's
    /// credential generation remains current.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<push::Notification>> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(300),
            "Invalid account notification timeout"
        );
        self.request(|connection, _, check| {
            wait_for_notification(timeout, check, |slice| connection.poll(slice))
        })
    }
}

/// Wait in one-second slices so a stop or renewal ends the wait promptly. The
/// reason for an interruption stays attached as [`AccountInterrupted`].
#[cfg(not(target_arch = "wasm32"))]
fn wait_for_notification<T>(
    timeout: Duration,
    check: &dyn Fn() -> Result<()>,
    mut wait: impl FnMut(Duration) -> Result<Option<T>>,
) -> Result<Option<T>> {
    let deadline = Instant::now() + timeout;
    loop {
        check()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let notification = wait(remaining.min(Duration::from_secs(1)))?;
        // After waiting, this is an observed operation, not proof that
        // a business request was never submitted. Preserve uncertainty.
        check().map_err(|error| {
            let message = "Account notification session superseded";
            match AccountInterrupted::find(&error) {
                Some(reason) => reason.because(message),
                None => anyhow::anyhow!(message),
            }
        })?;
        if notification.is_some() {
            return Ok(notification);
        }
    }
}

fn begin_push_request(client: &NativeClient, connected: u64) -> Result<(Exchange, AccountState)> {
    let (exchange, state) = client
        .owned_account()?
        .begin()
        .map_err(|error| error.context(NativeRequestNotSubmitted))?;
    if exchange.credential_generation != connected {
        return Err(AccountInterrupted::CredentialsSuperseded
            .because("Account notification credentials superseded"));
    }
    Ok((exchange, state))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn renewal_with_identical_keys_invalidates_an_existing_notification_transport() {
        let client = super::super::tests::client();
        let (first, _) = begin_push_request(&client, 0).unwrap();
        drop(first);
        let (renewal, same_credentials) = client.owned_account().unwrap().begin().unwrap();
        renewal.accept_credentials(same_credentials).unwrap();
        drop(renewal);
        let error = begin_push_request(&client, 0).err().unwrap();
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::CredentialsSuperseded)
        );
        assert_eq!(
            error.to_string(),
            "Account notification credentials superseded"
        );
        let (current, _) = begin_push_request(&client, 1).unwrap();
        drop(current);
        client.stop().unwrap();
        let error = begin_push_request(&client, 1).err().unwrap();
        assert!(error.is::<NativeRequestNotSubmitted>());
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::Stopped)
        );
        assert_eq!(client.account_status().unwrap().in_flight, 0);
    }

    fn renew(client: &NativeClient) {
        let (renewal, same_credentials) = client.owned_account().unwrap().begin().unwrap();
        renewal.accept_credentials(same_credentials).unwrap();
    }

    /// Run the notification wait inside an account request, with `during_wait`
    /// standing in for the time spent blocked on the socket.
    fn interrupted_wait(
        client: &NativeClient,
        before_wait: impl FnOnce(),
        during_wait: impl Fn(),
    ) -> anyhow::Error {
        before_wait();
        client
            .account_request_blocking(|_, _, check| {
                wait_for_notification(Duration::from_secs(5), check, |_| {
                    during_wait();
                    Ok(Some(()))
                })
            })
            .unwrap_err()
    }

    #[test]
    fn stop_during_a_notification_wait_reports_stopped_without_claiming_unsent() {
        let client = super::super::tests::client();
        let error = interrupted_wait(&client, || (), || client.stop().unwrap());
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::Stopped)
        );
        assert!(!error.is::<NativeRequestNotSubmitted>());
        assert!(format!("{error:#}").contains("Account notification session superseded"));
        assert_eq!(client.account_status().unwrap().in_flight, 0);
    }

    #[test]
    fn renewal_during_a_notification_wait_reports_superseded_credentials() {
        let client = super::super::tests::client();
        let error = interrupted_wait(&client, || (), || renew(&client));
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::CredentialsSuperseded)
        );
        assert!(!error.is::<NativeRequestNotSubmitted>());
        assert!(format!("{error:#}").contains("Account notification session superseded"));
    }

    #[test]
    fn transfer_before_a_request_reports_transferred() {
        let client = super::super::tests::client();
        client.shutdown_blocking().unwrap();
        client.owned_account().unwrap().lock().unwrap().lifecycle = AccountLifecycle::Transferred;
        let error = begin_push_request(&client, 0).err().unwrap();
        assert_eq!(
            AccountInterrupted::find(&error),
            Some(AccountInterrupted::Transferred)
        );
    }

    #[test]
    fn a_wait_that_is_not_interrupted_returns_its_notification_or_times_out() {
        let client = super::super::tests::client();
        let received = client
            .account_request_blocking(|_, _, check| {
                wait_for_notification(Duration::from_secs(5), check, |_| Ok(Some(7)))
            })
            .unwrap();
        assert_eq!(received, Some(7));
        let quiet = client
            .account_request_blocking(|_, _, check| {
                wait_for_notification(Duration::from_millis(20), check, |slice| {
                    std::thread::sleep(slice);
                    Ok(None::<()>)
                })
            })
            .unwrap();
        assert_eq!(quiet, None);
    }

    #[tokio::test]
    async fn async_push_poll_interrupted_after_wait_omits_not_submitted_and_closes() {
        use crate::transport::{AsyncByteStream, Clock, ReadOutcome, TransportFuture};
        use std::sync::atomic::{AtomicU64, AtomicUsize};

        struct StepClock(AtomicU64);
        impl Clock for StepClock {
            fn monotonic_ms(&self) -> u64 {
                self.0.load(Ordering::Acquire)
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(1_700_000_000_000 + self.monotonic_ms())
            }
        }

        struct SliceInterruptStream {
            calls: AtomicUsize,
            clock: Arc<StepClock>,
            on_timeout: Mutex<Option<Box<dyn FnOnce() + Send>>>,
            closed: Arc<AtomicBool>,
        }
        impl AsyncByteStream for SliceInterruptStream {
            fn read<'a>(
                &'a mut self,
                _buf: &'a mut [u8],
                _timeout: Duration,
            ) -> TransportFuture<'a, Result<ReadOutcome>> {
                let n = self.calls.fetch_add(1, Ordering::AcqRel);
                if n == 0 {
                    self.clock.0.fetch_add(1_000, Ordering::AcqRel);
                    if let Some(cb) = self.on_timeout.lock().unwrap().take() {
                        cb();
                    }
                }
                Box::pin(async { Ok(ReadOutcome::TimedOut) })
            }
            fn write_all<'a>(
                &'a mut self,
                _buf: &'a [u8],
                _timeout: Duration,
            ) -> TransportFuture<'a, Result<()>> {
                Box::pin(async { Ok(()) })
            }
            fn close<'a>(&'a mut self) -> TransportFuture<'a, Result<()>> {
                self.closed.store(true, Ordering::Release);
                Box::pin(async { Ok(()) })
            }
        }

        for (interrupt_with_stop, expected_reason) in [
            (false, AccountInterrupted::CredentialsSuperseded),
            (true, AccountInterrupted::Stopped),
        ] {
            let client = super::super::tests::client();
            let clock = Arc::new(StepClock(AtomicU64::new(100)));
            let closed = Arc::new(AtomicBool::new(false));
            let handle = client.clone();
            let stream = SliceInterruptStream {
                calls: AtomicUsize::new(0),
                clock: clock.clone(),
                on_timeout: Mutex::new(Some(Box::new(move || {
                    if interrupt_with_stop {
                        handle.stop().unwrap();
                    } else {
                        renew(&handle);
                    }
                }))),
                closed: closed.clone(),
            };
            let session = crate::mmtls::tests::async_session_for_test(
                Box::new(stream),
                clock.clone(),
                vec![1; 16],
                vec![2; 12],
            );
            let mut push_conn = AsyncPushConnection {
                client: client.clone(),
                connection: push::AsyncConnection::from_session_for_test(session, clock.clone()),
                clock,
                credential_generation: 0,
            };
            let err = push_conn.poll(Duration::from_secs(5)).await.unwrap_err();
            assert_eq!(AccountInterrupted::find(&err), Some(expected_reason));
            assert!(!err.is::<NativeRequestNotSubmitted>());
            assert!(format!("{err:#}").contains("Account notification session superseded"));
            assert_eq!(client.account_status().unwrap().in_flight, 0);
            push_conn.close().await.unwrap();
            assert!(closed.load(Ordering::Acquire));
        }
    }
}
