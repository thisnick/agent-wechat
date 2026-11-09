//! Persistent notifications share the account owner, not a saved credential copy.
use super::*;
use crate::{push, wire};
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
            let deadline = Instant::now() + timeout;
            loop {
                check()?;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(None);
                }
                let notification = connection.poll(remaining.min(Duration::from_secs(1)))?;
                // After waiting, this is an observed operation, not proof that
                // a business request was never submitted. Preserve uncertainty.
                check().map_err(|_| anyhow::anyhow!("Account notification session superseded"))?;
                if notification.is_some() {
                    return Ok(notification);
                }
            }
        })
    }
}

fn begin_push_request(client: &NativeClient, connected: u64) -> Result<(Exchange, AccountState)> {
    let (exchange, state) = client
        .owned_account()?
        .begin()
        .map_err(|error| error.context(NativeRequestNotSubmitted))?;
    ensure!(
        exchange.credential_generation == connected,
        "Account notification credentials superseded"
    );
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
        assert!(begin_push_request(&client, 0).is_err());
        let (current, _) = begin_push_request(&client, 1).unwrap();
        drop(current);
        client.stop().unwrap();
        let error = begin_push_request(&client, 1).err().unwrap();
        assert!(error.is::<NativeRequestNotSubmitted>());
        assert_eq!(client.account_status().unwrap().in_flight, 0);
    }
}
