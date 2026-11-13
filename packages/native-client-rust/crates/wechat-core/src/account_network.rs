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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn network_wait_releases_writer_but_retains_operation_order_and_logout_fence() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&directory.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::new(store, None).unwrap();
        core.save_state("one", &crate::runtime::tests::state())
            .unwrap();
        let fence = core.session_fence("one").unwrap();
        let operation = core.lock_operation("one", "group_recall").await.unwrap();
        let guard = core.lock_account("one").await.unwrap();
        let (began, started) = tokio::sync::oneshot::channel();
        let (finish, released) = std::sync::mpsc::channel();
        let worker = core.clone();
        let task = tokio::task::spawn_blocking(move || {
            let _operation = operation;
            let mut permit = AccountNetworkPermit {
                guard: Some(guard),
                runtime: tokio::runtime::Handle::current(),
            };
            permit
                .transfer(&worker, "one", || {
                    began.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(())
                })
                .unwrap();
            worker.session_state("one", &fence).map(|_| ())
        });
        started.await.unwrap();
        let guard = tokio::time::timeout(Duration::from_secs(1), core.lock_account("one"))
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(30),
            core.lock_operation("one", "group_recall")
        )
        .await
        .is_err());
        core.store
            .put_runtime("one", "logout-epoch", b"new epoch")
            .unwrap();
        drop(guard);
        finish.send(()).unwrap();
        assert!(task.await.unwrap().is_err());
        assert!(tokio::time::timeout(
            Duration::from_secs(1),
            core.lock_operation("one", "group_recall")
        )
        .await
        .is_ok());
    }
}
