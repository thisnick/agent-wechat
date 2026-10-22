use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};
use wechat_core::upstream::{SIMULATOR_MARKER, SIMULATOR_MARKER_CONTENT};

const MARKER: &str = ".agent-wechat-dev";
const CONTENT: &str = "agent-wechat-dev-v1\n";

fn marker(path: &Path, content: &str) -> Result<()> {
    if path.try_exists()? {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file()
                && !wechat_platform::is_link(&metadata)
                && metadata.len() == content.len() as u64,
            "invalid_simulator_marker"
        );
        ensure!(
            fs::read_to_string(path)? == content,
            "invalid_simulator_marker"
        );
    } else {
        let mut file = wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    Ok(())
}

/// Refuse existing unmarked data before opening a database or changing permissions.
pub fn prepare(path: &Path) -> Result<PathBuf> {
    if path.try_exists()? {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir() && !wechat_platform::is_link(&metadata),
            "simulator_requires_private_directory"
        );
        ensure!(
            path.join(MARKER).try_exists()? || fs::read_dir(path)?.next().is_none(),
            "refusing_unmarked_simulator_directory"
        );
    }
    wechat_platform::private_dir(path)?;
    let root = fs::canonicalize(path)?;
    marker(&root.join(MARKER), CONTENT)?;
    Ok(root)
}
pub fn hub_directory(root: &Path) -> Result<PathBuf> {
    marker(&root.join(MARKER), CONTENT)?;
    let hub = root.join("hub");
    if hub.try_exists()? {
        ensure!(
            hub.join(SIMULATOR_MARKER).try_exists()? || fs::read_dir(&hub)?.next().is_none(),
            "refusing_unmarked_hub_directory"
        );
    }
    wechat_platform::private_dir(&hub)?;
    marker(&hub.join(SIMULATOR_MARKER), SIMULATOR_MARKER_CONTENT)?;
    Ok(hub)
}
pub fn lease(root: &Path, name: &str) -> Result<File> {
    ensure!(
        matches!(name, "hub" | "upstream" | "launcher"),
        "invalid_simulator_lease"
    );
    let file = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(root.join(format!(".{name}.lock")))?;
    file.try_lock_exclusive()
        .context("simulator_process_already_running")?;
    Ok(file)
}
pub fn reset(root: &Path) -> Result<()> {
    marker(&root.join(MARKER), CONTENT)?;
    let _hub = lease(root, "hub")?;
    let _upstream = lease(root, "upstream")?;
    if root.join("hub").try_exists()? {
        let hub = hub_directory(root)?;
        fs::remove_dir_all(hub)?;
    }
    for name in [
        "upstream.sqlite",
        "upstream.sqlite-wal",
        "upstream.sqlite-shm",
    ] {
        let path = root.join(name);
        if path.try_exists()? {
            ensure!(
                fs::symlink_metadata(&path)?.is_file(),
                "invalid_simulator_database"
            );
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
pub fn token(root: &Path, name: &str) -> Result<String> {
    ensure!(
        matches!(name, "hub-token" | "simulator-token"),
        "invalid_simulator_token_file"
    );
    wechat_api::provision_token(&root.join(name), true)
}
