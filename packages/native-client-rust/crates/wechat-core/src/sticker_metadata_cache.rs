//! Temporary remote metadata only. Membership and media availability are never cached here.
use anyhow::{anyhow, Result};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const TTL: Duration = Duration::from_secs(60);
const ENTRIES: usize = 64;
const ENTRY_BYTES: usize = 128 * 1024;
struct Entry {
    created: Instant,
    value: Mutex<Option<(Instant, Arc<Vec<u8>>)>>,
}
impl Default for Entry {
    fn default() -> Self {
        Self {
            created: Instant::now(),
            value: Mutex::new(None),
        }
    }
}
#[derive(Default)]
pub(crate) struct StickerMetadataCache {
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}
impl StickerMetadataCache {
    pub(crate) fn invalidate_account(&self, account: &str) {
        let prefix = format!("{}:{}:", account.len(), account);
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|key, _| !key.starts_with(&prefix));
        }
    }

    pub(crate) fn get(
        &self,
        key: String,
        fetch: impl FnOnce() -> Result<Vec<u8>>,
    ) -> Result<Arc<Vec<u8>>> {
        let entry = {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| anyhow!("metadata cache unavailable"))?;
            entries.retain(|_, e| {
                e.value
                    .try_lock()
                    .map(|v| {
                        v.as_ref().map_or(e.created.elapsed() < TTL, |(at, bytes)| {
                            at.elapsed() < TTL && bytes.len() <= ENTRY_BYTES
                        })
                    })
                    .unwrap_or(true)
            });
            if let Some(entry) = entries.get(&key) {
                entry.clone()
            } else if entries.len() < ENTRIES {
                let entry = Arc::new(Entry::default());
                entries.insert(key, entry.clone());
                entry
            } else {
                drop(entries);
                return fetch().map(Arc::new);
            }
        };
        let mut value = entry
            .value
            .lock()
            .map_err(|_| anyhow!("metadata request interrupted"))?;
        if let Some((at, bytes)) = &*value {
            if at.elapsed() < TTL {
                return Ok(bytes.clone());
            }
        }
        let bytes = Arc::new(fetch()?);
        if bytes.len() <= ENTRY_BYTES {
            *value = Some((Instant::now(), bytes.clone()));
        }
        Ok(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };
    #[test]
    fn simultaneous_clients_share_one_fetch_and_scopes_do_not_share() {
        let cache = Arc::new(StickerMetadataCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    cache
                        .get("account/session/pack".into(), || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            Ok(vec![1, 2])
                        })
                        .unwrap()
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(&**thread.join().unwrap(), &[1, 2]);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            &**cache
                .get("other/session/pack".into(), || Ok(vec![3]))
                .unwrap(),
            &[3]
        );
        assert_eq!(
            &**cache
                .get("account/new-session/pack".into(), || Ok(vec![4]))
                .unwrap(),
            &[4]
        );
    }
    #[test]
    fn logout_discards_inflight_entries_and_large_results_are_not_retained() {
        let cache = Arc::new(StickerMetadataCache::default());
        let (began, started) = std::sync::mpsc::channel();
        let (finish, released) = std::sync::mpsc::channel();
        let worker = cache.clone();
        let task = std::thread::spawn(move || {
            worker
                .get("3:one:session:pack".into(), || {
                    began.send(()).unwrap();
                    released.recv().unwrap();
                    Ok(vec![1])
                })
                .unwrap()
        });
        started.recv().unwrap();
        cache.invalidate_account("one");
        finish.send(()).unwrap();
        task.join().unwrap();
        assert_eq!(
            &**cache
                .get("3:one:session:pack".into(), || Ok(vec![2]))
                .unwrap(),
            &[2]
        );
        cache
            .get("large".into(), || Ok(vec![0; ENTRY_BYTES + 1]))
            .unwrap();
        assert_eq!(&**cache.get("large".into(), || Ok(vec![3])).unwrap(), &[3]);
    }
    #[test]
    fn failures_and_expired_values_are_refetched_and_memory_is_bounded() {
        let cache = StickerMetadataCache::default();
        assert!(cache
            .get("fail".into(), || anyhow::bail!("failed"))
            .is_err());
        assert_eq!(&**cache.get("fail".into(), || Ok(vec![1])).unwrap(), &[1]);
        let entry = Arc::new(Entry::default());
        *entry.value.lock().unwrap() = Some((Instant::now() - TTL, Arc::new(vec![0])));
        cache
            .entries
            .lock()
            .unwrap()
            .insert("expired".into(), entry);
        assert_eq!(
            &**cache.get("expired".into(), || Ok(vec![2])).unwrap(),
            &[2]
        );
        for index in 0..100 {
            cache.get(index.to_string(), || Ok(vec![0])).unwrap();
        }
        assert!(cache.entries.lock().unwrap().len() <= ENTRIES);
        cache.entries.lock().unwrap().insert(
            "failed-old".into(),
            Arc::new(Entry {
                created: Instant::now() - TTL,
                value: Mutex::new(None),
            }),
        );
        cache.get("fresh".into(), || Ok(vec![1])).unwrap();
        assert!(!cache.entries.lock().unwrap().contains_key("failed-old"));
    }
}
