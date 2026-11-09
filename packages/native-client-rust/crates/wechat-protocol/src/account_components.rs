//! Shared extension state belongs to the account owner. Weak entries avoid an
//! owner -> runtime -> client -> owner cycle without introducing global caches.
use super::*;
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    sync::Weak,
};

#[derive(Default)]
pub(super) struct AccountComponents {
    entries: Mutex<HashMap<TypeId, Weak<dyn Any + Send + Sync>>>,
}

impl NativeClient {
    /// Resolve one typed component shared by handles of this account owner.
    ///
    /// This extension seam keeps higher-level runtimes out of protocol's
    /// dependencies. The factory may allocate state and read account metadata;
    /// it must not reenter this method, start workers or perform I/O. Start any
    /// caller-owned execution only after this method returns the shared value.
    /// A component must apply its own option-consistency and admission checks.
    /// The cache retains no strong reference and never copies credentials.
    #[doc(hidden)]
    pub fn shared_account_component<T: Send + Sync + 'static>(
        &self,
        factory: impl FnOnce() -> Result<T>,
    ) -> Result<Arc<T>> {
        let lease = self.account_lease()?;
        let account = self.owned_account()?;
        let mut entries = account
            .components
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("Account component cache unavailable"))?;
        ensure!(lease.current(), "Account is stopped or credentials changed");
        let key = TypeId::of::<T>();
        if let Some(value) = entries.get(&key).and_then(Weak::upgrade) {
            return value
                .downcast::<T>()
                .map_err(|_| anyhow::anyhow!("Account component type mismatch"));
        }
        entries.retain(|_, value| value.strong_count() > 0);
        let value = Arc::new(factory()?);
        ensure!(lease.current(), "Account is stopped or credentials changed");
        let erased: Arc<dyn Any + Send + Sync> = value.clone();
        entries.insert(key, Arc::downgrade(&erased));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, thread};

    struct Component {
        _client: NativeClient,
    }

    #[test]
    fn clones_and_request_options_resolve_one_component_without_holding_account_state() {
        let client = super::super::tests::client();
        let clone = client.clone();
        let other = client
            .with_request_options(NativeOptions::default())
            .unwrap();
        let first = client
            .shared_account_component(|| {
                assert_eq!(clone.account_status()?.in_flight, 0);
                Ok(Component { _client: clone })
            })
            .unwrap();
        let same = other
            .shared_account_component::<Component>(|| panic!("component already exists"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        let independent = super::super::tests::client();
        let separate = independent
            .shared_account_component(|| {
                Ok(Component {
                    _client: independent.clone(),
                })
            })
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &separate));
    }

    #[test]
    fn weak_component_cache_does_not_keep_a_runtime_or_its_account_alive() {
        let client = super::super::tests::client();
        let owner = Arc::downgrade(client.owned_account().unwrap());
        let component = client
            .shared_account_component(|| {
                Ok(Component {
                    _client: client.clone(),
                })
            })
            .unwrap();
        let previous = Arc::downgrade(&component);
        drop(component);
        assert!(previous.upgrade().is_none());
        let next = client
            .shared_account_component(|| {
                Ok(Component {
                    _client: client.clone(),
                })
            })
            .unwrap();
        drop(client);
        assert!(owner.upgrade().is_some());
        drop(next);
        assert!(owner.upgrade().is_none());
    }

    #[test]
    fn shutdown_fences_a_component_factory_without_waiting_for_the_cache_lock() {
        let client = super::super::tests::client();
        let other = client.clone();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let a = entered.clone();
        let b = release.clone();
        let worker = thread::spawn(move || {
            other.shared_account_component(|| {
                a.wait();
                b.wait();
                Ok(17u32)
            })
        });
        entered.wait();
        client.stop().unwrap();
        release.wait();
        assert!(worker.join().unwrap().is_err());
        assert!(client
            .shared_account_component::<u32>(|| panic!("stopped factory must not run"))
            .is_err());
    }

    #[test]
    fn concurrent_access_constructs_a_single_component() {
        let client = super::super::tests::client();
        let start = Arc::new(Barrier::new(3));
        let built = Arc::new(AtomicU64::new(0));
        let spawn = || {
            let client = client.clone();
            let start = start.clone();
            let built = built.clone();
            thread::spawn(move || {
                start.wait();
                client
                    .shared_account_component(|| {
                        built.fetch_add(1, Ordering::SeqCst);
                        Ok(7u32)
                    })
                    .unwrap()
            })
        };
        let first = spawn();
        let second = spawn();
        start.wait();
        let a = first.join().unwrap();
        let b = second.join().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(built.load(Ordering::SeqCst), 1);
    }
}
