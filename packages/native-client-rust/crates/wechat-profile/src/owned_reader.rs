//! Reusable connections for an exclusively owned profile. The caller's account
//! gate spans ordinary reads, or just snapshot pinning for request-owned readers.
use crate::{
    schema::inspect,
    snapshot::{walk, Snapshot},
    DatabaseInventory, DatabaseState, KeyFile, ProfileError, ProfileLocation, ProfileReader,
    ReadOptions, Result, SchemaReport,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    sync::{Arc, Mutex},
};

// Large profiles remain readable without retaining a handle for every shard.
const MAX_CACHED_DATABASES: usize = 32;

struct CachedDatabase {
    metadata: fs::Metadata,
    key: String,
    connection: Arc<Mutex<Connection>>,
    schema: SchemaReport,
    schema_version: i64,
}

fn same_database_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        wechat_platform::same_file(a, b)
    }
    #[cfg(not(unix))]
    {
        a.created().ok() == b.created().ok()
            && a.modified().ok() == b.modified().ok()
            && a.len() == b.len()
    }
}

#[derive(Default)]
pub struct OwnedReaderCache {
    root: Option<std::path::PathBuf>,
    databases: BTreeMap<String, CachedDatabase>,
    nested: BTreeMap<String, Arc<Mutex<Connection>>>,
}

impl OwnedReaderCache {
    /// Establish every database snapshot while the caller excludes profile writes.
    /// Each checked-out cache belongs to one request; nested queries use a second
    /// connection pinned at the same barrier rather than opening a later view.
    pub fn read_snapshot(
        &mut self,
        location: ProfileLocation,
        keys: KeyFile,
        options: ReadOptions,
    ) -> Result<ProfileReader> {
        let mut reader = self.read(location, keys, options)?;
        let mut pinned = crate::snapshot::PinnedTransactions {
            connections: BTreeMap::new(),
        };
        let deadline = std::time::Instant::now() + reader.snapshot.options.query_timeout;
        for entry in reader
            .snapshot
            .inventory
            .iter()
            .filter(|entry| entry.state == DatabaseState::Readable)
        {
            let Some(primary) = reader.snapshot.owned.get(&entry.relative_path) else {
                return Err(ProfileError::ParallelSnapshotUnavailable);
            };
            if !self.nested.contains_key(&entry.relative_path) {
                self.nested.insert(
                    entry.relative_path.clone(),
                    Arc::new(Mutex::new(
                        reader
                            .snapshot
                            .open_connection(&entry.relative_path, true)?,
                    )),
                );
            }
            let pair = vec![primary.clone(), self.nested[&entry.relative_path].clone()];
            pinned
                .connections
                .insert(entry.relative_path.clone(), pair.clone());
            for connection in pair {
                let connection = connection.lock().map_err(|_| ProfileError::Database)?;
                let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
                if !mode.eq_ignore_ascii_case("wal") {
                    return Err(ProfileError::ParallelSnapshotUnavailable);
                }
                connection
                    .progress_handler(1000, Some(move || std::time::Instant::now() > deadline));
                connection.set_limit(
                    rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                    reader.snapshot.options.max_blob_bytes as i32,
                );
                connection.execute_batch("BEGIN DEFERRED")?;
                connection.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
                    r.get::<_, i64>(0)
                })?;
            }
        }
        self.nested
            .retain(|relative, _| reader.snapshot.paths.contains_key(relative));
        reader.snapshot.pinned = Some(pinned);
        Ok(reader)
    }

    /// The caller must own the profile and hold its account gate until the
    /// returned reader is dropped. This path may update SQLite's WAL index.
    /// Offline inspection must continue to use `ProfileReader::open` instead.
    pub fn read(
        &mut self,
        location: ProfileLocation,
        keys: KeyFile,
        options: ReadOptions,
    ) -> Result<ProfileReader> {
        if !options.stopped_or_consistent_snapshot {
            return Err(ProfileError::SnapshotRequired);
        }
        if keys.account_binding != location.account {
            return Err(ProfileError::AccountMismatch);
        }
        if options.max_blob_bytes == 0
            || options.max_blob_bytes > 64 * 1024 * 1024
            || options.max_databases == 0
            || options.max_databases > 4096
            || options.query_timeout.is_zero()
        {
            return Err(ProfileError::InvalidQuery);
        }
        if self.root.as_ref() != Some(&location.root) {
            self.databases.clear();
            self.nested.clear();
            self.root = Some(location.root.clone());
        }
        let mut files = Vec::new();
        walk(&location.storage, &mut files, 0, options.max_databases)?;
        if files.is_empty() {
            return Err(ProfileError::InvalidProfile);
        }
        let mut snapshot = Snapshot {
            location,
            keys,
            options,
            generation: String::new(),
            inventory: Vec::new(),
            paths: BTreeMap::new(),
            _temp: None,
            owned: BTreeMap::new(),
            pinned: None,
        };
        let mut generation = Sha256::new();
        generation.update(snapshot.location.root.as_os_str().as_encoded_bytes());
        let mut total = 0u64;
        let mut schemas = Vec::new();
        for path in files {
            let relative = path
                .strip_prefix(&snapshot.location.storage)
                .ok()
                .and_then(|p| p.to_str())
                .ok_or(ProfileError::UnsupportedFile)?
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(&path)?;
            let mut stamp = Sha256::new();
            for suffix in ["", "-wal", "-shm"] {
                let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
                let stat = match fs::symlink_metadata(&file) {
                    Ok(stat) => stat,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() =>
                    {
                        continue
                    }
                    Err(error) => return Err(error.into()),
                };
                if !stat.is_file() || wechat_platform::is_link(&stat) {
                    return Err(ProfileError::UnsupportedFile);
                }
                total = total
                    .checked_add(stat.len())
                    .ok_or(ProfileError::ResourceLimit)?;
                if stat.len() > snapshot.options.max_database_bytes
                    || total > snapshot.options.max_total_bytes
                {
                    return Err(ProfileError::ResourceLimit);
                }
                // SHM changes as readers attach; it is not settled content.
                if suffix != "-shm" {
                    stamp.update(suffix.as_bytes());
                    stamp.update(stat.len().to_le_bytes());
                    stamp.update(
                        format!("{:?}:{:?}", stat.modified()?, stat.created().ok()).as_bytes(),
                    );
                }
            }
            let mut prefix = [0u8; 16];
            fs::File::open(&path)?.read_exact(&mut prefix)?;
            let plaintext = prefix == *b"SQLite format 3\0";
            let key = snapshot
                .keys
                .databases
                .get(&relative)
                .map(|key| {
                    let mut hash = Sha256::new();
                    hash.update(key.salt);
                    hash.update(key.raw_page_key.expose());
                    hex::encode(hash.finalize())
                })
                .unwrap_or_default();
            let entry = DatabaseInventory {
                relative_path: relative.clone(),
                bytes: metadata.len(),
                plaintext,
                has_wal: path
                    .with_file_name(format!(
                        "{}-wal",
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .ok_or(ProfileError::UnsupportedFile)?
                    ))
                    .try_exists()?,
                generation: String::new(),
                state: DatabaseState::Readable,
            };
            snapshot.paths.insert(relative.clone(), path);
            snapshot.inventory.push(entry);
            let mut uncached = None;
            let reuse = self.databases.get(&relative).is_some_and(|cached| {
                same_database_file(&cached.metadata, &metadata)
                    && cached.key == key
                    && (!plaintext || snapshot.options.allow_plaintext)
            });
            if !reuse {
                self.databases.remove(&relative);
                self.nested.remove(&relative);
                match snapshot.open_connection(&relative, true) {
                    Ok(connection) => {
                        let schema = inspect(&connection, &relative)
                            .map_err(|_| ProfileError::UnsupportedSchema(relative.clone()))?;
                        let schema_version =
                            connection.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
                        let database = CachedDatabase {
                            metadata,
                            key,
                            connection: Arc::new(Mutex::new(connection)),
                            schema,
                            schema_version,
                        };
                        if self.databases.len() < MAX_CACHED_DATABASES {
                            self.databases.insert(relative.clone(), database);
                        } else {
                            uncached = Some(database);
                        }
                    }
                    Err(ProfileError::KeyUnavailable(_)) => {
                        snapshot.inventory.last_mut().unwrap().state = DatabaseState::KeyUnavailable
                    }
                    Err(ProfileError::KeyMismatch(_)) => {
                        snapshot.inventory.last_mut().unwrap().state = DatabaseState::KeyMismatch
                    }
                    Err(error) => return Err(error),
                }
            }
            let retained = uncached.is_none();
            if let Some(cached) = self.databases.get_mut(&relative).or(uncached.as_mut()) {
                let connection = cached
                    .connection
                    .lock()
                    .map_err(|_| ProfileError::Database)?;
                // Idle time must not consume the next query's budget.
                let deadline = std::time::Instant::now() + snapshot.options.query_timeout;
                connection
                    .progress_handler(1000, Some(move || std::time::Instant::now() > deadline));
                connection.set_limit(
                    rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                    snapshot.options.max_blob_bytes as i32,
                );
                let schema_version =
                    connection.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
                if schema_version != cached.schema_version {
                    cached.schema = inspect(&connection, &relative)
                        .map_err(|_| ProfileError::UnsupportedSchema(relative.clone()))?;
                    cached.schema_version = schema_version;
                }
                // data_version is local to a connection, so it cannot identify
                // a profile generation across independent reader-pool slots.
                // Main/WAL file stamps above identify the same committed bytes.
                schemas.push(cached.schema.clone());
                if retained {
                    snapshot
                        .owned
                        .insert(relative.clone(), cached.connection.clone());
                }
            }
            let stamp = hex::encode(stamp.finalize());
            generation.update(relative.as_bytes());
            generation.update(stamp.as_bytes());
            snapshot.inventory.last_mut().unwrap().generation = stamp;
        }
        self.databases
            .retain(|relative, _| snapshot.paths.contains_key(relative));
        snapshot.generation = hex::encode(generation.finalize());
        Ok(ProfileReader { snapshot, schemas })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, ProfileLocation, Connection) {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("account/db_storage");
        fs::create_dir_all(&directory).unwrap();
        let writer = Connection::open(directory.join("sample.db")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1)").unwrap();
        let location = ProfileLocation::new(temp.path().join("account")).unwrap();
        (temp, location, writer)
    }
    fn read(cache: &mut OwnedReaderCache, location: &ProfileLocation) -> ProfileReader {
        cache
            .read(
                location.clone(),
                KeyFile::empty("account"),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    allow_plaintext: true,
                    ..Default::default()
                },
            )
            .unwrap()
    }
    fn value(reader: &ProfileReader) -> i64 {
        reader
            .snapshot
            .open("sample.db")
            .unwrap()
            .query_row("SELECT value FROM sample", [], |r| r.get(0))
            .unwrap()
    }

    fn pinned(cache: &mut OwnedReaderCache, location: &ProfileLocation) -> ProfileReader {
        cache
            .read_snapshot(
                location.clone(),
                KeyFile::empty("account"),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    allow_plaintext: true,
                    ..Default::default()
                },
            )
            .unwrap()
    }
    #[test]
    fn request_snapshots_pin_all_databases_and_nested_queries_at_one_boundary() {
        let (_temp, location, writer) = fixture();
        let other = Connection::open(location.storage.join("other.db")).unwrap();
        other.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(10)").unwrap();
        let mut cache = OwnedReaderCache::default();
        let first = pinned(&mut cache, &location);
        writer.execute("UPDATE sample SET value=2", []).unwrap();
        other.execute("UPDATE sample SET value=20", []).unwrap();
        let held = first.snapshot.open("sample.db").unwrap();
        assert_eq!(
            held.query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(value(&first), 1); // Uses the independently pinned nested connection.
        assert_eq!(
            first
                .snapshot
                .open("other.db")
                .unwrap()
                .query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10
        );
        drop(held);
        drop(first);
        let second = pinned(&mut cache, &location);
        assert_eq!(value(&second), 2);
        assert_eq!(
            second
                .snapshot
                .open("other.db")
                .unwrap()
                .query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            20
        );
    }
    #[test]
    fn independent_pool_connections_agree_on_source_generation() {
        let (_temp, location, writer) = fixture();
        let mut a = OwnedReaderCache::default();
        let before = pinned(&mut a, &location);
        drop(before);
        writer.execute("UPDATE sample SET value=2", []).unwrap();
        let a = pinned(&mut a, &location);
        let mut b = OwnedReaderCache::default();
        let b = pinned(&mut b, &location);
        assert_eq!(a.generation(), b.generation());
    }
    #[test]
    fn rollback_journal_fallback_does_not_change_mode_or_leave_transactions_open() {
        let (_temp, location, writer) = fixture();
        writer.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
        let mut cache = OwnedReaderCache::default();
        let error = cache.read_snapshot(
            location.clone(),
            KeyFile::empty("account"),
            ReadOptions {
                stopped_or_consistent_snapshot: true,
                allow_plaintext: true,
                ..Default::default()
            },
        );
        assert!(matches!(
            error,
            Err(ProfileError::ParallelSnapshotUnavailable)
        ));
        writer.execute("UPDATE sample SET value=3", []).unwrap();
        assert_eq!(value(&read(&mut cache, &location)), 3);
        assert_eq!(
            writer
                .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
    }

    #[test]
    fn unchanged_reads_reuse_handles_without_a_temporary_copy() {
        let (_temp, location, _writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let first = read(&mut cache, &location);
        let second = read(&mut cache, &location);
        assert!(first.snapshot._temp.is_none());
        assert!(Arc::ptr_eq(
            &first.snapshot.owned["sample.db"],
            &second.snapshot.owned["sample.db"]
        ));
        assert_eq!(first.generation(), second.generation());
        assert_eq!(value(&first), 1);
        assert!(first
            .snapshot
            .open("sample.db")
            .unwrap()
            .execute("UPDATE sample SET value=2", [])
            .is_err());
    }

    #[test]
    fn short_read_transactions_are_stable_and_release_before_the_next_read() {
        let (_temp, location, writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let first = read(&mut cache, &location);
        let transaction = first.snapshot.open("sample.db").unwrap();
        let query = || {
            transaction
                .query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(query(), 1);
        writer.execute("UPDATE sample SET value=2", []).unwrap();
        assert_eq!(query(), 1);
        drop(transaction);
        let second = read(&mut cache, &location);
        assert_eq!(value(&second), 2);
        assert_ne!(first.generation(), second.generation());
    }

    #[test]
    fn changed_schemas_and_new_or_removed_databases_refresh_the_inventory() {
        let (_temp, location, writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let first = read(&mut cache, &location);
        writer
            .execute_batch("ALTER TABLE sample ADD COLUMN text_value TEXT")
            .unwrap();
        let extra = location.storage.join("extra.db");
        Connection::open(&extra)
            .unwrap()
            .execute_batch("CREATE TABLE other(value INTEGER)")
            .unwrap();
        let second = read(&mut cache, &location);
        assert_ne!(first.generation(), second.generation());
        assert_eq!(second.inventory().len(), 2);
        assert!(second
            .schemas()
            .iter()
            .any(|schema| schema.tables.iter().any(|table| table
                .columns
                .iter()
                .any(|column| column.name == "text_value"))));
        drop(second);
        cache.databases.remove("extra.db");
        fs::remove_file(extra).unwrap();
        assert_eq!(read(&mut cache, &location).inventory().len(), 1);
    }

    #[test]
    fn account_binding_and_size_limits_still_apply() {
        let (_temp, location, _writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let options = ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        };
        assert!(matches!(
            cache.read(location.clone(), KeyFile::empty("another"), options.clone()),
            Err(ProfileError::AccountMismatch)
        ));
        assert!(matches!(
            cache.read(
                location,
                KeyFile::empty("account"),
                ReadOptions {
                    max_database_bytes: 1,
                    ..options
                }
            ),
            Err(ProfileError::ResourceLimit)
        ));
    }
    #[test]
    fn stricter_read_options_apply_to_reused_connections() {
        let (_temp, location, _writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let original = read(&mut cache, &location);
        let smaller = cache
            .read(
                location.clone(),
                KeyFile::empty("account"),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    allow_plaintext: true,
                    max_blob_bytes: 1024,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(Arc::ptr_eq(
            &original.snapshot.owned["sample.db"],
            &smaller.snapshot.owned["sample.db"]
        ));
        assert_eq!(
            smaller
                .snapshot
                .open("sample.db")
                .unwrap()
                .limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH),
            1024
        );
        let strict = cache
            .read(
                location,
                KeyFile::empty("account"),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(matches!(
            strict.snapshot.open("sample.db"),
            Err(ProfileError::KeyUnavailable(_))
        ));
    }
    #[test]
    fn nested_reads_do_not_wait_on_their_own_checked_out_connection() {
        let (_temp, location, _writer) = fixture();
        let reader = read(&mut OwnedReaderCache::default(), &location);
        let first = reader.snapshot.open("sample.db").unwrap();
        let second = reader.snapshot.open("sample.db").unwrap();
        for connection in [&*first, &*second] {
            assert_eq!(
                connection
                    .query_row("SELECT value FROM sample", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
    #[test]
    fn large_profiles_do_not_retain_a_handle_for_every_database() {
        let (_temp, location, _writer) = fixture();
        for index in 0..MAX_CACHED_DATABASES + 2 {
            Connection::open(location.storage.join(format!("extra-{index:02}.db")))
                .unwrap()
                .execute_batch("CREATE TABLE extra(value INTEGER); INSERT INTO extra VALUES(7)")
                .unwrap();
        }
        let mut cache = OwnedReaderCache::default();
        let reader = read(&mut cache, &location);
        assert_eq!(cache.databases.len(), MAX_CACHED_DATABASES);
        assert_eq!(reader.snapshot.owned.len(), MAX_CACHED_DATABASES);
        assert_eq!(reader.inventory().len(), MAX_CACHED_DATABASES + 3);
        // sample.db sorts after the retained extra shards and remains readable.
        assert!(!reader.snapshot.owned.contains_key("sample.db"));
        assert_eq!(value(&reader), 1);
        assert!(reader
            .snapshot
            .open("sample.db")
            .unwrap()
            .execute("UPDATE sample SET value=2", [])
            .is_err());
    }
    #[test]
    fn changed_encryption_keys_cannot_reuse_an_already_unlocked_handle() {
        let temp = tempfile::tempdir().unwrap();
        let (location, keys, _) =
            crate::initialize_profile(temp.path().join("account"), &[7; 32]).unwrap();
        let options = ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        };
        let mut cache = OwnedReaderCache::default();
        let original = cache
            .read(location.clone(), keys.clone(), options.clone())
            .unwrap();
        assert!(original.contacts(&crate::ListQuery::default()).is_ok());
        let relative = "contact/contact.db";
        let mut changed = keys.clone();
        changed.databases.insert(
            relative.into(),
            crate::DatabaseKey::new(relative.into(), keys.databases[relative].salt, [9; 32])
                .unwrap(),
        );
        let rejected = cache
            .read(location.clone(), changed, options.clone())
            .unwrap();
        assert!(matches!(
            rejected.contacts(&crate::ListQuery::default()),
            Err(ProfileError::KeyMismatch(_))
        ));
        let restored = cache.read(location, keys, options).unwrap();
        assert!(restored.contacts(&crate::ListQuery::default()).is_ok());
        assert!(!Arc::ptr_eq(
            &original.snapshot.owned[relative],
            &restored.snapshot.owned[relative]
        ));
    }
    #[cfg(unix)]
    #[test]
    fn replaced_files_reopen_instead_of_serving_an_old_inode() {
        let (_temp, location, writer) = fixture();
        let mut cache = OwnedReaderCache::default();
        let original = read(&mut cache, &location);
        let old_handle = Arc::downgrade(&original.snapshot.owned["sample.db"]);
        drop(original);
        // Replace a quiescent database family, never a new main file with an
        // unrelated live WAL. The latter is invalid for copied readers too.
        writer
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(writer);
        let replacement = location.storage.join("replacement");
        let db = Connection::open(&replacement).unwrap();
        db.execute_batch("CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(9)")
            .unwrap();
        drop(db);
        fs::rename(replacement, location.storage.join("sample.db")).unwrap();
        let current = read(&mut cache, &location);
        assert!(old_handle.upgrade().is_none());
        assert_eq!(value(&current), 9);
    }
}
