//! Persistent notifications share the account owner, not a saved credential copy.
use super::*;
use crate::{errors::AccountInterrupted, push, wire};
use std::time::Instant;

/// Persistent account connection for callers that already own blocking workers.
/// Requests and notification waits use the same account admission and handoff
/// barriers as ordinary operations. Reconnect after a credential generation
/// changes; an old authenticated socket cannot adopt newer credentials.
pub struct AccountPushConnection {
    client: NativeClient,
    connection: push::Connection,
    credential_generation: u64,
}

impl NativeClient {
    /// Open an authenticated notification transport on the caller's blocking
    /// worker. Creating the transport does not start a hidden executor or loop.
    pub fn open_push_connection_blocking(&self) -> Result<AccountPushConnection> {
        let (exchange, state) = self
            .owned_account()?
            .begin()
            .map_err(|error| error.context(NativeRequestNotSubmitted))?;
        let credential_generation = exchange.credential_generation;
        let options = self.options.clone();
        let connection = Self::execute_account_request(
            options.clone(),
            exchange,
            state,
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
}

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
            Arc::new(AtomicBool::new(false)),
            |_, state, check| operation(&mut self.connection, state, check),
        )
    }

    pub fn heartbeat(&mut self, cursor: &[u8]) -> Result<wire::Heartbeat> {
        self.request(|connection, state, check| connection.heartbeat_guarded(state, cursor, check))
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

#[cfg(test)]
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
}
