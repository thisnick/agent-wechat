//! SQLite may recover or unlink companions even when schema admission fails.
//! Validate an owned copy before allowing it to open an existing store family.
//!
//! A database that has no journal or WAL to replay and is already on the
//! current schema version is opened in place: neither SQLite nor a migration has
//! anything to change, so the copies would protect nothing. It is still checked
//! for corruption before use.
//!
//! The scratch copies are removed when admission returns. A process that is
//! stopped part-way leaves them behind, so the next successful admission of a
//! database removes leftover scratch directories beside it.
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
/// database. One kept on purpose carries a marker and is left alone.
fn sweep_scratch(parent: &Path) {
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
        sweep_scratch(parent);
        return Ok((db, value));
    }
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
    let opened = (|| -> Result<(Connection, T)> {
        let mut db = connect(path)?;
        let value = prepare(&mut db)?;
        Ok((db, value))
    })();
    match opened {
        Ok(result) => {
            drop(scratch);
            sweep_scratch(parent);
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
                        // Best effort: without the marker a later successful
                        // admission treats this directory as a leftover.
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

pub(crate) fn remember(cache: &mut AdmissionCache, path: &Path) -> Result<()> {
    // Retained handles establish identity across platforms; bound their number
    // when a daemon serves a large account registry.
    if cache.len() >= 64 && !cache.contains_key(path) {
        cache.pop_first();
    }
    cache.insert(path.to_owned(), Family::read(path)?);
    Ok(())
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

        // A refused open removes nothing.
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
}
