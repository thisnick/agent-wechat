//! One-time journal-mode conversion for databases a daemon created itself.
//!
//! Older daemon builds created every profile database with the rollback journal
//! (`journal_mode=DELETE`). A stopped official Linux 4.1.13.26 profile has `-wal`
//! and `-shm` files beside every database, so the official client keeps them in
//! WAL mode, and new daemon profiles now do the same. WAL lets the owned reader
//! pin a snapshot and query in parallel with the writer; with a rollback journal
//! every read holds the account lock for its whole duration.
//!
//! The caller must only pass profiles it created. An attached or imported
//! official profile is never converted: its journal mode belongs to whichever
//! client owns it.
use crate::{
    keys::verify_first_page, projection::ownership, snapshot::configure_cipher, KeyFile,
    ProfileLocation, Result, WriteOptions,
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{fs, io::Read, time::Duration};

/// Outcome of one conversion pass; every database lands in exactly one list.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WalConversionReport {
    /// Databases switched from a rollback journal to WAL in this pass.
    pub converted: Vec<String>,
    /// Databases that were already in WAL mode and were not modified.
    pub already_wal: Vec<String>,
    /// Databases left unchanged, with a short reason (missing, unreadable,
    /// busy, unsupported journal mode). A later pass retries them.
    pub skipped: Vec<(String, String)>,
}

/// Convert every keyed database of a daemon-created profile to WAL.
///
/// Preconditions the caller owns: the daemon created this profile, it holds
/// exclusive ownership and the account lock, and no reader, push worker or
/// projection has opened it yet. `PRAGMA journal_mode=WAL` needs the only open
/// connection; if another connection holds the file anyway, SQLite refuses with
/// a busy error and the database is reported as skipped, never corrupted.
///
/// Per database and idempotent: a database already in WAL is not touched, and
/// one that cannot be opened (missing, wrong key, busy) is skipped without
/// failing the rest, so a crash or skip part-way is finished by the next pass.
/// A leftover `-journal` file is not removed here; opening the database with
/// its key lets SQLite's normal hot-journal recovery roll it back first.
pub fn convert_profile_to_wal(
    location: &ProfileLocation,
    keys: &KeyFile,
    options: &WriteOptions,
) -> Result<WalConversionReport> {
    ownership(location, keys, options)?;
    let mut report = WalConversionReport::default();
    for relative in keys.databases.keys() {
        match convert_one(location, keys, relative) {
            Ok(true) => report.converted.push(relative.clone()),
            Ok(false) => report.already_wal.push(relative.clone()),
            Err(reason) => report.skipped.push((relative.clone(), reason.into())),
        }
    }
    Ok(report)
}

fn convert_one(
    location: &ProfileLocation,
    keys: &KeyFile,
    relative: &str,
) -> std::result::Result<bool, &'static str> {
    if !crate::keys::valid_relative_path(relative) {
        return Err("invalid_path");
    }
    let path = location.storage.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|_| "missing")?;
    if !metadata.is_file() || wechat_platform::is_link(&metadata) {
        return Err("unsupported_file");
    }
    let key = &keys.databases[relative];
    let mut page = [0u8; 4096];
    fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut page))
        .map_err(|_| "unreadable")?;
    // A plaintext database is not one this daemon created; never touch it.
    if &page[..16] == b"SQLite format 3\0" {
        return Err("unencrypted");
    }
    // Opened without CREATE so a missing file can never appear as a new empty one.
    let c = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "unreadable")?;
    c.busy_timeout(Duration::from_millis(250))
        .map_err(|_| "unreadable")?;
    if !verify_first_page(&page, key.raw_page_key.expose(), &key.salt) {
        return Err("key_mismatch");
    }
    configure_cipher(&c, &key.raw_page_key, relative).map_err(|_| "unreadable")?;
    // The first read also lets SQLite recover a hot rollback journal.
    c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|_| "unreadable")?;
    let mode: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(|_| "unreadable")?;
    if mode.eq_ignore_ascii_case("wal") {
        return Ok(false);
    }
    if !mode.eq_ignore_ascii_case("delete") {
        // truncate/persist/memory/off: not a layout this daemon ever wrote.
        return Err("unsupported_journal_mode");
    }
    let new_mode: String = c
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .map_err(|_| "busy")?;
    if !new_mode.eq_ignore_ascii_case("wal") {
        return Err("busy");
    }
    // Leave nothing in a sidecar: only the main file is covered by later copies
    // that were written for the rollback layout.
    let (busy, _, _): (i64, i64, i64) = c
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|_| "checkpoint_failed")?;
    if busy != 0 {
        return Err("checkpoint_failed");
    }
    c.close().map_err(|_| "close_failed")?;
    Ok(true)
}

impl From<WalConversionReport> for serde_json::Value {
    fn from(r: WalConversionReport) -> Self {
        serde_json::json!({
            "converted": r.converted.len(),
            "alreadyWal": r.already_wal.len(),
            "skipped": r.skipped.iter().map(|(d, why)| serde_json::json!({"database": d, "reason": why})).collect::<Vec<_>>(),
        })
    }
}
