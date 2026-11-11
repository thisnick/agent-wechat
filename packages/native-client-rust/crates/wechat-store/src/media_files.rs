//! Encrypted media payloads kept as private files beside an account's
//! `control.sqlite`. The database row names its file; the bytes are the same
//! vault ciphertext that earlier schema versions stored in a BLOB column.
//!
//! A file is published under its final name before the row that names it
//! commits, and a replaced or released file is removed after that commit. A
//! crash can therefore leave a file that no row names, never a row whose file
//! was not written. `sweep` removes those leftovers.
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const TEMPORARY: &str = ".tmp-";
const SUFFIX: &str = ".bin";

/// The directory holding one account's payload files.
pub(crate) fn directory(account_dir: &Path) -> PathBuf {
    account_dir.join("agent").join("media")
}
fn valid(name: &str) -> bool {
    name.strip_suffix(SUFFIX).is_some_and(|stem| {
        stem.len() == 32
            && stem
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
/// Write one payload and return the file name to store in its row.
pub(crate) fn write(directory: &Path, sealed: &[u8]) -> Result<String> {
    wechat_platform::private_dir(directory)?;
    let stem = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("{stem}{SUFFIX}");
    let temporary = directory.join(format!("{TEMPORARY}{stem}"));
    let written = (|| -> Result<()> {
        let mut file = wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(sealed)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, directory.join(&name))?;
        wechat_platform::sync_dir(directory)?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(directory.join(&name));
        return Err(error.context("cannot store media file"));
    }
    Ok(name)
}
/// Read one payload. A file that was deleted is reported as `media_not_cached`;
/// nothing is downloaded or rebuilt to replace it.
pub(crate) fn read(directory: &Path, name: &str) -> Result<Vec<u8>> {
    ensure!(valid(name), "invalid media file name");
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!("media_not_cached"),
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.is_file() && !wechat_platform::is_link(&metadata),
            "media file must be a regular file"
        ),
    }
    let mut file = wechat_platform::OpenOptions::new()
        .read(true)
        .open(&path)
        .context("cannot open media file")?;
    ensure!(
        wechat_platform::private_file(&file)? && wechat_platform::same_path_file(&path, &file)?,
        "media file must be private and stable"
    );
    let mut sealed = vec![];
    file.read_to_end(&mut sealed)?;
    Ok(sealed)
}
/// Remove a file whose row no longer names it. A missing file is not an error.
pub(crate) fn remove(directory: &Path, name: &str) {
    if valid(name) {
        let _ = fs::remove_file(directory.join(name));
    }
}
/// Remove every payload or unfinished temporary file that `referenced` does not
/// name. Entries this module did not create are left alone.
pub(crate) fn sweep(directory: &Path, referenced: &BTreeSet<String>) -> Result<usize> {
    let entries = match fs::read_dir(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        entries => entries?,
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let ours = name.starts_with(TEMPORARY) || (valid(&name) && !referenced.contains(&name));
        if ours && entry.file_type()?.is_file() && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn files_round_trip_and_only_unreferenced_ones_are_swept() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let kept = write(&directory, b"kept").unwrap();
        let orphan = write(&directory, b"orphan").unwrap();
        fs::write(directory.join(".tmp-unfinished"), b"partial").unwrap();
        fs::write(directory.join("notes.txt"), b"not ours").unwrap();
        assert_eq!(read(&directory, &kept).unwrap(), b"kept");
        assert_eq!(
            sweep(&directory, &BTreeSet::from([kept.clone()])).unwrap(),
            2
        );
        assert_eq!(read(&directory, &kept).unwrap(), b"kept");
        assert_eq!(
            read(&directory, &orphan).unwrap_err().to_string(),
            "media_not_cached"
        );
        assert!(directory.join("notes.txt").is_file());
        assert!(!directory.join(".tmp-unfinished").exists());
        assert!(read(&directory, "../control.sqlite").is_err());
        assert_eq!(
            sweep(&temp.path().join("absent"), &BTreeSet::new()).unwrap(),
            0
        );
    }
}
