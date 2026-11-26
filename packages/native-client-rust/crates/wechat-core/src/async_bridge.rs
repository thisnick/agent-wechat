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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn timed_value(value: u32) -> u32 {
        // Needs the runtime's timer, and a task spawned onto the runtime.
        tokio::time::sleep(Duration::from_millis(5)).await;
        tokio::spawn(async move { value }).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridges_work_from_every_thread_kind_the_daemon_uses() {
        // A spawn_blocking worker, as Core's account workers run.
        let from_blocking = tokio::task::spawn_blocking(|| block_on(timed_value(1)))
            .await
            .unwrap();
        assert_eq!(from_blocking, 1);

        // A runtime worker, as detached protocol hooks run: blocking sections
        // hand the worker's other tasks off instead of stalling them.
        let from_worker = tokio::spawn(async {
            let ticker = tokio::spawn(timed_value(2));
            let section = blocking_section(|| {
                std::thread::sleep(Duration::from_millis(20));
                3
            });
            (section, ticker.await.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(from_worker, (3, 2));

        // A plain thread holding a runtime handle, as call workers do.
        let handle = tokio::runtime::Handle::current();
        let from_thread = std::thread::spawn(move || block_on_with(&handle, timed_value(4)))
            .join()
            .unwrap();
        assert_eq!(from_thread, 4);
    }

    #[test]
    fn bridges_work_without_a_runtime_and_sections_run_inline() {
        assert_eq!(block_on(timed_value(5)), 5);
        assert_eq!(blocking_section(|| 6), 6);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // A current-thread runtime cannot hand work off; sections run inline.
        assert_eq!(runtime.block_on(async { blocking_section(|| 7) }), 7);
    }
}
