//! Cheap media fences derived from accepted account credentials and routing
//! policy. Cookies do not change the generation; policy replacement/logout do.
use super::ownership::AccountAuthority;
use crate::{runtime::authorization, Core};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use wechat_protocol::AccountState;

fn call_authorization(state: &AccountState) -> Result<[u8; 32]> {
    // Calls cannot keep using an old ordinary/Ax selection after a policy-only
    // update. Keep this stricter fence separate from general network admission.
    let mut digest = Sha256::new();
    digest.update(b"agent-wechat-call-authority-v1");
    digest.update(authorization(state)?);
    let policy = &state.auth_snapshot;
    digest.update(policy.update_mask.to_le_bytes());
    for section in [
        &policy.response,
        &policy.auth,
        &policy.account,
        &policy.network,
        &policy.ax_auth,
    ] {
        digest.update(u64::try_from(section.len())?.to_le_bytes());
        digest.update(section);
    }
    Ok(digest.finalize().into())
}
struct Generation {
    epoch: AtomicU64,
    active: AtomicBool,
    submission: Mutex<()>,
}
impl Generation {
    fn fence(&self) {
        self.active.store(false, Ordering::Release);
        // Close future admission immediately, then wait for a submission that
        // was already admitted to finish before revocation returns.
        let _submission = self.submission.lock().unwrap_or_else(|e| e.into_inner());
    }
}
pub(crate) struct Entry {
    generation: Arc<Generation>,
    digest: Option<[u8; 32]>,
    inhibited: bool,
}
impl Default for Entry {
    fn default() -> Self {
        Self {
            generation: Arc::new(Generation {
                epoch: AtomicU64::new(0),
                active: AtomicBool::new(false),
                submission: Mutex::new(()),
            }),
            digest: None,
            inhibited: false,
        }
    }
}
#[derive(Clone)]
pub struct CallAuthorityLease {
    authority: AccountAuthority,
    generation: Arc<Generation>,
    epoch: u64,
    digest: [u8; 32],
}
impl CallAuthorityLease {
    pub fn authority(&self) -> &AccountAuthority {
        &self.authority
    }
    pub fn current(&self) -> bool {
        self.generation.active.load(Ordering::Acquire)
            && self.generation.epoch.load(Ordering::Acquire) == self.epoch
    }
    /// Admit one synchronous physical submission while excluding revocation.
    /// The callback must not await or acquire Core/service locks. This cannot
    /// retract bytes already handed to a socket or an asynchronous HTTP client.
    pub fn with_current<T>(&self, submit: impl FnOnce() -> T) -> Option<T> {
        let _submission = self.generation.submission.lock().ok()?;
        self.current().then(submit)
    }
}
impl Core {
    /// Initial admission may read/profile-check credentials. Audio workers retain
    /// the returned lease and use current(), which performs no filesystem access.
    pub fn call_authority(&self, account: &str) -> Result<CallAuthorityLease> {
        if self.stopping.load(Ordering::Acquire)
            || self.store.runtime(account, "logged-out")?.is_some()
            || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            || self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                != Some(self.ownership_generation.as_bytes())
        {
            bail!("auth_required");
        }
        let state = self.account_state(account)?;
        self.lease_for_call_state(account, &state)
    }
    pub(crate) fn lease_for_call_state(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<CallAuthorityLease> {
        // A read may race with a newer accepted credential write. It must never
        // rotate the accepted cache back to the older state it happened to read.
        let lease = self
            .cached_call_authority(account)
            .context("auth_required")?;
        if lease.digest != call_authorization(state)? || !lease.current() {
            bail!("auth_required");
        }
        Ok(lease)
    }
    pub fn cached_call_authority(&self, account: &str) -> Option<CallAuthorityLease> {
        let map = self.call_epochs.lock().ok()?;
        let e = map.get(account)?;
        if !e.generation.active.load(Ordering::Acquire) {
            return None;
        }
        let epoch = e.generation.epoch.load(Ordering::Acquire);
        let digest = e.digest?;
        let generation = format!(
            "{}:{}:{}",
            self.ownership_generation,
            epoch,
            hex_digest(&digest)
        );
        Some(CallAuthorityLease {
            authority: AccountAuthority {
                account_id: account.into(),
                generation,
            },
            generation: e.generation.clone(),
            epoch,
            digest,
        })
    }
    pub(crate) fn fence_call_authority(&self, account: &str) {
        if let Ok(mut map) = self.call_epochs.lock() {
            let entry = map.entry(account.into()).or_default();
            entry.generation.fence();
            entry.digest = None;
            entry.inhibited = true;
        }
    }
    pub(crate) fn fence_all_call_authorities(&self) {
        if let Ok(mut entries) = self.call_epochs.lock() {
            for entry in entries.values_mut() {
                entry.generation.fence();
                entry.digest = None;
                entry.inhibited = true;
            }
        }
    }
    /// Invalidate before publishing a different credential state. A failed write
    /// stays closed; a cookie-only update preserves current media admission.
    pub(crate) fn prepare_call_authorization(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<()> {
        let digest = call_authorization(state)?;
        let mut map = self
            .call_epochs
            .lock()
            .map_err(|_| anyhow::anyhow!("call authority unavailable"))?;
        let e = map.entry(account.into()).or_default();
        if e.digest != Some(digest) {
            e.generation.fence();
            e.digest = None;
        }
        Ok(())
    }
    pub(crate) fn accept_call_authorization(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<()> {
        {
            let mut map = self
                .call_epochs
                .lock()
                .map_err(|_| anyhow::anyhow!("call authority unavailable"))?;
            map.entry(account.into()).or_default().inhibited = false;
        }
        self.bind_call_authorization(account, state)
    }
    pub(crate) fn bind_call_authorization(
        &self,
        account: &str,
        state: &AccountState,
    ) -> Result<()> {
        if self.stopping.load(Ordering::Acquire)
            || self.store.runtime(account, "logged-out")?.is_some()
            || self.store.runtime(account, "desired")?.as_deref() != Some(b"connected")
            || self
                .store
                .runtime(account, "validated-generation")?
                .as_deref()
                != Some(self.ownership_generation.as_bytes())
        {
            return Ok(());
        }
        let digest = call_authorization(state)?;
        let mut map = self
            .call_epochs
            .lock()
            .map_err(|_| anyhow::anyhow!("call authority unavailable"))?;
        let e = map.entry(account.into()).or_default();
        if e.inhibited {
            return Ok(());
        }
        if e.digest == Some(digest) && e.generation.active.load(Ordering::Acquire) {
            return Ok(());
        }
        let _submission = e
            .generation
            .submission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        e.generation.active.store(false, Ordering::Release);
        let epoch = e
            .generation
            .epoch
            .load(Ordering::Acquire)
            .checked_add(1)
            .context("call authority generation exhausted")?;
        e.generation.epoch.store(epoch, Ordering::Release);
        e.digest = Some(digest);
        e.generation.active.store(true, Ordering::Release);
        Ok(())
    }
}
fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_generation_stays_invalid_after_reactivation() {
        let generation = Arc::new(Generation {
            epoch: AtomicU64::new(1),
            active: AtomicBool::new(true),
            submission: Mutex::new(()),
        });
        let lease = CallAuthorityLease {
            authority: AccountAuthority {
                account_id: "a".into(),
                generation: "one".into(),
            },
            generation: generation.clone(),
            epoch: 1,
            digest: [1; 32],
        };
        assert!(lease.current());
        assert_eq!(lease.with_current(|| 42), Some(42));
        generation.fence();
        assert!(!lease.current());
        assert!(lease
            .with_current(|| panic!("revoked submission ran"))
            .is_none());
        generation.epoch.store(2, Ordering::Release);
        generation.active.store(true, Ordering::Release);
        assert!(!lease.current());
    }

    #[test]
    fn revocation_waits_for_an_admitted_physical_submission() {
        use std::{sync::mpsc, thread, time::Duration};
        let generation = Arc::new(Generation {
            epoch: AtomicU64::new(1),
            active: AtomicBool::new(true),
            submission: Mutex::new(()),
        });
        let lease = CallAuthorityLease {
            authority: AccountAuthority {
                account_id: "a".into(),
                generation: "one".into(),
            },
            generation: generation.clone(),
            epoch: 1,
            digest: [1; 32],
        };
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let submit = thread::spawn(move || {
            lease
                .with_current(|| {
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();
        let (started, started_rx) = mpsc::channel();
        let (finished, finished_rx) = mpsc::channel();
        let revoke = thread::spawn(move || {
            started.send(()).unwrap();
            generation.fence();
            assert!(!generation.active.load(Ordering::Acquire));
            finished.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(25)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        submit.join().unwrap();
        revoke.join().unwrap();
    }
}
