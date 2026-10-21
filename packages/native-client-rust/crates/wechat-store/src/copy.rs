use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: PathBuf,
    pub kind: String,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub link_target: Option<PathBuf>,
    pub mode: u32,
    pub modified_seconds: i64,
    pub modified_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub entries: Vec<Entry>,
}

pub fn inventory(root: &Path) -> Result<Manifest> {
    inventory_inner(root, false)
}
fn inventory_inner(root: &Path, official_home: bool) -> Result<Manifest> {
    fn walk(
        root: &Path,
        relative: &Path,
        entries: &mut Vec<Entry>,
        total: &mut u64,
        official_home: bool,
    ) -> Result<()> {
        if relative.components().count() > 128 {
            bail!("profile directory-depth limit exceeded");
        }
        let mut children =
            fs::read_dir(root.join(relative))?.collect::<std::io::Result<Vec<_>>>()?;
        if official_home && relative.as_os_str().is_empty() {
            children.retain(|child| {
                child.file_name() == "xwechat_files" || child.file_name() == ".xwechat"
            });
        }
        children.sort_by_key(|e| e.file_name());
        for child in children {
            if entries.len() >= 100_000 {
                bail!("profile file-count limit exceeded");
            }
            let rel = relative.join(child.file_name());
            if rel.to_str().is_none() {
                bail!("non-UTF-8 profile paths are not supported yet");
            }
            let path = root.join(&rel);
            let m = fs::symlink_metadata(&path)?;
            let mut entry = Entry {
                path: rel.clone(),
                kind: String::new(),
                bytes: 0,
                sha256: None,
                link_target: None,
                mode: file_mode(&m),
                modified_seconds: filetime::FileTime::from_last_modification_time(&m)
                    .unix_seconds(),
                modified_nanos: filetime::FileTime::from_last_modification_time(&m).nanoseconds()
                    as i64,
            };
            if wechat_platform::is_link(&m) {
                let target = fs::read_link(&path)?;
                if target.is_absolute()
                    || !fs::canonicalize(&path)
                        .context("dangling profile link")?
                        .starts_with(root)
                {
                    bail!("profile contains an external or absolute symbolic link");
                }
                entry.kind = "symlink".into();
                entry.link_target = Some(target);
            } else if m.is_file() {
                *total = total
                    .checked_add(m.len())
                    .context("profile size overflow")?;
                if *total > 512 * 1024 * 1024 * 1024 {
                    bail!("profile byte limit exceeded");
                }
                let mut f = wechat_platform::OpenOptions::new()
                    .read(true)
                    .custom_flags(wechat_platform::NOFOLLOW)
                    .open(&path)?;
                let opened = f.metadata()?;
                let same = wechat_platform::same_path_file(&path, &f)?;
                #[cfg(unix)]
                let same = same && wechat_platform::same_file(&opened, &m);
                if !same || opened.len() != m.len() {
                    bail!("profile changed during inventory");
                }
                let mut hash = Sha256::new();
                let mut buf = vec![0u8; 64 * 1024];
                let mut count = 0;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buf[..n]);
                    count += n as u64;
                    if count > m.len() {
                        bail!("profile grew during inventory");
                    }
                }
                if count != m.len() {
                    bail!("profile changed during inventory");
                }
                entry.kind = "file".into();
                entry.bytes = count;
                entry.sha256 = Some(format!("{:x}", hash.finalize()));
            } else if m.is_dir() {
                entry.kind = "directory".into();
            } else {
                bail!("unsupported special file in profile");
            }
            entries.push(entry);
            if m.is_dir() {
                walk(root, &rel, entries, total, official_home)?;
            }
        }
        Ok(())
    }
    let root = fs::canonicalize(root)?;
    if !root.is_dir() {
        bail!("profile must be a directory");
    }
    let mut entries = Vec::new();
    walk(&root, Path::new(""), &mut entries, &mut 0, official_home)?;
    Ok(Manifest {
        schema_version: 1,
        entries,
    })
}

/// Publish a directory on the same filesystem without replacing even an empty target.
pub fn publish(stage: &Path, destination: &Path) -> Result<()> {
    wechat_platform::publish_directory(stage, destination)
        .context("cannot publish profile without overwrite")
}
fn file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o777
    }
    #[cfg(windows)]
    {
        if metadata.is_dir() {
            0o700
        } else {
            0o600
        }
    }
}
/// Byte-preserving snapshot copy. This does not decode or migrate official databases.
pub fn copy_profile(source: &Path, destination: &Path, source_stopped: bool) -> Result<Manifest> {
    copy_inner(source, destination, source_stopped, false)
}
pub(crate) fn copy_official_home(
    source: &Path,
    destination: &Path,
    source_stopped: bool,
) -> Result<Manifest> {
    copy_inner(source, destination, source_stopped, true)
}
fn copy_inner(
    source: &Path,
    destination: &Path,
    source_stopped: bool,
    official_home: bool,
) -> Result<Manifest> {
    if !source_stopped {
        bail!("a stopped source or consistent snapshot must be explicitly confirmed");
    }
    let source = fs::canonicalize(source)?;
    let parent = fs::canonicalize(destination.parent().context("destination needs a parent")?)?;
    let name = destination
        .file_name()
        .context("destination needs a name")?;
    let destination = parent.join(name);
    let overlap = if official_home {
        ["xwechat_files", ".xwechat"].iter().any(|name| {
            let root = source.join(name);
            parent.starts_with(&root) || root.starts_with(&destination)
        })
    } else {
        parent.starts_with(&source) || source.starts_with(&destination)
    };
    if overlap {
        bail!("source and destination overlap");
    }
    if fs::symlink_metadata(&destination).is_ok() {
        bail!("destination already exists");
    }
    let _lease = crate::profile_lease(&source)?;
    let before = inventory_inner(&source, official_home)?;
    let stage = tempfile::Builder::new()
        .prefix(".wx-copy-")
        .tempdir_in(&parent)?;
    for entry in &before.entries {
        let target = stage.path().join(&entry.path);
        match entry.kind.as_str() {
            "directory" => {
                fs::create_dir(&target)?;
            }
            "file" => {
                let input = wechat_platform::OpenOptions::new()
                    .read(true)
                    .custom_flags(wechat_platform::NOFOLLOW)
                    .open(source.join(&entry.path))?;
                let mut output = wechat_platform::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)?;
                let copied = std::io::copy(&mut input.take(entry.bytes + 1), &mut output)?;
                if copied != entry.bytes {
                    bail!("profile changed during copy");
                }
                output.sync_all()?;
            }
            // Make links after regular files/directories; never traverse them while copying.
            "symlink" => {}
            _ => bail!("unsupported manifest entry"),
        }
    }
    for entry in &before.entries {
        if let Some(target) = &entry.link_target {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, stage.path().join(&entry.path))?;
            #[cfg(windows)]
            {
                let _ = target;
                bail!("profile symbolic-link copying is unsupported on Windows");
            }
        }
    }
    for entry in before.entries.iter().rev() {
        if entry.kind == "symlink" {
            filetime::set_symlink_file_times(
                stage.path().join(&entry.path),
                filetime::FileTime::from_unix_time(
                    entry.modified_seconds,
                    entry.modified_nanos as u32,
                ),
                filetime::FileTime::from_unix_time(
                    entry.modified_seconds,
                    entry.modified_nanos as u32,
                ),
            )?;
        } else {
            let target = stage.path().join(&entry.path);
            #[cfg(unix)]
            fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))?;
            #[cfg(windows)]
            wechat_platform::protect_path(&target, target.is_dir())?;
            filetime::set_file_mtime(
                &target,
                filetime::FileTime::from_unix_time(
                    entry.modified_seconds,
                    entry.modified_nanos as u32,
                ),
            )?;
            if target.is_dir() {
                wechat_platform::sync_dir(&target)?;
            } else {
                File::open(&target)?.sync_all()?;
            }
        }
    }
    if inventory(stage.path())? != before || inventory_inner(&source, official_home)? != before {
        bail!("profile changed or copied bytes/metadata did not verify");
    }
    wechat_platform::sync_dir(stage.path())?;
    publish(stage.path(), &destination)?;
    Ok(before)
}
