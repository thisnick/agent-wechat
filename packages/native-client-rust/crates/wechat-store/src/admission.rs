//! SQLite may recover or unlink companions even when schema admission fails.
//! Validate an owned copy before allowing it to open an existing store family.
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
    let scratch = wechat_platform::private_tempdir_in(parent, ".sqlite-admission-")?;
    let original = scratch.path().join("original");
    let validation = scratch.path().join("validation");
    wechat_platform::private_dir(&original)?;
    wechat_platform::private_dir(&validation)?;
    let saved = original.join("database.sqlite");
    family.copy_to(path, &saved)?;
    let copied = Family::read(&saved)?;
    let retain_unverified = wal::validate(&copied)?;
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
        Ok(result) => Ok(result),
        Err(error) => {
            // A successful private validation followed by a source I/O failure
            // is uncertain. Keep the pre-open family for explicit recovery.
            let recovery = match retained {
                Some(path) => path,
                None => match retain_recovery(&original, parent) {
                    Ok(path) => path,
                    Err(_) => {
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
