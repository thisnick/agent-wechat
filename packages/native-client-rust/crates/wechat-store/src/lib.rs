//! Official-format profiles remain opaque here; only our sidecar schema is migrated.
mod account_removal;
mod admission;
mod authorization;
mod checkpoint;
pub mod copy;
mod group_mutations;
mod media;
mod metadata;
mod operations;
mod outbox;
mod outgoing_recall;
mod pagination;
mod recalls;
mod settings;
pub mod vault;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
pub use group_mutations::*;
pub use media::*;
pub use operations::*;
pub use outgoing_recall::*;
pub use pagination::*;
pub use recalls::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct Lease(Vec<File>);

/// Reject overlapping source/control roots before a caller initializes the store.
pub fn validate_source_placement(data_dir: &Path, source: &Path) -> Result<()> {
    let source = fs::canonicalize(source)?;
    let mut ancestor = data_dir.to_owned();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .context("data directory needs an existing ancestor")?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .context("data directory needs a parent")?
            .to_owned();
    }
    let mut resolved = fs::canonicalize(ancestor)?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    if resolved
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("use a data directory path without unresolved parent components");
    }
    if resolved.starts_with(&source) || source.starts_with(&resolved) {
        bail!("profile source and daemon data directory must be separate");
    }
    Ok(())
}
impl Drop for Lease {
    fn drop(&mut self) {
        for file in &self.0 {
            let _ = FileExt::unlock(file);
        }
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    Ok(wechat_platform::private_dir(path)?)
}

fn lock(path: &Path) -> Result<Lease> {
    let f = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(path)?;
    f.try_lock_exclusive()
        .context("storage is already owned by another operation")?;
    Ok(Lease(vec![f]))
}

/// Our process lock does not exclude an unmanaged official application.
pub fn profile_lease(profile: &Path) -> Result<Lease> {
    let profile = fs::canonicalize(profile)?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("agent-wechat-{}", wechat_platform::user_key()))
        });
    let locks = runtime.join("agent-wechat-profile-locks");
    private_dir(&locks)?;
    // A full home and its account subtree are two spellings of the same
    // operating databases. Lock those canonical roots as well as the binding.
    let mut roots = std::collections::BTreeSet::from([profile.clone()]);
    if profile.join("db_storage").is_dir() {
        roots.insert(fs::canonicalize(profile.join("db_storage"))?);
    }
    let accounts = profile.join("xwechat_files");
    if accounts.is_dir() {
        for (index, entry) in fs::read_dir(accounts)?.enumerate() {
            if index >= 4096 {
                bail!("profile account inventory exceeds limit");
            }
            let path = entry?.path().join("db_storage");
            if path.is_dir() {
                roots.insert(fs::canonicalize(path)?);
            }
        }
    }
    let mut held = Lease(vec![]);
    for root in roots {
        let hash = format!("{:x}", Sha256::digest(wechat_platform::path_bytes(&root)));
        let mut lease = lock(&locks.join(hash))?;
        held.0.append(&mut lease.0);
    }
    Ok(held)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub storage_mode: String,
    pub profile_path: PathBuf,
    pub schema_compatibility: String,
}

pub struct Store {
    pub root: PathBuf,
    pub server_id: String,
    db: Mutex<Connection>,
    admitted: Mutex<admission::AdmissionCache>,
    _lease: Lease,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_inner(root, true)
    }
    pub fn open_existing(root: &Path) -> Result<Self> {
        if !root.is_dir() || !root.join("registry.sqlite").is_file() {
            bail!("store is not initialized");
        }
        Self::open_inner(root, false)
    }
    fn open_inner(root: &Path, initialize: bool) -> Result<Self> {
        private_dir(root)?;
        let root = fs::canonicalize(root)?;
        let lease = lock(&root.join("owner.lock"))?;
        let db_path = root.join("registry.sqlite");
        if !db_path.exists() {
            wechat_platform::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db_path)?
                .sync_all()?;
        }
        let m = fs::symlink_metadata(&db_path)?;
        if !m.is_file() || !wechat_platform::private_path(&db_path)? {
            bail!("registry must be an owner-only regular file");
        }
        let (db, server_id) =
            admission::open_prepared(&db_path, None, |db| Self::prepare_registry(db, initialize))?;
        Ok(Self {
            root,
            server_id,
            db: Mutex::new(db),
            admitted: Mutex::new(Default::default()),
            _lease: lease,
        })
    }
    fn prepare_registry(db: &mut Connection, initialize: bool) -> Result<String> {
        db.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if !(0..=3).contains(&version) {
            bail!("unsupported registry schema version");
        }
        if version == 0 {
            if !initialize {
                bail!("registry is not initialized; existing data preserved");
            }
            let count: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if count != 0 {
                bail!("unrecognized registry schema; existing data preserved");
            }
            let tx = db.transaction()?;
            tx.execute_batch(include_str!("../migrations/registry/V001__registry.sql"))?;
            tx.execute(
                "INSERT INTO metadata VALUES('server_id',?1)",
                [uuid::Uuid::new_v4().to_string()],
            )?;
            tx.commit()?;
        }
        if version < 2 {
            operations::migrate(db)?;
        }
        if version < 3 {
            db.execute_batch(include_str!(
                "../migrations/registry/V003__removed_accounts.sql"
            ))?;
        }
        let server_id = db.query_row(
            "SELECT value FROM metadata WHERE key='server_id'",
            [],
            |r| r.get(0),
        )?;
        Ok(server_id)
    }
    pub(crate) fn require_registered(db: &Connection, id: &str) -> Result<()> {
        let active: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND removed_at IS NULL)",
            [id],
            |r| r.get(0),
        )?;
        if !active {
            bail!("account not found");
        }
        Ok(())
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        let mut q = db.prepare(
            "SELECT id,storage_mode,profile_path,schema_compatibility FROM accounts WHERE removed_at IS NULL ORDER BY id",
        )?;
        let rows = q.query_map([], |r| {
            Ok(Account {
                id: r.get(0)?,
                storage_mode: r.get(1)?,
                profile_path: PathBuf::from(r.get::<_, String>(2)?),
                schema_compatibility: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    fn account_dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            bail!("invalid local account ID");
        }
        Ok(self.root.join("accounts").join(id))
    }
    pub fn attach(&self, id: &str, profile: &Path, source_stopped: bool) -> Result<Account> {
        self.attach_with_lease(id, profile, source_stopped)
            .map(|(account, _)| account)
    }
    /// Retain ownership continuously when attaching to a running daemon.
    pub fn attach_with_lease(
        &self,
        id: &str,
        profile: &Path,
        source_stopped: bool,
    ) -> Result<(Account, Lease)> {
        if !source_stopped {
            bail!("confirm that the source is stopped or a consistent snapshot");
        }
        let profile = fs::canonicalize(profile)?;
        if !profile.is_dir() || profile.starts_with(&self.root) || self.root.starts_with(&profile) {
            bail!("direct profile must be a separate directory");
        }
        let lease = profile_lease(&profile)?;
        // Registration never opens SQLCipher, changes a source, or asserts write compatibility.
        Ok((self.register(id, "direct", &profile, None, None)?, lease))
    }
    /// Bound operating files under a normal user HOME. Sidecars may be elsewhere
    /// in HOME, but must never overlap either official data subtree. The mode is
    /// committed with registration so even interrupted setup exports only WeChat.
    pub fn attach_official_home(&self, id: &str, home: &Path) -> Result<(Account, Lease)> {
        let home = fs::canonicalize(home)?;
        if !home.join("xwechat_files").is_dir() || home.starts_with(&self.root) {
            bail!("official_profile_overlaps_store");
        }
        for name in ["xwechat_files", ".xwechat"] {
            let path = home.join(name);
            if self.root.starts_with(&path) || path.starts_with(&self.root) {
                bail!("official_profile_overlaps_store");
            }
        }
        let lease = profile_lease(&home)?;
        Ok((self.register(id, "official", &home, None, None)?, lease))
    }
    pub fn import(&self, id: &str, source: &Path, source_stopped: bool) -> Result<Account> {
        validate_source_placement(&self.root, source)?;
        let target = self.account_dir(id)?;
        if fs::symlink_metadata(&target).is_ok() {
            bail!("account directory already exists; refusing overwrite");
        }
        let parent = target.parent().unwrap();
        private_dir(parent)?;
        let stage = wechat_platform::private_tempdir_in(parent, ".account-")?;
        let manifest = copy::copy_profile(source, &stage.path().join("profile"), source_stopped)?;
        self.register(
            id,
            "managed",
            &target.join("profile"),
            Some((stage, manifest)),
            None,
        )
    }
    fn register(
        &self,
        id: &str,
        mode: &str,
        profile: &Path,
        staged: Option<(wechat_platform::PrivateTempDir, copy::Manifest)>,
        alias: Option<&str>,
    ) -> Result<Account> {
        let target = self.account_dir(id)?;
        let parent = target.parent().unwrap();
        private_dir(parent)?;
        if fs::symlink_metadata(&target).is_ok() {
            bail!("account directory already exists; refusing overwrite");
        }
        let mut db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("registry lock poisoned"))?;
        let tx = db.transaction()?;
        let alias_collision: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM account_settings WHERE alias=?1)",
            [id],
            |r| r.get(0),
        )?;
        if alias_collision {
            bail!("account_alias_conflict");
        }
        if let Some(alias) = alias {
            if alias.is_empty()
                || alias.len() > 64
                || !alias
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                bail!("invalid account alias");
            }
            let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 UNION ALL SELECT 1 FROM account_settings WHERE alias=?1)",[alias],|r|r.get(0))?;
            if conflict {
                bail!("account_alias_conflict");
            }
        }
        let path = profile.to_str().context("profile path is not UTF-8")?;
        tx.execute(
            "INSERT INTO accounts(id,storage_mode,profile_path,schema_compatibility) VALUES(?1,?2,?3,'unverified')",
            params![id, mode, path],
        )?;
        if let Some(alias) = alias {
            tx.execute(
                "INSERT INTO account_settings(account_id,alias)VALUES(?1,?2)",
                params![id, alias],
            )?;
        }
        let (stage, manifest) = match staged {
            Some((s, m)) => (s, Some(m)),
            None => (
                wechat_platform::private_tempdir_in(parent, ".account-")?,
                None,
            ),
        };
        let agent = stage.path().join("agent");
        private_dir(&agent)?;
        let control_path = agent.join("control.sqlite");
        wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&control_path)?;
        let control = Connection::open(&control_path)?;
        control.execute_batch("CREATE TABLE copy_receipts(id INTEGER PRIMARY KEY,manifest_json TEXT NOT NULL); PRAGMA user_version=1;")?;
        if let Some(m) = manifest {
            control.execute(
                "INSERT INTO copy_receipts(manifest_json) VALUES(?1)",
                [serde_json::to_string(&m)?],
            )?;
        }
        drop(control);
        wechat_platform::sync_file(&control_path)?;
        wechat_platform::sync_dir(&agent)?;
        copy::publish(stage.path(), &target)?;
        // A crash before this commit leaves an unregistered directory. Refuse reuse;
        // do not erase it or infer authenticated account state during recovery.
        tx.commit().context("profile published but registry commit failed; preserve the account directory for recovery")?;
        Ok(Account {
            id: id.into(),
            storage_mode: mode.into(),
            profile_path: profile.into(),
            schema_compatibility: "unverified".into(),
        })
    }
    pub fn export(
        &self,
        id: &str,
        destination: &Path,
        source_stopped: bool,
    ) -> Result<copy::Manifest> {
        let account = self
            .accounts()?
            .into_iter()
            .find(|a| a.id == id)
            .context("account is not registered")?;
        if account.storage_mode == "official" {
            copy::copy_official_home(&account.profile_path, destination, source_stopped)
        } else {
            copy::copy_profile(&account.profile_path, destination, source_stopped)
        }
    }
    pub fn profile_leases(&self) -> Result<std::collections::BTreeMap<String, Vec<Lease>>> {
        self.accounts()?
            .into_iter()
            .map(|a| Ok((a.id, vec![profile_lease(&a.profile_path)?])))
            .collect()
    }
}
