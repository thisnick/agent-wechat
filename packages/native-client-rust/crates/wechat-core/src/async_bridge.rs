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
