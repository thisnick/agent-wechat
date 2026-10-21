//! Saved-login storage is separate from page keys, active business-session keys
//! and SyncKey. This module only handles caller-authorized stopped files. It never
//! authenticates, launches a process, obtains credentials or makes network calls.
use crate::*;
use md5::{Digest, Md5};
use rusqlite::OptionalExtension;
use std::{fs, path::Path};
use zeroize::{Zeroize, Zeroizing};

/// One accepted credential generation. No Debug/serde implementation is provided.
/// The caller binds this generation to bootstrap username/UIN/device first.
pub struct SavedLoginProjection<'a> {
    pub auto_auth_key: &'a [u8],
    pub cookie: &'a [u8],
    pub key_info: &'a [u8],
    /// Native stored numeric launch policy. The inspected saved-device fixture uses
    /// 3; a generic boolean is not interchangeable with this value.
    pub auto_login_flag: u32,
    pub pc_account_name: Option<&'a str>,
    pub local_device_id64: Option<u64>,
}
fn scalar(input: &[u8]) -> Result<u64> {
    if input.len() > 10 {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut n = 0u64;
    for (i, b) in input.iter().copied().enumerate() {
        if i == 9 && b > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        n |= u64::from(b & 127) << (7 * i);
        if b & 128 == 0 {
            return if i + 1 == input.len() {
                Ok(n)
            } else {
                Err(ProfileError::InvalidMetadata)
            };
        }
    }
    Err(ProfileError::InvalidMetadata)
}
fn scalar_bytes(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    while n >= 128 {
        out.push((n as u8 & 127) | 128);
        n >>= 7
    }
    out.push(n as u8);
    out
}
pub(crate) fn validate_auto_auth_key(input: &[u8]) -> Result<()> {
    let fields = crate::projection::proto_fields(input)?;
    if fields.len() != 2 {
        return Err(ProfileError::InvalidMetadata);
    }
    for tag in [1, 2] {
        let values = fields
            .iter()
            .filter(|f| f.0 == tag && f.1 == 2)
            .collect::<Vec<_>>();
        if values.len() != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        let nested = crate::projection::proto_fields(values[0].2)?;
        if nested.len() != 2 {
            return Err(ProfileError::InvalidMetadata);
        }
        let lens = nested
            .iter()
            .filter(|f| f.0 == 1 && f.1 == 0)
            .collect::<Vec<_>>();
        let bytes = nested
            .iter()
            .filter(|f| f.0 == 2 && f.1 == 2)
            .collect::<Vec<_>>();
        if lens.len() != 1
            || bytes.len() != 1
            || bytes[0].2.is_empty()
            || scalar(lens[0].2)? != bytes[0].2.len() as u64
        {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    Ok(())
}
pub(crate) fn saved_login_values(
    username: &str,
    input: &SavedLoginProjection<'_>,
) -> Result<Vec<(String, Option<Vec<u8>>)>> {
    validate_auto_auth_key(input.auto_auth_key)?;
    if input.cookie.len() > 255 || input.key_info.is_empty() || input.key_info.len() > 1024 * 1024 {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut out = vec![];
    for (name, bytes) in [
        ("mmkv_key_auto_auth_key", input.auto_auth_key),
        ("mmkv_key_latest_login_username", username.as_bytes()),
    ] {
        out.push((name.into(), Some(mmkv_string_value(bytes)?.to_vec())))
    }
    if !input.cookie.is_empty() {
        out.push((
            "mmkv_key_server_id".into(),
            Some(mmkv_string_value(input.cookie)?.to_vec()),
        ))
    }
    out.push((
        "mmkv_key_auto_login_flag".into(),
        Some(scalar_bytes(input.auto_login_flag as u64)),
    ));
    if let Some(name) = input.pc_account_name {
        if name.len() > 4096 || name.chars().any(char::is_control) {
            return Err(ProfileError::InvalidMetadata);
        }
        out.push((
            "mmkv_key_pc_account_name".into(),
            Some(mmkv_string_value(name.as_bytes())?.to_vec()),
        ))
    }
    if let Some(local) = input.local_device_id64 {
        if local == 0 || local > i64::MAX as u64 {
            return Err(ProfileError::InvalidMetadata);
        }
        out.push(("key_wechat_local_id_64".into(), Some(scalar_bytes(local))));
    }
    Ok(out)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyInfoSource {
    LatestDatabaseRow,
    LegacyFallback,
}
/// Authorized saved material only; it does not imply a currently valid session.
/// Values are zeroized and intentionally do not implement Debug or Serialize.
pub struct ProfileSavedLogin {
    pub identity: ProfileIdentity,
    pub auto_auth_key: Zeroizing<Vec<u8>>,
    pub cookie: Zeroizing<Vec<u8>>,
    pub key_info: Zeroizing<Vec<u8>>,
    pub key_info_source: KeyInfoSource,
    pub auto_login_flag: u32,
    pub logout_locked: Option<bool>,
    pub pc_account_name: Option<Zeroizing<Vec<u8>>>,
}
/// Read identity and the exact native latest-row/legacy selection from a stopped
/// full HOME. Database/WAL/SHM queries use an isolated working copy; encrypted
/// profile DB keys and native login credentials are never substituted.
pub fn read_profile_saved_login(
    home: impl AsRef<Path>,
    format_key: &[u8],
    stopped_or_consistent_snapshot: bool,
) -> Result<ProfileSavedLogin> {
    if !stopped_or_consistent_snapshot {
        return Err(ProfileError::SnapshotRequired);
    }
    let home = fs::canonicalize(home)?;
    let global = "xwechat_files/all_users/config/global_config";
    let data = crate::media::read_cache(&home, Path::new(global), 4 * 1024 * 1024 + 4)?;
    let meta = crate::media::read_cache(&home, Path::new(&format!("{global}.crc")), 4096)?;
    let identity = read_mmkv_identity(&data, &meta, format_key)?;
    crate::official_account_directory(&identity.username, identity.uin)?;
    let decoded = decode_mmkv_records(&data, &meta, format_key)?;
    let token = decoded
        .string_value("mmkv_key_auto_auth_key")?
        .ok_or(ProfileError::InvalidMetadata)?;
    validate_auto_auth_key(&token)?;
    let cookie = decoded
        .string_value("mmkv_key_server_id")?
        .unwrap_or_else(|| Zeroizing::new(vec![]));
    if cookie.len() > 255 {
        return Err(ProfileError::InvalidMetadata);
    }
    let flag = u32::try_from(scalar(
        decoded
            .raw_value("mmkv_key_auto_login_flag")
            .ok_or(ProfileError::InvalidMetadata)?,
    )?)
    .map_err(|_| ProfileError::InvalidMetadata)?;
    let locked = decoded
        .raw_value("mmkv_key_logout_locked_flag")
        .map(scalar)
        .transpose()?
        .map(|n| match n {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProfileError::InvalidMetadata),
        })
        .transpose()?;
    let pc_account_name = decoded.string_value("mmkv_key_pc_account_name")?;
    let (key_info, key_info_source) = read_key_info(&home, &identity.username)?;
    Ok(ProfileSavedLogin {
        identity,
        auto_auth_key: token,
        cookie,
        key_info,
        key_info_source,
        auto_login_flag: flag,
        logout_locked: locked,
        pc_account_name,
    })
}
fn read_key_info(home: &Path, username: &str) -> Result<(Zeroizing<Vec<u8>>, KeyInfoSource)> {
    let (relative, legacy) = key_info_relative(username)?;
    let source = home.join(&relative);
    if source.exists() {
        // Main/WAL/SHM form one caller-confirmed stopped generation. Opening only the
        // copied files avoids changes to source SHM reader lock metadata.
        let temp = tempfile::tempdir()?;
        for suffix in ["", "-wal", "-shm"] {
            let path = format!("{relative}{suffix}");
            match crate::media::read_cache(home, Path::new(&path), 16 * 1024 * 1024) {
                Ok(bytes) => fs::write(temp.path().join(format!("key_info.db{suffix}")), bytes)?,
                Err(ProfileError::NotCached) if !suffix.is_empty() => {}
                Err(e) => return Err(e),
            }
        }
        let c = rusqlite::Connection::open_with_flags(
            temp.path().join("key_info.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.execute_batch("PRAGMA trusted_schema=OFF;PRAGMA temp_store=MEMORY;")?;
        let expected = ["user_name_md5", "key_md5", "key_info_md5", "key_info_data"];
        let actual = c
            .prepare("PRAGMA table_info(LoginKeyInfoTable)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if actual.iter().map(String::as_str).collect::<Vec<_>>() != expected {
            return Err(ProfileError::UnsupportedSchema("login/key_info.db".into()));
        }
        let hash = format!("{:x}", Md5::digest(username.as_bytes()));
        let row=c.query_row("SELECT key_info_md5,key_info_data FROM LoginKeyInfoTable WHERE user_name_md5=?1 ORDER BY rowid DESC LIMIT 1",[hash],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<Vec<u8>>>(1)?))).optional()?;
        if let Some((Some(hash), Some(data))) = row {
            if !data.is_empty()
                && data.len() <= 1024 * 1024
                && format!("{:x}", Md5::digest(&data)) == hash
            {
                return Ok((Zeroizing::new(data), KeyInfoSource::LatestDatabaseRow));
            }
        }
    }
    let data = crate::media::read_cache(home, Path::new(&legacy), 1024 * 1024)?;
    if data.is_empty() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok((Zeroizing::new(data), KeyInfoSource::LegacyFallback))
}

/// Restore only the existing account's key-info for a new approval/QR exchange.
/// This remains useful when saved AutoAuth trust is expired or unavailable; a
/// fresh approval does not justify dropping the installation's key-info.
pub fn read_profile_key_info(
    home: impl AsRef<Path>,
    username: &str,
    stopped_or_consistent_snapshot: bool,
) -> Result<(Zeroizing<Vec<u8>>, KeyInfoSource)> {
    if !stopped_or_consistent_snapshot {
        return Err(ProfileError::SnapshotRequired);
    }
    crate::official_account_directory(username, 1)?;
    read_key_info(&fs::canonicalize(home)?, username)
}

/// Stage accepted saved credentials against an already bound identity store. No
/// filesystem mutation occurs, so a coordinator can journal the complete sealed
/// generation before publishing the MMKV pair and legacy key-info file.
pub fn stage_saved_login_projection(
    data: &[u8],
    metadata: &[u8],
    format_key: &[u8],
    username: &str,
    input: &SavedLoginProjection<'_>,
) -> Result<StagedMmkv> {
    let identity = read_mmkv_identity(data, metadata, format_key)?;
    if identity.username != username {
        return Err(ProfileError::AccountMismatch);
    }
    let mut values = saved_login_values(username, input)?;
    let staged = stage_mmkv_values(data, metadata, format_key, &values);
    values.zeroize();
    staged
}
/// Atomic legacy fallback publication with an expected old-generation SHA-256.
/// None means a new file, never permission to overwrite unknown contents. The
/// coordinator journals/retries this step under its exclusive profile lease.
pub fn publish_legacy_key_info(
    path: impl AsRef<Path>,
    bytes: &[u8],
    expected_sha256: Option<&str>,
    options: &WriteOptions,
) -> Result<bool> {
    use sha2::Sha256;
    use std::io::Write;
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    if bytes.is_empty()
        || bytes.len() > 1024 * 1024
        || expected_sha256.is_some_and(|s| !crate::media::digest_hex(s, 64))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let path = path.as_ref();
    if path.file_name().is_none_or(|n| n != "key_info.dat") {
        return Err(ProfileError::InvalidQuery);
    }
    let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
    let canonical = fs::canonicalize(parent)?;
    if canonical != parent {
        return Err(ProfileError::UnsupportedFile);
    }
    let old = match crate::media::read_cache(parent, Path::new("key_info.dat"), 1024 * 1024) {
        Ok(old) => Some(Zeroizing::new(old)),
        Err(ProfileError::NotCached) => None,
        Err(e) => return Err(e),
    };
    if old.as_deref().is_some_and(|old| old.as_slice() == bytes) {
        return Ok(false);
    }
    let matches = match (&old, expected_sha256) {
        (None, None) => true,
        (Some(old), Some(expected)) => {
            hex::encode(Sha256::digest(old.as_slice())) == expected.to_ascii_lowercase()
        }
        _ => false,
    };
    if !matches {
        return Err(ProfileError::SourceChanged);
    }
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    if old.is_none() {
        staged
            .persist_noclobber(path)
            .map_err(|e| ProfileError::Io(e.error))?;
    } else {
        staged
            .persist(path)
            .map_err(|e| ProfileError::Io(e.error))?;
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(true)
}

/// Hash-only expected generations, suitable for embedding in a coordinator's
/// encrypted journal. No credential bytes are returned by this plan.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedKeyInfoPlan {
    pub username_sha256: String,
    pub database_present: bool,
    pub expected_selection_sha256: Option<String>,
    pub expected_legacy_sha256: Option<String>,
}
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedKeyInfoPublication {
    pub database_changed: bool,
    pub legacy_changed: bool,
}
fn key_info_relative(username: &str) -> Result<(String, String)> {
    // Apply the same conservative path-component validation as HOME bootstrap.
    crate::official_account_directory(username, 1)?;
    if !crate::media::valid_component(username) {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok((
        format!("xwechat_files/all_users/login/{username}/key_info.db"),
        format!(".xwechat/login/{username}/key_info.dat"),
    ))
}
fn login_schema(c: &rusqlite::Connection) -> Result<()> {
    let columns = c
        .prepare("PRAGMA table_info(LoginKeyInfoTable)")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let expected = [
        ("user_name_md5", "TEXT"),
        ("key_md5", "TEXT"),
        ("key_info_md5", "TEXT"),
        ("key_info_data", "BLOB"),
    ]
    .into_iter()
    .map(|(n, t)| (n.to_string(), t.to_string(), false, None, 0))
    .collect::<Vec<_>>();
    let indices = c
        .prepare("PRAGMA index_list(LoginKeyInfoTable)")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let triggers: i64 = c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='LoginKeyInfoTable'",
        [],
        |r| r.get(0),
    )?;
    if columns != expected
        || indices != vec![("LoginKeyInfoTable_USER_KEYINFO".into(), true, false)]
        || triggers != 0
    {
        return Err(ProfileError::UnsupportedSchema("login/key_info.db".into()));
    }
    let indexed = c
        .prepare("PRAGMA index_info(LoginKeyInfoTable_USER_KEYINFO)")?
        .query_map([], |r| r.get::<_, String>(2))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let ordering = c
        .prepare("PRAGMA index_xinfo(LoginKeyInfoTable_USER_KEYINFO)")?
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let keys = ordering.into_iter().filter(|row| row.3).collect::<Vec<_>>();
    let expected_keys = vec![
        (Some("user_name_md5".into()), false, "BINARY".into(), true),
        (Some("key_info_md5".into()), false, "BINARY".into(), true),
    ];
    if indexed != ["user_name_md5", "key_info_md5"] || keys != expected_keys {
        return Err(ProfileError::UnsupportedSchema("login/key_info.db".into()));
    }
    Ok(())
}
type LoginRow = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
);
fn selected_login_row(c: &rusqlite::Connection, username: &str) -> Result<Option<LoginRow>> {
    Ok(c.query_row("SELECT rowid,user_name_md5,key_md5,key_info_md5,key_info_data FROM LoginKeyInfoTable WHERE user_name_md5=?1 ORDER BY rowid DESC LIMIT 1",[format!("{:x}",Md5::digest(username.as_bytes()))],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?)
}
fn login_selection_hash(row: &Option<LoginRow>) -> Result<String> {
    use sha2::Sha256;
    let bytes = Zeroizing::new(serde_json::to_vec(row).map_err(|_| ProfileError::InvalidMetadata)?);
    Ok(hex::encode(Sha256::digest(bytes.as_slice())))
}
/// Inspect only caller-confirmed stopped files, copying DB/WAL/SHM before SQLite.
/// Existing databases must have the exact known unique-index/table shape.
pub fn plan_saved_key_info(
    home: impl AsRef<Path>,
    username: &str,
    stopped: bool,
) -> Result<SavedKeyInfoPlan> {
    use sha2::Sha256;
    if !stopped {
        return Err(ProfileError::SnapshotRequired);
    }
    let home = fs::canonicalize(home)?;
    let (relative, legacy) = key_info_relative(username)?;
    let database = home.join(&relative);
    let database_present = fs::symlink_metadata(&database).is_ok();
    let expected_selection_sha256 = if database_present {
        let temp = tempfile::tempdir()?;
        for suffix in ["", "-wal", "-shm"] {
            match crate::media::read_cache(
                &home,
                Path::new(&format!("{relative}{suffix}")),
                16 * 1024 * 1024,
            ) {
                Ok(bytes) => fs::write(temp.path().join(format!("key_info.db{suffix}")), bytes)?,
                Err(ProfileError::NotCached) if !suffix.is_empty() => {}
                Err(e) => return Err(e),
            }
        }
        let c = rusqlite::Connection::open_with_flags(
            temp.path().join("key_info.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.execute_batch("PRAGMA trusted_schema=OFF;PRAGMA temp_store=MEMORY;")?;
        login_schema(&c)?;
        Some(login_selection_hash(&selected_login_row(&c, username)?)?)
    } else {
        None
    };
    let expected_legacy_sha256 =
        match crate::media::read_cache(&home, Path::new(&legacy), 1024 * 1024) {
            Ok(bytes) => Some(hex::encode(Sha256::digest(bytes))),
            Err(ProfileError::NotCached) => None,
            Err(e) => return Err(e),
        };
    Ok(SavedKeyInfoPlan {
        username_sha256: hex::encode(Sha256::digest(username.as_bytes())),
        database_present,
        expected_selection_sha256,
        expected_legacy_sha256,
    })
}
fn create_login_parent(home: &Path, relative: &Path) -> Result<()> {
    let mut path = home.to_path_buf();
    for part in relative
        .parent()
        .ok_or(ProfileError::InvalidQuery)?
        .components()
    {
        path.push(part.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => return Err(ProfileError::UnsupportedFile),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                }
                fs::File::open(path.parent().ok_or(ProfileError::InvalidQuery)?)?.sync_all()?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// Publish native-shaped key-info, retaining unrelated accounts/history. The
/// inspected writer supplies key_md5='' (not a digest of the new credential).
/// Its known UNIQUE(user_name_md5,key_info_md5) tuple is replaced only when the
/// older tuple contains identical bytes and the same native empty key_md5.
/// If no DB exists, use the native raw legacy fallback without inventing a DB.
pub fn publish_saved_key_info(
    home: impl AsRef<Path>,
    username: &str,
    plan: &SavedKeyInfoPlan,
    bytes: &[u8],
    options: &WriteOptions,
) -> Result<SavedKeyInfoPublication> {
    publish_saved_key_info_guarded(home, username, plan, bytes, options, || true)
}
/// The coordinator must recheck its authorization fence immediately before each
/// independently durable publication. A false guard preserves the caller's
/// encrypted intent for replay; it cannot roll back an earlier committed file.
pub fn publish_saved_key_info_guarded(
    home: impl AsRef<Path>,
    username: &str,
    plan: &SavedKeyInfoPlan,
    bytes: &[u8],
    options: &WriteOptions,
    mut allowed: impl FnMut() -> bool,
) -> Result<SavedKeyInfoPublication> {
    use rusqlite::params;
    use sha2::Sha256;
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    if bytes.is_empty()
        || bytes.len() > 1024 * 1024
        || plan.username_sha256 != hex::encode(Sha256::digest(username.as_bytes()))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let home = fs::canonicalize(home)?;
    let (relative, legacy) = key_info_relative(username)?;
    let path = home.join(&relative);
    let present = fs::symlink_metadata(&path).is_ok();
    if present != plan.database_present {
        return Err(ProfileError::SourceChanged);
    }
    // Preflight the separate legacy file before the database transaction. Replay
    // accepts exact target bytes; an unrelated generation is never overwritten.
    match crate::media::read_cache(&home, Path::new(&legacy), 1024 * 1024) {
        Ok(old) if old == bytes => {}
        Ok(old)
            if plan.expected_legacy_sha256.as_deref()
                == Some(hex::encode(Sha256::digest(&old)).as_str()) => {}
        Err(ProfileError::NotCached) if plan.expected_legacy_sha256.is_none() => {}
        Ok(_) | Err(ProfileError::NotCached) => return Err(ProfileError::SourceChanged),
        Err(e) => return Err(e),
    }
    let mut database_changed = false;
    if present {
        if fs::canonicalize(&path)? != path || !fs::symlink_metadata(&path)?.is_file() {
            return Err(ProfileError::UnsupportedFile);
        }
        let mut c = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.execute_batch(
            "PRAGMA trusted_schema=OFF;PRAGMA synchronous=FULL;PRAGMA temp_store=MEMORY;",
        )?;
        login_schema(&c)?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let selected = selected_login_row(&tx, username)?;
        let digest = format!("{:x}", Md5::digest(bytes));
        let already = selected.as_ref().is_some_and(|r| {
            r.3.as_deref() == Some(digest.as_str()) && r.4.as_deref() == Some(bytes)
        });
        if !already {
            if Some(login_selection_hash(&selected)?) != plan.expected_selection_sha256 {
                return Err(ProfileError::SourceChanged);
            }
            let user_hash = format!("{:x}", Md5::digest(username.as_bytes()));
            let duplicate:Option<(Option<String>,Option<Vec<u8>>)>=tx.query_row("SELECT key_md5,key_info_data FROM LoginKeyInfoTable WHERE user_name_md5=?1 AND key_info_md5=?2",params![user_hash,digest],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if duplicate.as_ref().is_some_and(|(key, data)| {
                key.as_deref() != Some("") || data.as_deref() != Some(bytes)
            }) {
                return Err(ProfileError::InvalidMetadata);
            }
            if !allowed() {
                return Err(ProfileError::SourceChanged);
            }
            tx.execute("INSERT OR REPLACE INTO LoginKeyInfoTable(user_name_md5,key_md5,key_info_md5,key_info_data)VALUES(?1,'',?2,?3)",params![user_hash,digest,bytes])?;
            database_changed = true;
        }
        tx.commit()?;
    }
    create_login_parent(&home, Path::new(&legacy))?;
    if !allowed() {
        return Err(ProfileError::SourceChanged);
    }
    let legacy_changed = publish_legacy_key_info(
        home.join(legacy),
        bytes,
        plan.expected_legacy_sha256.as_deref(),
        options,
    )?;
    Ok(SavedKeyInfoPublication {
        database_changed,
        legacy_changed,
    })
}
/// Preserve the existing desktop auto-login policy while reading a bound store.
pub fn saved_login_policy(data: &[u8], metadata: &[u8], format_key: &[u8]) -> Result<u32> {
    let records = decode_mmkv_records(data, metadata, format_key)?;
    u32::try_from(scalar(
        records
            .raw_value("mmkv_key_auto_login_flag")
            .ok_or(ProfileError::InvalidMetadata)?,
    )?)
    .map_err(|_| ProfileError::InvalidMetadata)
}
/// Accepted-login-only variant. The caller must have installed the matching
/// authenticated generation and cleared its own logout fence first. Generic
/// staging retains the logout flag; only this explicit path may clear it.
pub fn stage_accepted_saved_login_projection(
    data: &[u8],
    metadata: &[u8],
    format_key: &[u8],
    username: &str,
    input: &SavedLoginProjection<'_>,
) -> Result<StagedMmkv> {
    let identity = read_mmkv_identity(data, metadata, format_key)?;
    if identity.username != username {
        return Err(ProfileError::AccountMismatch);
    }
    let mut values = saved_login_values(username, input)?;
    values.push(("mmkv_key_logout_locked_flag".into(), Some(vec![0])));
    if input.cookie.is_empty() {
        values.push((
            "mmkv_key_server_id".into(),
            Some(mmkv_string_value(&[])?.to_vec()),
        ));
    }
    let staged = stage_mmkv_values(data, metadata, format_key, &values);
    values.zeroize();
    staged
}
