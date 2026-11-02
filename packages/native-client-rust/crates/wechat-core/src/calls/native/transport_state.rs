//! Accepted account lineage around one unlocked host exchange. This never
//! changes call ownership, grants media admission or retries an operation.
use super::*;
use crate::runtime::SessionFence;
use zeroize::Zeroizing;

pub(super) struct CallNetworkState {
    expected: Zeroizing<Vec<u8>>,
    desired: Option<Vec<u8>>,
    fence: SessionFence,
    non_cookie: [u8; 32],
}
impl CallNetworkState {
    /// The caller holds the account writer. Only an already accepted state may
    /// issue a request; cache inspection never rebinds an older credential set.
    pub(super) fn capture(
        core: &Core,
        account: &str,
        lease: &CallAuthorityLease,
        state: &AccountState,
    ) -> Result<Self> {
        let desired = core.store.runtime(account, "desired")?;
        ensure!(
            desired.as_deref() == Some(b"connected"),
            "Call account disconnected"
        );
        ensure!(
            core.store
                .runtime(account, "validated-generation")?
                .as_deref()
                == Some(core.ownership_generation.as_bytes()),
            "Call account generation changed"
        );
        let fence = SessionFence {
            authorization: authorization(state)?,
            logout_epoch: core.store.runtime(account, "logout-epoch")?,
        };
        let expected = core
            .store
            .runtime(account, "account-state")?
            .context("Missing call account state")?;
        ensure!(
            lease.current()
                && core.lease_for_call_state(account, state)?.authority() == lease.authority(),
            "Call accepted policy changed"
        );
        ensure!(
            core.store.saved_login_authorization_current(
                account,
                &expected,
                fence.logout_epoch.as_deref()
            )?,
            "Call account state not accepted"
        );
        Ok(Self {
            expected: Zeroizing::new(expected),
            desired,
            fence,
            non_cookie: non_cookie_digest(state)?,
        })
    }

    /// Called only after reacquiring the writer and retaining the raw reply.
    /// Owner/scope loss does not discard an authenticated allocation needed for
    /// cleanup; it still prevents every new nonterminal handoff/media action.
    pub(super) fn accept(
        self,
        core: &Core,
        account: &str,
        lease: &CallAuthorityLease,
        returned: &AccountState,
    ) -> Result<AccountState> {
        ensure!(
            lease.current() && !core.stopping.load(Ordering::Acquire),
            "Call authority changed during host operation"
        );
        ensure!(
            core.store.runtime(account, "desired")? == self.desired
                && core.store.runtime(account, "logout-epoch")? == self.fence.logout_epoch
                && core
                    .store
                    .runtime(account, "validated-generation")?
                    .as_deref()
                    == Some(core.ownership_generation.as_bytes()),
            "Call account lineage changed"
        );
        let mut latest = core.session_state(account, &self.fence)?;
        ensure!(
            core.lease_for_call_state(account, &latest)?.authority() == lease.authority(),
            "Call accepted policy changed"
        );
        let current = core
            .store
            .runtime(account, "account-state")?
            .context("Missing current call account state")?;
        ensure!(
            core.store.saved_login_authorization_current(
                account,
                &current,
                self.fence.logout_epoch.as_deref()
            )?,
            "Current call state not accepted"
        );
        // This RPC only changes its host cookie. Treat any other returned-state
        // mutation as unsupported before preparing or binding call authority.
        ensure!(
            non_cookie_digest(returned)? == self.non_cookie,
            "Unexpected call host state change"
        );
        if current != *self.expected {
            // A different accepted state is reusable only when its difference
            // is the ordinary concurrent cookie update. Preserve that entire
            // newer state and its accepted ciphertext; never overwrite it.
            ensure!(
                non_cookie_digest(&latest)? == self.non_cookie,
                "Call account changed beyond its cookie"
            );
            return Ok(latest);
        }
        latest.cookie = returned.cookie.clone();
        latest.validate()?;
        // The exact-cipher precheck above precedes this helper's policy-cache
        // preparation. Its store transaction still checks ciphertext, accepted
        // generation, desired mode and logout epoch atomically.
        core.save_network_state(
            account,
            &latest,
            &self.expected,
            &self.fence,
            self.desired.as_deref(),
        )?;
        Ok(latest)
    }
}

fn non_cookie_digest(state: &AccountState) -> Result<[u8; 32]> {
    let mut copy = state.clone();
    copy.cookie.fill(0);
    copy.cookie.clear();
    let serialized = Zeroizing::new(serde_json::to_vec(&copy)?);
    Ok(Sha256::digest(serialized.as_slice()).into())
}
