//! SQLite may recover or unlink companions even when schema admission fails.
//! Validate an owned copy before allowing it to open an existing store family.
//!
//! A database that has no journal or WAL to replay and is already on the
//! current schema version is opened in place: neither SQLite nor a migration has
//! anything to change, so the copies would protect nothing. It is still checked
//! for corruption before use.
//!
//! The scratch copies are removed when admission returns. A process that is
//! stopped part-way leaves them behind. Until an admission starts opening the
//! original they are only copies of an unchanged family, so the next admission
//! that copies the database removes them first. Otherwise starts that keep
//! failing for lack of space would never free the space they need. A directory
//! whose admission went on to open the original is marked: its copy may be the
//! only pre-open state, so only a successful admission removes it. One kept on
//! purpose after a failed open is marked as retained and never removed.
use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

const SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];
const SCRATCH: &str = ".sqlite-admission-";
/// Marks a scratch directory that was kept on purpose as the only pre-open copy.
const RETAINED: &str = "retained";
/// Marks a scratch directory whose admission went on to open the original.
const OPENING: &str = "opening";
#[path = "admission_wal.rs"]
mod wal;
pub(crate) type AdmissionCache = BTreeMap<PathBuf, Family>;
struct Member {
    file: File,
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}
pub(crate) struct Family(BTreeMap<&'static str, Member>);
fn member_path(path: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = path
        .file_name()
        .context("database path needs a filename")?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}
impl Family {
    fn read(path: &Path) -> Result<Self> {
        let mut members = BTreeMap::new();
        for suffix in SUFFIXES {
            let name = member_path(path, suffix)?;
            match fs::symlink_metadata(&name) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => {
                    continue
                }
                Err(e) => return Err(e.into()),
                Ok(metadata) => ensure!(
                    metadata.is_file() && !wechat_platform::is_link(&metadata),
                    "database family must contain regular files"
                ),
            }
            let file = wechat_platform::OpenOptions::new().read(true).open(&name)?;
            ensure!(
                wechat_platform::private_file(&file)?
                    && wechat_platform::same_path_file(&name, &file)?,
                "database family must be private and stable"
            );
            let metadata = file.metadata()?;
            members.insert(
                suffix,
                Member {
                    file,
                    length: metadata.len(),
                    modified: metadata.modified()?,
                    #[cfg(unix)]
                    changed: {
                        use std::os::unix::fs::MetadataExt;
                        (metadata.ctime(), metadata.ctime_nsec())
                    },
                },
            );
        }
        Ok(Self(members))
    }
    fn unchanged(&self, path: &Path) -> Result<bool> {
        let current = Self::read(path)?;
        if self.0.keys().ne(current.0.keys()) {
            return Ok(false);
        }
        for (suffix, before) in &self.0 {
            let after = &current.0[suffix];
            if !wechat_platform::same_path_file(&member_path(path, suffix)?, &before.file)?
                || before.length != after.length
                || before.modified != after.modified
            {
                return Ok(false);
            }
            #[cfg(unix)]
            if before.changed != after.changed {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn copy_to(&self, source: &Path, destination: &Path) -> Result<()> {
        for (suffix, member) in &self.0 {
            let mut input = member.file.try_clone()?;
            input.seek(SeekFrom::Start(0))?;
            let mut output = wechat_platform::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(member_path(destination, suffix)?)?;
            let copied = std::io::copy(
                &mut input.take(member.length.saturating_add(1)),
                &mut output,
            )?;
            ensure!(
                copied == member.length,
                "database family changed during validation"
            );
            output.sync_all()?;
        }
        ensure!(
            self.unchanged(source)?,
            "database family changed during validation"
        );
        wechat_platform::sync_dir(destination.parent().context("database needs a parent")?)?;
        Ok(())
    }
    fn digest(&self, path: &Path) -> Result<String> {
        let mut hash = Sha256::new();
        for (suffix, member) in &self.0 {
            hash.update((suffix.len() as u64).to_be_bytes());
            hash.update(suffix.as_bytes());
            hash.update(member.length.to_be_bytes());
            let mut input = member.file.try_clone()?;
            input.seek(SeekFrom::Start(0))?;
            let mut buffer = [0; 64 * 1024];
            let mut remaining = member.length;
            while remaining > 0 {
                let wanted = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..wanted])?;
                hash.update(&buffer[..wanted]);
                remaining -= wanted as u64;
            }
        }
        ensure!(self.unchanged(path)?, "database recovery family changed");
        Ok(format!("{:x}", hash.finalize()))
    }
}
fn retain_recovery(original: &Path, parent: &Path) -> Result<PathBuf> {
    let path = original.join("database.sqlite");
    let family = Family::read(&path)?;
    let digest = family.digest(&path)?;
    let destination = parent.join(format!(".sqlite-recovery-{digest}"));
    if destination.try_exists()? {
        ensure!(
            fs::symlink_metadata(&destination)?.is_dir()
                && wechat_platform::private_path(&destination)?,
            "invalid database recovery directory; existing files preserved"
        );
        ensure!(
            Family::read(&destination.join("database.sqlite"))?
                .digest(&destination.join("database.sqlite"))?
                == digest,
            "database recovery backup mismatch; existing files preserved"
        );
    } else {
        wechat_platform::publish_directory(original, &destination)?;
    }
    wechat_platform::sync_dir(parent)?;
    Ok(destination)
}
impl Family {
    /// True when opening this family cannot change it: no journal, WAL or WAL
    /// index holds anything to replay, and the header already carries the
    /// current schema version, so no migration will run.
    fn settled(&self, current: i64) -> Result<bool> {
        if self
            .0
            .iter()
            .any(|(suffix, m)| !suffix.is_empty() && m.length > 0)
        {
            return Ok(false);
        }
        let Some(main) = self.0.get("").filter(|m| m.length >= 100) else {
            return Ok(false);
        };
        let mut header = [0; 100];
        let mut file = main.file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut header)?;
        Ok(&header[..16] == b"SQLite format 3\0"
            && i64::from(i32::from_be_bytes(header[60..64].try_into().unwrap())) == current)
    }
}
/// Remove scratch directories that an interrupted admission left beside this
/// database. One kept on purpose carries a marker and is left alone. Before a
/// new copy, one whose admission had started opening the original stays too.
fn sweep_scratch(parent: &Path, before_copy: bool) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(SCRATCH));
        if ours
            && fs::symlink_metadata(&path)
                .is_ok_and(|m| m.is_dir() && !wechat_platform::is_link(&m))
            && !path.join(RETAINED).exists()
            && !(before_copy && path.join(OPENING).exists())
        {
            let _ = fs::remove_dir_all(&path);
        }
    }
}
fn connect(path: &Path) -> Result<Connection> {
    // Never create a missing original after admission, and never follow a final
    // link that appeared between validation and SQLite's own open.
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?)
}
fn check(db: &Connection) -> Result<()> {
    let mut query = db.prepare("PRAGMA quick_check")?;
    let mut rows = query.query([])?;
    let first = rows
        .next()?
        .context("database integrity check returned no result")?;
    ensure!(
        first.get::<_, String>(0)? == "ok",
        "database integrity check failed"
    );
    ensure!(rows.next()?.is_none(), "database integrity check failed");
    Ok(())
}

pub(crate) fn open_prepared<T>(
    path: &Path,
    cache: Option<&AdmissionCache>,
    current: i64,
    prepare: impl Fn(&mut Connection) -> Result<T>,
) -> Result<(Connection, T)> {
    // Permission failures should be rejected before SQLite can recover or clean
    // companions. This check opens a handle without writing any source bytes.
    let permission_probe = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .context("database must be readable and writable; existing files preserved")?;
    ensure!(
        wechat_platform::private_file(&permission_probe)?
            && wechat_platform::same_path_file(path, &permission_probe)?,
        "database must be private and stable"
    );
    drop(permission_probe);
    if cache
        .and_then(|c| c.get(path))
        .map(|known| known.unchanged(path))
        .transpose()?
        .unwrap_or(false)
    {
        let mut db = connect(path)?;
        let value = prepare(&mut db)?;
        return Ok((db, value));
    }
    let family = Family::read(path)?;
    let parent = path.parent().context("database needs a parent")?;
    if family.settled(current)? {
        let mut db = connect(path)?;
        check(&db).map_err(|error| {
            anyhow::anyhow!(
                "cannot validate {}: {error}; existing database and companions preserved",
                path.display()
            )
        })?;
        let value = prepare(&mut db)?;
        sweep_scratch(parent, false);
        return Ok((db, value));
    }
    // Free what interrupted admissions left before making new copies.
    sweep_scratch(parent, true);
    let needed = family
        .0
        .values()
        .map(|member| member.length)
        .sum::<u64>()
        .saturating_mul(2);
    let (scratch, original, probe, retain_unverified) = (|| -> Result<_> {
        let scratch = wechat_platform::private_tempdir_in(parent, SCRATCH)?;
        let original = scratch.path().join("original");
        let validation = scratch.path().join("validation");
        wechat_platform::private_dir(&original)?;
        wechat_platform::private_dir(&validation)?;
        let saved = original.join("database.sqlite");
        family.copy_to(path, &saved)?;
        let copied = Family::read(&saved)?;
        let retain_unverified = wal::validate(&copied).map_err(|error| {
            anyhow::anyhow!(
                "cannot open {}: {error}. Stop the daemon and back up this file with its -wal and -shm files before changing them; see https://thisnick.github.io/agent-wechat/reference/troubleshooting/#starting-the-daemon",
                path.display()
            )
        })?;
        let probe = validation.join("database.sqlite");
        copied.copy_to(&saved, &probe)?;
        wechat_platform::sync_dir(scratch.path())?;
        wechat_platform::sync_dir(parent)?;
        Ok((scratch, original, probe, retain_unverified))
    })()
    .map_err(space_hint(path, needed))?;
    let validate = || -> Result<()> {
        let mut db = connect(&probe)?;
        check(&db)?;
        prepare(&mut db)?;
        check(&db)?;
        db.close().map_err(|(_, error)| error)?;
        Ok(())
    };
    validate().map_err(|error| {
        anyhow::anyhow!(
            "cannot validate {}: {error}; existing database and companions preserved",
            path.display()
        )
    })?;
    ensure!(
        family.unchanged(path)?,
        "database family changed during validation; existing files preserved"
    );
    // Rollback recovery can legitimately change the main file or consume its
    // journal. Keep the pre-open family before permitting that operation; the
    // content-addressed name deduplicates unchanged retries and cold restarts.
    let retained = if retain_unverified {
        Some(retain_recovery(&original, parent)?)
    } else {
        None
    };
    ensure!(
        family.unchanged(path)?,
        "database family changed before admission; existing files preserved"
    );
    // From here the original can change. If this process stops, the copy above
    // is kept until an admission of this database succeeds.
    File::create(scratch.path().join(OPENING))?;
    wechat_platform::sync_dir(scratch.path())?;
    let opened = (|| -> Result<(Connection, T)> {
        let mut db = connect(path)?;
        let value = prepare(&mut db)?;
        Ok((db, value))
    })();
    match opened {
        Ok(result) => {
            drop(scratch);
            sweep_scratch(parent, false);
            Ok(result)
        }
        Err(error) => {
            // A successful private validation followed by a source I/O failure
            // is uncertain. Keep the pre-open family for explicit recovery.
            let recovery = match retained {
                Some(path) => path,
                None => match retain_recovery(&original, parent) {
                    Ok(path) => path,
                    Err(_) => {
                        // Best effort: without this marker, the opening marker
                        // still keeps the directory until an admission succeeds.
                        let _ = File::create(scratch.path().join(RETAINED));
                        let recovery = scratch.keep();
                        return Err(error.context(format!("database admission failed; unpublished pre-open recovery family retained at {}",recovery.join("original").display())));
                    }
                },
            };
            Err(error.context(format!(
                "database admission failed; pre-open database family retained at {}",
                recovery.display()
            )))
        }
    }
}

/// Name the space the copies need when the disk or quota ran out. The original
/// error stays the source, so callers still classify it as an I/O failure.
fn space_hint(path: &Path, needed: u64) -> impl Fn(anyhow::Error) -> anyhow::Error + '_ {
    move |error| {
        let full = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .any(|cause| {
                matches!(
                    cause.kind(),
                    std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
                )
            });
        if !full {
            return error;
        }
        error.context(format!(
            "cannot copy {} for validation: the copies need about {} MiB free beside it; existing database and companions preserved",
            path.display(),
            needed.div_ceil(1 << 20).max(1)
        ))
    }
}

pub(crate) fn remember(cache: &mut AdmissionCache, path: &Path) -> Result<()> {
    // Retained handles establish identity across platforms; bound their number
    // when a daemon serves a large account registry.
    if cache.len() >= 64 && !cache.contains_key(path) {
        cache.pop_first();
    }
    cache.insert(path.to_owned(), Family::read(path)?);
    Ok(())
}

/// Read-only inspection of one file in a SQLite database family (`""`, `"-wal"`, `"-shm"`, `"-journal"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseMemberInspection {
    /// File suffix relative to the main database path (`""`, `"-wal"`, `"-shm"`, or `"-journal"`).
    pub suffix: String,
    /// Size of the file in bytes.
    pub bytes: u64,
    /// True when the path is a symlink or reparse point (which daemon startup refuses).
    pub is_link: bool,
    /// True when the path is a regular file.
    pub is_regular_file: bool,
    /// True when the file passes the platform owner-only privacy check.
    pub private: bool,
    /// Leading header bytes formatted as lowercase hex (at most 96 bytes, no table rows).
    pub header_hex: String,
    /// Read error if the file metadata or leading bytes could not be read.
    pub read_error: Option<String>,
}

/// Result of running the startup WAL/SHM/journal admission validator read-only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum WalInspection {
    /// Accepted by startup admission without keeping a recovery copy.
    Valid {
        /// True when startup admission will retain a `.sqlite-recovery-…` backup before opening.
        retain_recovery: bool,
    },
    /// Refused by startup admission; includes the exact admission error without paths.
    Refused {
        /// Reason reported by startup WAL/SHM/journal admission validation.
        reason: String,
    },
    /// Validation could not run (for example because a member was unreadable or not a regular file).
    Skipped {
        /// Why WAL validation could not run.
        reason: String,
    },
}

/// Complete read-only inspection of a SQLite database family.
///
/// Never opens any file for writing, never invokes SQLite, never takes locks,
/// and never creates or removes files or scratch directories.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInspection {
    /// Whether the main database file exists.
    pub exists: bool,
    /// Metadata and leading header bytes for each present file in the family.
    pub members: Vec<DatabaseMemberInspection>,
    /// True when the main file starts with the 16-byte SQLite 3 header magic.
    pub valid_sqlite_header: bool,
    /// SQLite page size parsed from the header, when available.
    pub page_size: Option<u32>,
    /// SQLite file format write version byte (1 = rollback journal, 2 = WAL).
    pub write_version: Option<u8>,
    /// SQLite `user_version` parsed from bytes 60..64 of the main header.
    pub schema_version: Option<i64>,
    /// Maximum schema version supported by this binary for this database role.
    pub supported_schema_version: i64,
    /// True when startup admission can open this database in place without validation copies.
    pub settled: bool,
    /// Approximate free space in bytes needed beside the database when copy validation runs.
    pub copy_validation_bytes: u64,
    /// Startup WAL/SHM/journal admission result from `admission_wal::validate`.
    pub wal: WalInspection,
    /// Count of leftover `.sqlite-admission-*` scratch directories beside this database.
    pub leftover_admission_dirs: usize,
    /// Count of retained `.sqlite-recovery-*` backup directories beside this database.
    pub recovery_backup_dirs: usize,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Inspect a SQLite database family strictly read-only, reusing startup WAL admission checks.
///
/// Never opens any file for writing, never takes `owner.lock`, never invokes SQLite
/// (so no `-shm` is created or modified), and never creates or removes scratch directories.
pub fn inspect_readonly(path: &Path, supported_schema_version: i64) -> DatabaseInspection {
    let mut members = Vec::new();
    let mut family_map = BTreeMap::new();
    let mut main_header = None;
    let mut exists = false;
    let mut wal_skip_reason: Option<String> = None;

    for suffix in SUFFIXES {
        let Ok(name) = member_path(path, suffix) else {
            wal_skip_reason = Some("database path needs a filename".into());
            break;
        };
        let meta = match fs::symlink_metadata(&name) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                if suffix.is_empty() {
                    exists = true;
                }
                members.push(DatabaseMemberInspection {
                    suffix: suffix.into(),
                    bytes: 0,
                    is_link: false,
                    is_regular_file: false,
                    private: false,
                    header_hex: String::new(),
                    read_error: Some(e.to_string()),
                });
                wal_skip_reason
                    .get_or_insert_with(|| format!("cannot inspect {suffix} metadata: {e}"));
                continue;
            }
            Ok(meta) => meta,
        };
        if suffix.is_empty() {
            exists = true;
        }
        let is_link = wechat_platform::is_link(&meta);
        let is_regular_file = meta.is_file() && !is_link;
        let private = !is_link && wechat_platform::private_path(&name).unwrap_or(false);
        let length = meta.len();
        if !is_regular_file {
            members.push(DatabaseMemberInspection {
                suffix: suffix.into(),
                bytes: length,
                is_link,
                is_regular_file: false,
                private,
                header_hex: String::new(),
                read_error: Some(if is_link {
                    "path is a symlink or reparse point".into()
                } else {
                    "path is not a regular file".into()
                }),
            });
            wal_skip_reason
                .get_or_insert_with(|| "database family must contain regular files".into());
            continue;
        }
        match wechat_platform::OpenOptions::new().read(true).open(&name) {
            Ok(mut file) => {
                let header_cap = if suffix.is_empty() {
                    100
                } else if suffix == "-shm" {
                    96
                } else {
                    32
                };
                let to_read = (length as usize).min(header_cap);
                let mut buf = vec![0u8; to_read];
                let read_error = if to_read > 0 {
                    match file.read_exact(&mut buf) {
                        Ok(()) => {
                            if suffix.is_empty() && buf.len() == 100 {
                                main_header = Some(buf.clone());
                            }
                            None
                        }
                        Err(e) => Some(e.to_string()),
                    }
                } else {
                    None
                };
                let _ = file.seek(SeekFrom::Start(0));
                let file_meta = file.metadata().ok();
                let actual_len = file_meta.as_ref().map_or(length, fs::Metadata::len);
                let modified = file_meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                #[cfg(unix)]
                let changed = file_meta.as_ref().map_or((0, 0), |m| {
                    use std::os::unix::fs::MetadataExt;
                    (m.ctime(), m.ctime_nsec())
                });
                let preview_len = buf.len().min(if suffix == "-shm" { 48 } else { 32 });
                members.push(DatabaseMemberInspection {
                    suffix: suffix.into(),
                    bytes: actual_len,
                    is_link,
                    is_regular_file: true,
                    private,
                    header_hex: hex_encode(&buf[..preview_len]),
                    read_error: read_error.clone(),
                });
                if let Some(err) = read_error {
                    wal_skip_reason
                        .get_or_insert_with(|| format!("cannot read database file: {err}"));
                } else {
                    family_map.insert(
                        suffix,
                        Member {
                            file,
                            length: actual_len,
                            modified,
                            #[cfg(unix)]
                            changed,
                        },
                    );
                }
            }
            Err(e) => {
                members.push(DatabaseMemberInspection {
                    suffix: suffix.into(),
                    bytes: length,
                    is_link,
                    is_regular_file: true,
                    private,
                    header_hex: String::new(),
                    read_error: Some(e.to_string()),
                });
                wal_skip_reason
                    .get_or_insert_with(|| format!("cannot open database file read-only: {e}"));
            }
        }
    }

    let (valid_sqlite_header, page_size, write_version, schema_version) =
        if let Some(header) = &main_header {
            let valid = &header[..16] == b"SQLite format 3\0";
            if valid {
                let raw_page = u16::from_be_bytes([header[16], header[17]]);
                let page = if raw_page == 1 {
                    65536
                } else {
                    u32::from(raw_page)
                };
                let write_ver = header[18];
                let user_ver = i64::from(i32::from_be_bytes([
                    header[60], header[61], header[62], header[63],
                ]));
                (true, Some(page), Some(write_ver), Some(user_ver))
            } else {
                (false, None, None, None)
            }
        } else {
            (false, None, None, None)
        };

    let family = Family(family_map);
    let settled = exists
        && wal_skip_reason.is_none()
        && family.settled(supported_schema_version).unwrap_or(false);
    let total_bytes: u64 = members.iter().map(|m| m.bytes).sum();
    let copy_validation_bytes = if exists && !settled {
        total_bytes.saturating_mul(2)
    } else {
        0
    };
    let wal = if !exists {
        WalInspection::Skipped {
            reason: "database file does not exist".into(),
        }
    } else if let Some(reason) = wal_skip_reason {
        WalInspection::Skipped { reason }
    } else {
        match wal::validate(&family) {
            Ok(retain_recovery) => WalInspection::Valid { retain_recovery },
            Err(error) => WalInspection::Refused {
                reason: error.to_string(),
            },
        }
    };

    let mut leftover_admission_dirs = 0;
    let mut recovery_backup_dirs = 0;
    if let Some(parent) = path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !fs::symlink_metadata(entry.path())
                    .is_ok_and(|m| m.is_dir() && !wechat_platform::is_link(&m))
                {
                    continue;
                }
                if name.starts_with(SCRATCH) {
                    leftover_admission_dirs += 1;
                } else if name.starts_with(".sqlite-recovery-") {
                    recovery_backup_dirs += 1;
                }
            }
        }
    }

    DatabaseInspection {
        exists,
        members,
        valid_sqlite_header,
        page_size,
        write_version,
        schema_version,
        supported_schema_version,
        settled,
        copy_validation_bytes,
        wal,
        leftover_admission_dirs,
        recovery_backup_dirs,
    }
}

fn normalize_uri_path(raw: &str, windows: bool) -> String {
    let normalized = if windows {
        if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            format!("//{}", unc.replace('\\', "/"))
        } else if let Some(verbatim) = raw.strip_prefix(r"\\?\") {
            verbatim.replace('\\', "/")
        } else {
            raw.replace('\\', "/")
        }
    } else {
        raw.to_owned()
    };
    let mut encoded = String::with_capacity(normalized.len() + 24);
    encoded.push_str("file:");
    if windows && !normalized.starts_with('/') {
        encoded.push('/');
    }
    for ch in normalized.chars() {
        match ch {
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            _ => encoded.push(ch),
        }
    }
    encoded.push_str("?immutable=1");
    encoded
}

/// Encode a filesystem path for a SQLite `file:` URI without mutating or opening files.
fn sqlite_immutable_uri(path: &Path) -> Result<String> {
    let raw = path.to_str().context("database path is not valid UTF-8")?;
    Ok(normalize_uri_path(raw, cfg!(windows)))
}

/// Read registered accounts from `registry.sqlite` strictly read-only (`immutable=1`).
///
/// With `immutable=1`, SQLite reads only the main database file and never creates
/// `-shm`, acquires file locks, or replays WAL/journal files.
pub fn read_accounts_immutable(registry_path: &Path) -> Result<Vec<crate::Account>> {
    let meta = fs::symlink_metadata(registry_path)?;
    ensure!(
        meta.is_file() && !wechat_platform::is_link(&meta),
        "registry must be a regular file"
    );
    let uri = sqlite_immutable_uri(registry_path)?;
    let db = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let has_accounts: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='accounts')",
        [],
        |r| r.get(0),
    )?;
    if !has_accounts {
        return Ok(Vec::new());
    }
    let has_removed_at: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('accounts') WHERE name='removed_at')",
        [],
        |r| r.get(0),
    )?;
    let sql = if has_removed_at {
        "SELECT id, storage_mode, profile_path, schema_compatibility FROM accounts WHERE removed_at IS NULL ORDER BY id"
    } else {
        "SELECT id, storage_mode, profile_path, schema_compatibility FROM accounts ORDER BY id"
    };
    let mut stmt = db.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(crate::Account {
            id: r.get(0)?,
            storage_mode: r.get(1)?,
            profile_path: PathBuf::from(r.get::<_, String>(2)?),
            schema_compatibility: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn database(directory: &Path, version: i64) -> PathBuf {
        let path = directory.join("store.sqlite");
        private(&path, b"");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE t(value TEXT);INSERT INTO t VALUES('kept');PRAGMA user_version={version};"
        ))
        .unwrap();
        drop(db);
        path
    }
    fn private(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        wechat_platform::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    fn scratch(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with(SCRATCH))
            .collect();
        names.sort();
        names
    }
    /// Opens the database and reports how many times `prepare` ran: once when
    /// opened in place, twice when a copy was validated first.
    fn open(path: &Path, current: i64) -> Result<usize> {
        let runs = Cell::new(0);
        let (db, ()) = open_prepared(path, None, current, |db| {
            runs.set(runs.get() + 1);
            db.pragma_update(None, "user_version", current)?;
            Ok(())
        })?;
        let value: String = db.query_row("SELECT value FROM t", [], |r| r.get(0))?;
        assert_eq!(value, "kept");
        Ok(runs.get())
    }

    #[test]
    fn current_database_without_companions_opens_in_place_and_others_are_copied_first() {
        let temp = tempfile::tempdir().unwrap();
        let path = database(temp.path(), 3);
        // An earlier schema version is migrated on a copy before the original.
        assert_eq!(open(&path, 4).unwrap(), 2);
        // Now current, with nothing to replay: no copy is made.
        assert_eq!(open(&path, 4).unwrap(), 1);
        let settled = || Family::read(&path).unwrap().settled(4).unwrap();
        assert!(settled());
        // A newer schema version than the database carries is not settled.
        assert!(!Family::read(&path).unwrap().settled(5).unwrap());
        // An empty companion holds nothing to replay; one with content does.
        for suffix in ["-journal", "-wal"] {
            let companion = member_path(&path, suffix).unwrap();
            private(&companion, b"");
            assert!(settled());
            private(&companion, b"pending");
            assert!(!settled());
            fs::remove_file(companion).unwrap();
        }
        // A live WAL is copied first; a cleanly closed one is opened in place.
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;INSERT INTO t VALUES('later');")
            .unwrap();
        assert!(!settled());
        drop(db);
        assert!(settled());
        assert_eq!(open(&path, 4).unwrap(), 1);
        assert!(scratch(temp.path()).is_empty());
    }

    #[test]
    fn a_corrupt_current_database_is_refused_in_place_without_being_changed() {
        let temp = tempfile::tempdir().unwrap();
        let path = database(temp.path(), 4);
        let mut bytes = fs::read(&path).unwrap();
        assert!(Family::read(&path).unwrap().settled(4).unwrap());
        // Damage the table's page while leaving the header and version intact.
        let page = bytes.len() - 4096;
        bytes[page..page + 64].fill(0xff);
        private(&path, &bytes);
        assert!(Family::read(&path).unwrap().settled(4).unwrap());
        let error = open_prepared(&path, None, 4, |_| Ok(()))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("existing database and companions preserved"),
            "{error}"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(scratch(temp.path()).is_empty());
    }

    #[test]
    fn copying_again_first_removes_leftovers_even_when_admission_then_fails() {
        let temp = tempfile::tempdir().unwrap();
        let path = database(temp.path(), 4);
        // Stopped before opening the original: only copies of an unchanged family.
        let stale = temp.path().join(format!("{SCRATCH}interrupted"));
        fs::create_dir_all(stale.join("validation")).unwrap();
        fs::write(stale.join("validation/database.sqlite"), b"partial copy").unwrap();
        // Stopped while opening the original: its copy may be the only pre-open state.
        let opening = temp.path().join(format!("{SCRATCH}opening"));
        fs::create_dir_all(opening.join("original")).unwrap();
        fs::write(opening.join("original/database.sqlite"), b"pre-open copy").unwrap();
        fs::write(opening.join(OPENING), b"").unwrap();
        let kept = temp.path().join(format!("{SCRATCH}kept"));
        fs::create_dir_all(&kept).unwrap();
        fs::write(kept.join(RETAINED), b"").unwrap();
        // A log that needs replaying sends the open through the copy path,
        // which then refuses this unrecognized one.
        let wal = member_path(&path, "-wal").unwrap();
        private(&wal, b"pending");
        let error = open(&path, 4).unwrap_err().to_string();
        assert!(error.contains("unrecognized database WAL"), "{error}");
        // A start that keeps failing, such as on a full disk, still frees plain
        // copies. The marked copies and the original files stay.
        assert_eq!(
            scratch(temp.path()),
            [format!("{SCRATCH}kept"), format!("{SCRATCH}opening")]
        );
        assert_eq!(fs::read(&wal).unwrap(), b"pending");

        // The opening marker is written before the original is opened.
        fs::remove_file(&wal).unwrap();
        let runs = Cell::new(0);
        open_prepared(&path, None, 5, |db| {
            runs.set(runs.get() + 1);
            let marked = scratch(temp.path())
                .iter()
                .filter(|name| temp.path().join(name).join(OPENING).exists())
                .count();
            // The leftover above, plus this admission's own while it opens the original.
            assert_eq!(marked, runs.get());
            db.pragma_update(None, "user_version", 5)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(runs.get(), 2);
        // Only a successful admission removes a copy left while opening.
        assert_eq!(scratch(temp.path()), [format!("{SCRATCH}kept")]);
    }

    #[test]
    fn a_full_disk_names_the_space_the_copies_need_and_keeps_the_cause() {
        use std::io::{Error, ErrorKind};
        let path = Path::new("/data/control.sqlite");
        let hint = space_hint(path, 3 << 20);
        let full = hint(Error::from(ErrorKind::StorageFull).into());
        assert!(full.to_string().contains("about 3 MiB free"), "{full}");
        assert_eq!(
            full.downcast_ref::<Error>().map(Error::kind),
            Some(ErrorKind::StorageFull)
        );
        // A small database still names a nonzero amount.
        let quota = space_hint(path, 10)(Error::from(ErrorKind::QuotaExceeded).into());
        assert!(quota.to_string().contains("about 1 MiB free"), "{quota}");
        // Other failures pass through unchanged.
        let other = hint(anyhow::anyhow!("database family changed during validation"));
        assert_eq!(
            other.to_string(),
            "database family changed during validation"
        );
        let denied = hint(Error::from(ErrorKind::PermissionDenied).into());
        assert_eq!(denied.chain().count(), 1);
    }

    #[test]
    fn leftovers_of_an_interrupted_admission_are_removed_and_a_kept_copy_stays() {
        let temp = tempfile::tempdir().unwrap();
        let path = database(temp.path(), 4);
        let stale = temp.path().join(format!("{SCRATCH}interrupted"));
        fs::create_dir_all(stale.join("original")).unwrap();
        fs::write(stale.join("original/database.sqlite"), b"partial copy").unwrap();
        fs::create_dir_all(stale.join("validation")).unwrap();
        let kept = temp.path().join(format!("{SCRATCH}kept"));
        fs::create_dir_all(kept.join("original")).unwrap();
        fs::write(kept.join("original/database.sqlite"), b"only copy").unwrap();
        fs::write(kept.join(RETAINED), b"").unwrap();
        let other = temp.path().join(".sqlite-recovery-0000");
        fs::create_dir_all(&other).unwrap();
        // A file with the scratch prefix is not a scratch directory.
        fs::write(temp.path().join(format!("{SCRATCH}file")), b"").unwrap();

        // A refused open in place removes nothing.
        assert!(open_prepared(&path, None, 4, |_| -> Result<()> {
            anyhow::bail!("refused")
        })
        .is_err());
        assert_eq!(scratch(temp.path()).len(), 3);

        for current in [4, 5] {
            // Opened in place, then through the copy path after a version bump.
            open(&path, current).unwrap();
            assert_eq!(
                scratch(temp.path()),
                [format!("{SCRATCH}file"), format!("{SCRATCH}kept")]
            );
        }
        assert_eq!(
            fs::read(kept.join("original/database.sqlite")).unwrap(),
            b"only copy"
        );
        assert!(other.is_dir());
    }

    #[test]
    fn inspect_readonly_checks_wal_and_schema_without_modifying_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = database(temp.path(), 4);
        let original_bytes = fs::read(&path).unwrap();

        // Settled current database
        let ok = inspect_readonly(&path, 4);
        assert!(ok.exists);
        assert!(ok.valid_sqlite_header);
        assert_eq!(ok.schema_version, Some(4));
        assert!(ok.settled);
        assert_eq!(
            ok.wal,
            WalInspection::Valid {
                retain_recovery: false
            }
        );

        // Newer schema version than this binary supports
        let newer = inspect_readonly(&path, 3);
        assert_eq!(newer.schema_version, Some(4));
        assert_eq!(newer.supported_schema_version, 3);
        assert!(!newer.settled);

        // #665 shape: 3-byte -shm beside 0-byte -wal (refused on main; byte-for-byte unchanged)
        let shm = member_path(&path, "-shm").unwrap();
        let wal = member_path(&path, "-wal").unwrap();
        private(&shm, &[0, 0, 0]);
        private(&wal, &[]);
        let shape_665 = inspect_readonly(&path, 4);
        assert!(matches!(
            shape_665.wal,
            WalInspection::Refused { ref reason } if reason.contains("unrecognized database WAL index")
        ));
        let shm_member = shape_665
            .members
            .iter()
            .find(|m| m.suffix == "-shm")
            .unwrap();
        assert_eq!(shm_member.bytes, 3);
        assert_eq!(shm_member.header_hex, "000000");

        // Leftover admission & recovery dirs are counted without being swept
        let leftover = temp.path().join(format!("{SCRATCH}leftover"));
        let recovery = temp.path().join(".sqlite-recovery-abcd");
        fs::create_dir_all(&leftover).unwrap();
        fs::create_dir_all(&recovery).unwrap();
        let with_dirs = inspect_readonly(&path, 4);
        assert_eq!(with_dirs.leftover_admission_dirs, 1);
        assert_eq!(with_dirs.recovery_backup_dirs, 1);

        // Files remain byte-for-byte identical and leftover dirs untouched
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
        assert_eq!(fs::read(&shm).unwrap(), [0, 0, 0]);
        assert_eq!(fs::read(&wal).unwrap(), b"");
        assert!(leftover.is_dir());
        assert!(recovery.is_dir());
    }

    #[test]
    fn read_accounts_immutable_reads_registry_without_creating_shm() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = crate::Store::open(&root).unwrap();
        store.create_account("bob").unwrap();
        drop(store);

        let registry = root.join("registry.sqlite");
        let before_bytes = fs::read(&registry).unwrap();
        let shm = member_path(&registry, "-shm").unwrap();
        assert!(!shm.exists());

        let accounts = read_accounts_immutable(&registry).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "bob");
        assert!(!shm.exists(), "immutable read must not create -shm");
        assert_eq!(fs::read(&registry).unwrap(), before_bytes);
    }

    #[test]
    fn sqlite_immutable_uri_handles_special_chars_and_windows_verbatim_paths() {
        assert_eq!(
            normalize_uri_path("/tmp/a?b#c%20/registry.sqlite", false),
            "file:/tmp/a%3Fb%23c%2520/registry.sqlite?immutable=1"
        );
        assert_eq!(
            normalize_uri_path(r"\\?\C:\Users\Demo\registry.sqlite", true),
            "file:/C:/Users/Demo/registry.sqlite?immutable=1"
        );
        assert_eq!(
            normalize_uri_path(r"\\?\UNC\server\share\registry.sqlite", true),
            "file://server/share/registry.sqlite?immutable=1"
        );
    }
}
