//! Release the account writer during blocking network/codec work, then reacquire
//! it before validating the caller's credential and logout fences.
use crate::Core;
use anyhow::{Context, Result};

pub(crate) struct AccountNetworkPermit {
    pub(crate) guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    pub(crate) runtime: tokio::runtime::Handle,
}
impl AccountNetworkPermit {
    /// Caller retains any operation-specific admission and validates its fence
    /// after return, before committing the result.
    pub(crate) fn transfer<T>(
        &mut self,
        core: &Core,
        account: &str,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        drop(self.guard.take().context("account writer permit missing")?);
        let result = action();
        self.guard = Some(self.runtime.block_on(core.lock_account(account))?);
        result
    }
}
