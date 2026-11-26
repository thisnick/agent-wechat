//! Core's account workers are synchronous (`spawn_blocking` workers driving
//! the `Upstream` traits); protocol I/O is asynchronous on the native
//! `ProtocolTransport`. This bridges the two without blocking an executor.
use std::future::Future;

/// Run an async protocol operation to completion from a synchronous worker.
///
/// On a `spawn_blocking` thread or a multi-threaded runtime worker, the future
/// runs on the caller's thread while the runtime keeps driving I/O and timers.
/// Outside any runtime (synchronous tests), it runs on a temporary
/// current-thread runtime. Calling this from a current-thread runtime's async
/// context panics, as blocking there would deadlock.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => tokio::task::block_in_place(|| runtime.block_on(future)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("temporary protocol runtime")
            .block_on(future),
    }
}

/// [`block_on`] on a specific runtime, for synchronous threads that hold a
/// runtime handle but were not started by that runtime (call workers).
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn block_on_with<F: Future>(runtime: &tokio::runtime::Handle, future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| runtime.block_on(future))
    } else {
        runtime.block_on(future)
    }
}

/// Run synchronous store or vault work from inside an async protocol hook
/// (a pre-submission guard or reply retention) without stalling a Tokio
/// worker: on a multi-threaded runtime the worker hands its other tasks off
/// first. Elsewhere it runs inline.
pub(crate) fn blocking_section<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}
