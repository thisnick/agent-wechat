//! Serialize interruption acknowledgments with nonblocking media handoff.
use anyhow::Result;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};

/// Clear's acknowledgment and physical UDP handoff share this gate. A business
/// HTTP handoff uses a separate account action guard and is never retracted here.
#[derive(Default)]
pub struct SubmissionGate {
    gate: Mutex<()>,
}
impl SubmissionGate {
    pub fn advance(&self, generation: &AtomicU32, next: u32) -> Result<()> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("Call submission gate unavailable"))?;
        generation.fetch_max(next, Ordering::AcqRel);
        Ok(())
    }
    pub fn submit<T>(
        &self,
        allowed: impl FnOnce() -> Result<bool>,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| anyhow::anyhow!("Call submission gate unavailable"))?;
        if !allowed()? {
            return Ok(None);
        }
        write().map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread, time::Duration};
    #[test]
    fn clear_ack_waits_for_entered_handoff_and_later_old_frames_never_submit() {
        let admission = Arc::new(SubmissionGate::default());
        let generation = Arc::new(AtomicU32::new(1));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer_admission = admission.clone();
        let writer_generation = generation.clone();
        let writer = thread::spawn(move || {
            writer_admission
                .submit(
                    || Ok(writer_generation.load(Ordering::Acquire) == 1),
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    },
                )
                .unwrap()
        });
        entered_rx.recv().unwrap();
        let (clear_started_tx, clear_started_rx) = std::sync::mpsc::channel();
        let (acked_tx, acked_rx) = std::sync::mpsc::channel();
        let clear_admission = admission.clone();
        let clear_generation = generation.clone();
        let clear = thread::spawn(move || {
            clear_started_tx.send(()).unwrap();
            clear_admission.advance(&clear_generation, 2).unwrap();
            acked_tx.send(()).unwrap();
        });
        clear_started_rx.recv().unwrap();
        assert!(matches!(
            acked_rx.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(()).unwrap();
        assert!(writer.join().unwrap().is_some());
        acked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        clear.join().unwrap();
        assert!(admission
            .submit(
                || Ok(generation.load(Ordering::Acquire) == 1),
                || -> Result<()> { panic!("stale audio submitted") }
            )
            .unwrap()
            .is_none());
    }
}
