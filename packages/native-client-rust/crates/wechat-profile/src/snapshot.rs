use crate::{
    keys::{derive_key, verify_first_page, SecretBytes},
    DatabaseInventory, DatabaseKey, DatabaseState, KeyFile, ProfileError, Result,
};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub struct ProfileLocation {
    pub(crate) root: PathBuf,
    pub(crate) storage: PathBuf,
    pub(crate) account: String,
}
impl ProfileLocation {
    /// Select one account root containing db_storage, or db_storage itself. A
    /// whole WeChat home with multiple accounts is never resolved implicitly.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let p = fs::canonicalize(path).map_err(|_| ProfileError::InvalidProfile)?;
        let root = if p.file_name().is_some_and(|n| n == "db_storage") {
            p.parent()
                .ok_or(ProfileError::InvalidProfile)?
                .to_path_buf()
        } else {
            p
        };
        let storage = root.join("db_storage");
        if !root.is_dir()
            || !storage.is_dir()
            || fs::symlink_metadata(&storage)?.file_type().is_symlink()
        {
            return Err(ProfileError::InvalidProfile);
        }
        let account = root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(ProfileError::InvalidProfile)?
            .to_string();
        Ok(Self {
            root,
            storage,
            account,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn account_directory(&self) -> &str {
        &self.account
    }
}

#[derive(Clone, Debug)]
pub struct ReadOptions {
    /// This is a caller assertion, not an operating-system guarantee. Advisory
    /// locks cannot stop an unmanaged official client from writing the profile.
    pub stopped_or_consistent_snapshot: bool,
    /// Intended for explicit plaintext exports and synthetic fixtures only.
    pub allow_plaintext: bool,
    pub max_database_bytes: u64,
    pub max_total_bytes: u64,
    pub max_databases: usize,
    pub max_blob_bytes: usize,
    pub query_timeout: Duration,
}
impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            stopped_or_consistent_snapshot: false,
            allow_plaintext: false,
            max_database_bytes: 2 * 1024 * 1024 * 1024,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_databases: 4096,
            max_blob_bytes: 16 * 1024 * 1024,
            query_timeout: Duration::from_secs(10),
        }
    }
}

pub(crate) struct Snapshot {
    pub location: ProfileLocation,
    pub keys: KeyFile,
    pub options: ReadOptions,
    pub generation: String,
    pub inventory: Vec<DatabaseInventory>,
    pub paths: BTreeMap<String, PathBuf>,
    pub(crate) _temp: Option<wechat_platform::PrivateTempDir>,
    pub(crate) owned: BTreeMap<String, Arc<Mutex<Connection>>>,
    pub(crate) pinned: Option<PinnedTransactions>,
}

pub(crate) struct PinnedTransactions {
    pub(crate) connections: BTreeMap<String, Vec<Arc<Mutex<Connection>>>>,
}
impl Drop for PinnedTransactions {
    fn drop(&mut self) {
        for connections in self.connections.values() {
            for connection in connections {
                if let Ok(connection) = connection.lock() {
                    connection.progress_handler(0, None::<fn() -> bool>);
                    let _ = connection.execute_batch("ROLLBACK");
                }
            }
        }
    }
}

pub(crate) enum ReadConnection<'a> {
    Copied(Connection),
    Owned(MutexGuard<'a, Connection>),
    Pinned(MutexGuard<'a, Connection>),
}
impl std::ops::Deref for ReadConnection<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Copied(connection) => connection,
            Self::Owned(connection) | Self::Pinned(connection) => connection,
        }
    }
}
impl Drop for ReadConnection<'_> {
    fn drop(&mut self) {
        if let Self::Owned(connection) = self {
            connection.progress_handler(0, None::<fn() -> bool>);
            let _ = connection.execute_batch("ROLLBACK");
        }
    }
}

fn relative(root: &Path, p: &Path) -> Result<String> {
    p.strip_prefix(root)
        .ok()
        .and_then(|v| v.to_str())
        .map(|v| v.replace('\\', "/"))
        .ok_or(ProfileError::UnsupportedFile)
}
pub(crate) fn walk(root: &Path, out: &mut Vec<PathBuf>, depth: usize, max: usize) -> Result<()> {
    if depth > 16 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut entries = fs::read_dir(root)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let m = fs::symlink_metadata(e.path())?;
        if wechat_platform::is_link(&m) || (!m.is_file() && !m.is_dir()) {
            return Err(ProfileError::UnsupportedFile);
        }
        if m.is_dir() {
            walk(&e.path(), out, depth + 1, max)?;
        } else if e.path().extension().is_some_and(|s| s == "db") {
            out.push(e.path());
            if out.len() > max {
                return Err(ProfileError::ResourceLimit);
            }
        }
    }
    Ok(())
}
fn hash_copy(
    source: &Path,
    destination: Option<&Path>,
    remaining: &mut u64,
    max: u64,
) -> Result<(u64, String)> {
    let m = fs::symlink_metadata(source)?;
    if !m.is_file() || wechat_platform::is_link(&m) {
        return Err(ProfileError::UnsupportedFile);
    }
    if m.len() > max || m.len() > *remaining {
        return Err(ProfileError::ResourceLimit);
    }
    let mut options = wechat_platform::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut from = options.open(source)?;
    if !from.metadata()?.is_file() {
        return Err(ProfileError::UnsupportedFile);
    }
    let mut to = if let Some(destination) = destination {
        let mut options = wechat_platform::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        Some(options.open(destination)?)
    } else {
        None
    };
    let mut h = Sha256::new();
    let mut bytes = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > max || bytes > *remaining {
            return Err(ProfileError::ResourceLimit);
        }
        h.update(&buf[..n]);
        if let Some(f) = &mut to {
            f.write_all(&buf[..n])?;
        }
    }
    *remaining -= bytes;
    Ok((bytes, hex::encode(h.finalize())))
}
fn candidates(storage: &Path, options: &ReadOptions) -> Result<Vec<PathBuf>> {
    let mut dbs = Vec::new();
    walk(storage, &mut dbs, 0, options.max_databases)?;
    if dbs.is_empty() {
        return Err(ProfileError::InvalidProfile);
    }
    let mut files = Vec::new();
    for p in dbs {
        files.push(p.clone());
        for suffix in ["-wal", "-shm"] {
            let x = PathBuf::from(format!("{}{suffix}", p.display()));
            if x.try_exists()? {
                files.push(x);
            }
        }
    }
    Ok(files)
}

impl Snapshot {
    #[track_caller]
    pub fn create(location: ProfileLocation, keys: KeyFile, options: ReadOptions) -> Result<Self> {
        let mut timing = wechat_platform::timing::Timing::start("profile.snapshot.create");
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
        let temp = wechat_platform::private_tempdir("agent-wechat-read-")?;
        // macOS and custom TMPDIRs may reach this private directory through a
        // symlink. Resolve its root before opening files with SQLITE_OPEN_NOFOLLOW.
        let temp_root = fs::canonicalize(temp.path())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
        }
        let mut manifest = BTreeMap::new();
        let mut total = options.max_total_bytes;
        let files = candidates(&location.storage, &options)?;
        let copy = wechat_platform::timing::Timing::start("profile.snapshot.copy");
        for source in &files {
            let rel = relative(&location.storage, source)?;
            let dest = temp_root.join(&rel);
            fs::create_dir_all(dest.parent().ok_or(ProfileError::UnsupportedFile)?)?;
            let (bytes, hash) =
                hash_copy(source, Some(&dest), &mut total, options.max_database_bytes)?;
            manifest.insert(rel, (bytes, hash));
        }
        drop(copy);
        timing.counts(manifest.values().map(|(bytes, _)| bytes).sum(), files.len());
        // Re-inventory catches newly created/deleted WAL files as well as main
        // changes. A stable double scan still requires the caller's quiescence.
        if files != candidates(&location.storage, &options)? {
            return Err(ProfileError::SourceChanged);
        }
        let mut total = options.max_total_bytes;
        let recheck = wechat_platform::timing::Timing::start("profile.snapshot.recheck");
        for source in &files {
            let rel = relative(&location.storage, source)?;
            if manifest.get(&rel)
                != Some(&hash_copy(
                    source,
                    None,
                    &mut total,
                    options.max_database_bytes,
                )?)
            {
                return Err(ProfileError::SourceChanged);
            }
        }
        drop(recheck);
        let _open = wechat_platform::timing::Timing::start("profile.snapshot.open");
        let mut h = Sha256::new();
        h.update(location.root.as_os_str().as_encoded_bytes());
        for (rel, (bytes, digest)) in &manifest {
            h.update(rel.as_bytes());
            h.update(bytes.to_le_bytes());
            h.update(digest.as_bytes());
        }
        let generation = hex::encode(h.finalize());
        let mut inventory = Vec::new();
        let mut paths = BTreeMap::new();
        for (rel, (bytes, _)) in &manifest {
            if !rel.ends_with(".db") {
                continue;
            }
            let p = temp_root.join(rel);
            let mut prefix = [0u8; 16];
            let mut f = fs::File::open(&p)?;
            let n = f.read(&mut prefix)?;
            let plain = n == 16 && prefix == *b"SQLite format 3\0";
            let mut dbh = Sha256::new();
            dbh.update(manifest.get(rel).unwrap().1.as_bytes());
            if let Some((_, digest)) = manifest.get(&format!("{rel}-wal")) {
                dbh.update(digest.as_bytes());
            }
            inventory.push(DatabaseInventory {
                relative_path: rel.clone(),
                bytes: *bytes,
                plaintext: plain,
                has_wal: manifest.contains_key(&format!("{rel}-wal")),
                generation: hex::encode(dbh.finalize()),
                state: DatabaseState::Readable,
            });
            paths.insert(rel.clone(), p);
        }
        let mut result = Self {
            location,
            keys,
            options,
            generation,
            inventory,
            paths,
            _temp: Some(temp),
            owned: BTreeMap::new(),
            pinned: None,
        };
        for i in 0..result.inventory.len() {
            let rel = result.inventory[i].relative_path.clone();
            let state = match result.open(&rel) {
                Ok(_) => DatabaseState::Readable,
                Err(ProfileError::KeyUnavailable(_)) => DatabaseState::KeyUnavailable,
                Err(ProfileError::KeyMismatch(_)) => DatabaseState::KeyMismatch,
                Err(e) => return Err(e),
            };
            result.inventory[i].state = state;
        }
        Ok(result)
    }
    pub fn open(&self, rel: &str) -> Result<ReadConnection<'_>> {
        if let Some(pinned) = &self.pinned {
            let connections = pinned
                .connections
                .get(rel)
                .ok_or(ProfileError::InvalidQuery)?;
            for connection in connections {
                match connection.try_lock() {
                    Ok(connection) => return Ok(ReadConnection::Pinned(connection)),
                    Err(TryLockError::WouldBlock) => continue,
                    Err(TryLockError::Poisoned(_)) => return Err(ProfileError::Database),
                }
            }
            // Never fall back to a fresh connection with a different snapshot.
            return Err(ProfileError::ResourceLimit);
        }
        if let Some(connection) = self.owned.get(rel) {
            let connection = match connection.try_lock() {
                Ok(connection) => connection,
                Err(TryLockError::WouldBlock) => {
                    // Some compound readers nest a second query of the same
                    // database. It must not wait on its own checked-out handle.
                    return Ok(ReadConnection::Copied(self.open_connection(rel, true)?));
                }
                Err(TryLockError::Poisoned(_)) => return Err(ProfileError::Database),
            };
            let deadline = std::time::Instant::now() + self.options.query_timeout;
            connection.progress_handler(1000, Some(move || std::time::Instant::now() > deadline));
            connection.execute_batch("BEGIN DEFERRED")?;
            return Ok(ReadConnection::Owned(connection));
        }
        Ok(ReadConnection::Copied(
            self.open_connection(rel, self._temp.is_none())?,
        ))
    }
    pub(crate) fn open_connection(&self, rel: &str, read_only: bool) -> Result<Connection> {
        let path = self.paths.get(rel).ok_or(ProfileError::InvalidQuery)?;
        let entry = self
            .inventory
            .iter()
            .find(|d| d.relative_path == rel)
            .ok_or(ProfileError::InvalidQuery)?;
        let c = Connection::open_with_flags(
            path,
            (if read_only {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            } else {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            }) | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        c.busy_timeout(Duration::from_millis(1000))?;
        if entry.plaintext {
            if !self.options.allow_plaintext {
                return Err(ProfileError::KeyUnavailable(rel.into()));
            }
        } else {
            let k = self
                .keys
                .databases
                .get(rel)
                .ok_or_else(|| ProfileError::KeyUnavailable(rel.into()))?;
            let mut page = [0u8; 4096];
            fs::File::open(path)?
                .read_exact(&mut page)
                .map_err(|_| ProfileError::KeyMismatch(rel.into()))?;
            if !verify_first_page(&page, k.raw_page_key.expose(), &k.salt) {
                return Err(ProfileError::KeyMismatch(rel.into()));
            }
            configure_cipher(&c, &k.raw_page_key, rel)?;
        }
        c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|_| ProfileError::DatabaseOpen(rel.into()))?;
        c.execute_batch(
            "PRAGMA query_only=ON; PRAGMA temp_store=MEMORY; PRAGMA trusted_schema=OFF;",
        )?;
        c.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            self.options.max_blob_bytes as i32,
        );
        c.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
            1024 * 1024,
        );
        c.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0);
        let deadline = std::time::Instant::now() + self.options.query_timeout;
        c.progress_handler(1000, Some(move || std::time::Instant::now() > deadline));
        Ok(c)
    }
}

pub(crate) fn configure_cipher(c: &Connection, key: &SecretBytes, rel: &str) -> Result<()> {
    let available = c
        .query_row("PRAGMA cipher_version", [], |r| r.get::<_, String>(0))
        .ok()
        .is_some_and(|s| !s.is_empty());
    if !available {
        return Err(ProfileError::SqlcipherUnavailable);
    }
    let sql = Zeroizing::new(format!(
        "PRAGMA key = \"x'{}'\"; PRAGMA cipher_compatibility=4; PRAGMA cipher_page_size=4096;",
        hex::encode(key.expose())
    ));
    c.execute_batch(&sql)
        .map_err(|_| ProfileError::DatabaseOpen(rel.into()))
}

/// Export only validated per-file keys from an explicitly supplied existing
/// account password. No password discovery, fallback, account switch, or source
/// mutation takes place; the password itself is not retained in the result.
pub fn keys_from_password(
    location: ProfileLocation,
    password: &[u8],
    mut options: ReadOptions,
) -> Result<KeyFile> {
    if password.len() != 32 {
        return Err(ProfileError::InvalidKeysFile);
    }
    options.allow_plaintext = false;
    // Capture first, then derive on the private copy. Missing keys are expected.
    let snap = Snapshot::create(
        location.clone(),
        KeyFile::empty(location.account.clone()),
        options,
    )?;
    let mut keys = KeyFile::empty(location.account.clone());
    for (rel, path) in &snap.paths {
        let mut page = [0u8; 4096];
        fs::File::open(path)?
            .read_exact(&mut page)
            .map_err(|_| ProfileError::KeyMismatch(rel.clone()))?;
        if &page[..16] == b"SQLite format 3\0" {
            return Err(ProfileError::KeyMismatch(rel.clone()));
        }
        let mut salt = [0u8; 16];
        salt.copy_from_slice(&page[..16]);
        let key = derive_key(password, &salt);
        if !verify_first_page(&page, key.expose(), &salt) {
            return Err(ProfileError::KeyMismatch(rel.clone()));
        }
        let c = Connection::open(path).map_err(|_| ProfileError::DatabaseOpen(rel.clone()))?;
        configure_cipher(&c, &key, rel)?;
        c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|_| ProfileError::DatabaseOpen(rel.clone()))?;
        keys.databases.insert(
            rel.clone(),
            DatabaseKey {
                relative_path: rel.clone(),
                salt,
                raw_page_key: key,
            },
        );
    }
    Ok(keys)
}

/// Read only the matching authorized cached key records from a coherent copied
/// agent database. This never calls the old database initializer/migrator or any
/// key acquisition process. The agent token is an unlock passphrase, not an
/// exported profile key or network credential.
pub fn keys_from_agent_cache(
    location: ProfileLocation,
    agent_database: &Path,
    agent_token: &[u8],
    session_id: Option<&str>,
    options: ReadOptions,
) -> Result<KeyFile> {
    if !options.stopped_or_consistent_snapshot {
        return Err(ProfileError::SnapshotRequired);
    }
    if agent_token.is_empty() || agent_token.len() > 16384 {
        return Err(ProfileError::InvalidKeysFile);
    }
    let pass = std::str::from_utf8(agent_token)
        .map_err(|_| ProfileError::InvalidKeysFile)?
        .trim();
    if pass.is_empty() || pass.contains('\0') {
        return Err(ProfileError::InvalidKeysFile);
    }
    let temp = wechat_platform::private_tempdir("agent-wechat-key-export-")?;
    let copied = temp.path().join("agent.db");
    let mut hashes = BTreeMap::new();
    let mut total = options.max_total_bytes;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
    }
    for suffix in ["", "-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{suffix}", agent_database.display()));
        if source.try_exists()? {
            let dest = PathBuf::from(format!("{}{suffix}", copied.display()));
            hashes.insert(
                source.clone(),
                hash_copy(&source, Some(&dest), &mut total, options.max_database_bytes)?,
            );
        }
    }
    if !hashes.contains_key(agent_database) {
        return Err(ProfileError::KeysMissing);
    }
    let mut total = options.max_total_bytes;
    for (source, hash) in &hashes {
        if &hash_copy(source, None, &mut total, options.max_database_bytes)? != hash {
            return Err(ProfileError::SourceChanged);
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{suffix}", agent_database.display()));
        if source.try_exists()? != hashes.contains_key(&source) {
            return Err(ProfileError::SourceChanged);
        }
    }
    let c = Connection::open(&copied)?;
    if c.query_row("PRAGMA cipher_version", [], |r| r.get::<_, String>(0))
        .ok()
        .is_none()
    {
        return Err(ProfileError::SqlcipherUnavailable);
    }
    let sql=Zeroizing::new(format!("PRAGMA key='{}'; PRAGMA query_only=ON; PRAGMA temp_store=MEMORY; PRAGMA trusted_schema=OFF;",pass.replace('\'',"''")));
    c.execute_batch(&sql)
        .map_err(|_| ProfileError::InvalidKeysFile)?;
    c.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024);
    let deadline = std::time::Instant::now() + options.query_timeout;
    c.progress_handler(1000, Some(move || std::time::Instant::now() > deadline));
    let mut s=c.prepare("SELECT DISTINCT session_id FROM wechat_keys WHERE account_dir=?1 ORDER BY session_id LIMIT 3").map_err(|_|ProfileError::InvalidKeysFile)?;
    let sessions = s
        .query_map([&location.account], |r| r.get::<_, String>(0))
        .map_err(|_| ProfileError::InvalidKeysFile)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| ProfileError::InvalidKeysFile)?;
    let selected = match session_id {
        Some(id) if sessions.iter().any(|s| s == id) => id,
        None if sessions.len() == 1 => &sessions[0],
        _ => return Err(ProfileError::InvalidKeysFile),
    };
    let mut s=c.prepare("SELECT db_name,hex_key FROM wechat_keys WHERE account_dir=?1 AND session_id=?2 LIMIT 4097").map_err(|_|ProfileError::InvalidKeysFile)?;
    let mut records = Zeroizing::new(
        s.query_map(rusqlite::params![location.account, selected], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| ProfileError::InvalidKeysFile)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| ProfileError::InvalidKeysFile)?,
    );
    if records.len() > 4096 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut keys = KeyFile::empty(location.account.clone());
    let snap = Snapshot::create(location.clone(), keys.clone(), options.clone())?;
    for (name, key) in records.iter() {
        if name.starts_with('_') {
            continue;
        }
        let matching = snap
            .paths
            .iter()
            .filter(|(p, _)| p.rsplit('/').next() == Some(name.as_str()))
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            continue;
        }
        let (rel, path) = matching[0];
        let mut page = [0u8; 4096];
        fs::File::open(path)?.read_exact(&mut page)?;
        let salt: [u8; 16] = page[..16]
            .try_into()
            .map_err(|_| ProfileError::InvalidKeysFile)?;
        let key = SecretBytes::new(hex::decode(key).map_err(|_| ProfileError::InvalidKeysFile)?);
        if key.expose().len() != 32 || !verify_first_page(&page, key.expose(), &salt) {
            return Err(ProfileError::KeyMismatch(rel.clone()));
        }
        let test = Connection::open(path)?;
        configure_cipher(&test, &key, rel)?;
        test.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(|_| ProfileError::KeyMismatch(rel.clone()))?;
        keys.databases.insert(
            rel.clone(),
            DatabaseKey {
                relative_path: rel.clone(),
                salt,
                raw_page_key: key,
            },
        );
    }
    let aes = records
        .iter()
        .filter(|(n, _)| n == "_image_aes")
        .collect::<Vec<_>>();
    let xor = records
        .iter()
        .filter(|(n, _)| n == "_image_xor")
        .collect::<Vec<_>>();
    if aes.len() > 1 || xor.len() > 1 {
        return Err(ProfileError::InvalidKeysFile);
    }
    if let (Some(a), Some(x)) = (aes.first(), xor.first()) {
        let x = u8::from_str_radix(&x.1, 16).map_err(|_| ProfileError::InvalidKeysFile)?;
        keys.image_dat = Some(crate::ImageDatKey::from_legacy_cache(&a.1, x)?);
    }
    for (_, v) in records.iter_mut() {
        zeroize::Zeroize::zeroize(v);
    }
    if keys.databases.is_empty() {
        return Err(ProfileError::KeyUnavailable(
            "authorized agent cache".into(),
        ));
    }
    Ok(keys)
}
