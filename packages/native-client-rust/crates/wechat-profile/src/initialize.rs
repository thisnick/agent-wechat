//! Fresh creation of the inspected ordinary SQLCipher schemas. No source profile
//! or vendor process is used. Custom FTS and saved-login initialization are not
//! inferred from ordinary database creation.
use crate::{
    keys::{derive_key, verify_first_page},
    DatabaseKey, KeyFile, ProfileError, ProfileLocation, Result,
};
use rusqlite::Connection;
use serde::Serialize;
use std::{fs, io::Read, path::Path};

pub(crate) const LAYOUTS: &[(&str, &str)] = &[
    ("contact/contact.db", include_str!("layout/contact.db.sql")),
    ("session/session.db", include_str!("layout/session.db.sql")),
    (
        "message/message_0.db",
        include_str!("layout/message_0.db.sql"),
    ),
    ("message/media_0.db", include_str!("layout/media_0.db.sql")),
    (
        "message/message_resource.db",
        include_str!("layout/message_resource.db.sql"),
    ),
    (
        "hardlink/hardlink.db",
        include_str!("layout/hardlink.db.sql"),
    ),
    (
        "head_image/head_image.db",
        include_str!("layout/head_image.db.sql"),
    ),
];
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializedLayout {
    pub databases: Vec<String>,
    pub encrypted: bool,
    pub official_startup_verified: bool,
    pub unsupported: Vec<String>,
}
/// Creates a NEW account root atomically. The caller supplies an existing/new
/// 32-byte binary database password explicitly and stores returned keys separately.
/// For an official home this is the accepted account AuthSect field 9 payload;
/// generating another password cannot substitute for that server-issued value.
/// This initializes ordinary native schemas; it does not claim a fully trusted
/// login profile or manufacture custom-tokenizer indexes.
pub fn initialize_profile(
    path: impl AsRef<Path>,
    password: &[u8],
) -> Result<(ProfileLocation, KeyFile, InitializedLayout)> {
    if password.len() != 32 {
        return Err(ProfileError::InvalidKeysFile);
    }
    let target = path.as_ref();
    let parent = target.parent().ok_or(ProfileError::InvalidProfile)?;
    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(ProfileError::InvalidProfile)?;
    if name.is_empty() || name.len() > 512 || name.contains(['/', '\\', '\0']) {
        return Err(ProfileError::InvalidProfile);
    }
    if target.exists() && (!target.is_dir() || fs::read_dir(target)?.next().is_some()) {
        return Err(ProfileError::InvalidProfile);
    }
    let parent = fs::canonicalize(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".profile-create-")
        .tempdir_in(&parent)?
        .keep();
    let root = staging.as_path();
    let mut keys = KeyFile::empty(name);
    let mut databases = vec![];
    for (relative, sql) in LAYOUTS {
        let file = root.join("db_storage").join(relative);
        fs::create_dir_all(file.parent().ok_or(ProfileError::InvalidProfile)?)?;
        let c = Connection::open(&file)?;
        let available = c
            .query_row("PRAGMA cipher_version", [], |r| r.get::<_, String>(0))
            .ok()
            .is_some_and(|v| !v.is_empty());
        if !available {
            return Err(ProfileError::SqlcipherUnavailable);
        }
        // sqlite3_key accepts binary passphrases, avoiding SQL interpolation and any
        // accidental raw-key interpretation of an x'...' SQL literal.
        let rc = unsafe {
            rusqlite::ffi::sqlite3_key(c.handle(), password.as_ptr().cast(), password.len() as i32)
        };
        if rc != rusqlite::ffi::SQLITE_OK {
            return Err(ProfileError::Database);
        }
        c.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;PRAGMA synchronous=FULL;PRAGMA journal_mode=DELETE;")?;
        c.execute_batch(sql)?;
        if relative.ends_with("hardlink.db") {
            // The schema uses the observed v4 hardlink tables. Mark that version
            // explicitly; stock-scan completion and native UUID remain unset.
            c.execute("INSERT INTO db_info(Key,ValueInt64,ValueDouble,ValueStdStr,ValueBlob)VALUES('table_version',4,0,'',x'')",[])?;
        }
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .and_then(|s| {
                if s == "ok" {
                    Ok(())
                } else {
                    Err(rusqlite::Error::InvalidQuery)
                }
            })?;
        drop(c);
        let mut page = [0u8; 4096];
        fs::File::open(&file)?.read_exact(&mut page)?;
        let mut salt = [0; 16];
        salt.copy_from_slice(&page[..16]);
        let raw = derive_key(password, &salt);
        if !verify_first_page(&page, raw.expose(), &salt) {
            return Err(ProfileError::KeyMismatch((*relative).into()));
        }
        let array: [u8; 32] = raw
            .expose()
            .try_into()
            .map_err(|_| ProfileError::InvalidKeysFile)?;
        keys.databases.insert(
            (*relative).into(),
            DatabaseKey::new((*relative).into(), salt, array)?,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        }
        wechat_platform::sync_file(&file)?;
        databases.push((*relative).into());
    }
    // Refuse a destination created while staging. persist_noclobber for directory
    // publication is provided by renameat2(RENAME_NOREPLACE) on Linux.
    let destination = parent.join(name);
    publish_staging(staging, &destination)?;
    wechat_platform::sync_dir(&parent)?;
    let location = ProfileLocation::new(&destination)?;
    Ok((
        location,
        keys,
        InitializedLayout {
            databases,
            encrypted: true,
            official_startup_verified: false,
            unsupported: vec![
                "custom FTS tokenizer/index initialization".into(),
                "trusted login/MMKV/device bootstrap".into(),
                "ordinary subsystem schemas not in this adapter".into(),
            ],
        },
    ))
}

fn publish_staging(source: std::path::PathBuf, target: &Path) -> Result<()> {
    // Preserve a failed publication for diagnosis. In particular never let a
    // TempDir destructor delete an unexpected displaced directory after a race.
    sync_created_directories(&source, 0)?;
    publish_directory(&source, target)?;
    if source.exists() {
        fs::remove_dir(&source)?;
    }
    Ok(())
}
fn sync_created_directories(path: &Path, depth: usize) -> Result<()> {
    if depth > 16 {
        return Err(ProfileError::ResourceLimit);
    }
    for e in fs::read_dir(path)? {
        let e = e?;
        let m = fs::symlink_metadata(e.path())?;
        if wechat_platform::is_link(&m) {
            return Err(ProfileError::UnsupportedFile);
        } else if m.is_dir() {
            sync_created_directories(&e.path(), depth + 1)?;
        }
    }
    wechat_platform::sync_dir(path)?;
    Ok(())
}
fn publish_directory(source: &Path, target: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let exists = target.exists();
        if exists {
            let m = fs::symlink_metadata(target)?;
            if !m.is_dir() || wechat_platform::is_link(&m) || fs::read_dir(target)?.next().is_some()
            {
                return Err(ProfileError::InvalidProfile);
            }
        }
        let a = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| ProfileError::InvalidProfile)?;
        let b = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| ProfileError::InvalidProfile)?;
        let flags = if exists {
            libc::RENAME_EXCHANGE
        } else {
            libc::RENAME_NOREPLACE
        };
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                flags,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if exists && fs::read_dir(source)?.next().is_some() {
            // Never remove unexpected data introduced by an unmanaged writer. Restore
            // the source tree before returning; the caller's exclusive lease is required.
            let rc = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            };
            if rc != 0 {
                return Err(ProfileError::SourceChanged);
            }
            return Err(ProfileError::SourceChanged);
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        wechat_platform::publish_directory(source, target)?;
        Ok(())
    }
}
/// Observed directory naming in the inspected Linux profile. The suffix is the
/// first four lowercase MD5 hex digits of decimal UIN, not a username hash. The
/// complete fresh-home takeover still needs a same-build compatibility check.
pub fn official_account_directory(username: &str, uin: u32) -> Result<String> {
    use md5::{Digest, Md5};
    if uin == 0
        || username.is_empty()
        || matches!(username, "." | "..")
        || username.len() > 256
        || !username
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.@-".contains(&c))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let suffix = format!("{:x}", Md5::digest(uin.to_string().as_bytes()));
    Ok(format!("{username}_{}", &suffix[..4]))
}
/// Credentials and additional raw MMKV values are caller supplied; this type has
/// no Debug/Serialize so a bootstrap request cannot enter ordinary logs.
pub struct OfficialHomeBootstrap {
    pub username: String,
    pub uin: u32,
    pub device_id: Vec<u8>,
    /// Additional already encoded values, e.g. verified saved-login records. Unknown
    /// defaults are not fabricated. Identity keys are reserved and cannot override.
    pub global_values: Vec<(String, Option<Vec<u8>>)>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialHomeReport {
    pub account_directory: String,
    pub identity_store_relative_path: String,
    pub layout: InitializedLayout,
    pub saved_login_initialization_complete: bool,
    pub database_password_hydration_verified: bool,
}
/// Create a fresh full official HOME layout, publishing only to a nonexistent or
/// empty managed target. Never overwrites a populated or external profile.
/// Returned account location is suitable for existing readers/projections.
fn initialize_official_home_impl(
    home: impl AsRef<Path>,
    input: &OfficialHomeBootstrap,
    password: &[u8],
    format_key: &[u8],
    saved_login: Option<&crate::SavedLoginProjection<'_>>,
) -> Result<(ProfileLocation, KeyFile, OfficialHomeReport)> {
    use crate::{create_mmkv_v4, mmkv_string_value, stage_mmkv_values};
    if format_key.len() != 16 || input.device_id.len() < 16 || input.device_id.len() > 256 {
        return Err(ProfileError::InvalidMetadata);
    }
    let account = official_account_directory(&input.username, input.uin)?;
    let home = home.as_ref();
    let parent = fs::canonicalize(home.parent().ok_or(ProfileError::InvalidProfile)?)?;
    let name = home.file_name().ok_or(ProfileError::InvalidProfile)?;
    if home.exists() && (!home.is_dir() || fs::read_dir(home)?.next().is_some()) {
        return Err(ProfileError::InvalidProfile);
    }
    let stage = tempfile::Builder::new()
        .prefix(".official-home-create-")
        .tempdir_in(&parent)?
        .keep();
    let files = stage.join("xwechat_files");
    fs::create_dir(&files)?;
    let account_path = files.join(&account);
    let (_, mut keys, layout) = initialize_profile(&account_path, password)?;
    keys.image_dat = Some(crate::ImageDatKey::derive(input.uin, &input.username)?);
    let global = files.join("all_users/config");
    fs::create_dir_all(&global)?;
    let mut iv = [0; 16];
    openssl::rand::rand_bytes(&mut iv).map_err(|_| ProfileError::InvalidMetadata)?;
    let seed = create_mmkv_v4(format_key, iv, 65536)?;
    let mut scalar = vec![];
    let mut n = u64::from(input.uin);
    while n >= 128 {
        scalar.push((n as u8 & 127) | 128);
        n >>= 7
    }
    scalar.push(n as u8);
    let mut values = vec![
        (
            "mmkv_key_user_name".into(),
            Some(mmkv_string_value(input.username.as_bytes())?.to_vec()),
        ),
        ("mmkv_key_latest_login_uin".into(), Some(scalar)),
        (
            "key_wechat_device_id".into(),
            Some(mmkv_string_value(&input.device_id)?.to_vec()),
        ),
    ];
    let reserved = [
        "mmkv_key_user_name",
        "mmkv_key_latest_login_uin",
        "key_wechat_device_id",
    ];
    if input
        .global_values
        .iter()
        .any(|(k, _)| reserved.contains(&k.as_str()))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    values.extend(input.global_values.clone());
    if let Some(login) = saved_login {
        values.extend(crate::login_storage::saved_login_values(
            &input.username,
            login,
        )?);
    }
    let populated = stage_mmkv_values(seed.data(), seed.metadata(), format_key, &values);
    use zeroize::Zeroize;
    values.zeroize();
    let populated = populated?;
    fn put(path: &Path, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let mut o = wechat_platform::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            o.mode(0o600);
        }
        let mut f = o.open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
    put(&global.join("global_config"), populated.data())?;
    put(&global.join("global_config.crc"), populated.metadata())?;
    // Account-local v2 storage uses MD5(username), not the global format key.
    // An empty new store has no inherited cursor; only verified apply commits add it.
    let account_config = account_path.join("config");
    fs::create_dir(&account_config)?;
    openssl::rand::rand_bytes(&mut iv).map_err(|_| ProfileError::InvalidMetadata)?;
    let account_key = crate::account_mmkv_key(&input.username)?;
    let account_seed = create_mmkv_v4(&account_key, iv, 65536)?;
    put(&account_config.join("login_configv2"), account_seed.data())?;
    put(
        &account_config.join("login_configv2.crc"),
        account_seed.metadata(),
    )?;
    let identity = crate::read_mmkv_identity(populated.data(), populated.metadata(), format_key)?;
    if identity.username != input.username
        || identity.uin != input.uin
        || identity.device_id != input.device_id
    {
        return Err(ProfileError::InvalidMetadata);
    }
    if let Some(login) = saved_login {
        // The inspected selector explicitly falls back to this raw legacy file
        // when key_info.db is absent. We do not manufacture its opaque key_md5.
        let legacy = stage.join(".xwechat/login").join(&input.username);
        fs::create_dir_all(&legacy)?;
        put(&legacy.join("key_info.dat"), login.key_info)?;
    }
    let destination = parent.join(name);
    publish_staging(stage, &destination)?;
    wechat_platform::sync_dir(&parent)?;
    let location = ProfileLocation::new(destination.join("xwechat_files").join(&account))?;
    Ok((
        location,
        keys,
        OfficialHomeReport {
            account_directory: account,
            identity_store_relative_path: "xwechat_files/all_users/config/global_config".into(),
            layout,
            saved_login_initialization_complete: false,
            database_password_hydration_verified: false,
        },
    ))
}

/// Minimal ordinary HOME and explicit identity, without saved credentials.
pub fn initialize_official_home(
    home: impl AsRef<Path>,
    input: &OfficialHomeBootstrap,
    password: &[u8],
    format_key: &[u8],
) -> Result<(ProfileLocation, KeyFile, OfficialHomeReport)> {
    initialize_official_home_impl(home, input, password, format_key, None)
}
/// Fresh official-format HOME with server-issued saved credentials. Known global
/// records and native legacy key-info fallback are initialized, without guessing
/// a key_md5 value. Same-build official startup acceptance remains a runtime check.
pub fn initialize_official_home_with_login(
    home: impl AsRef<Path>,
    input: &OfficialHomeBootstrap,
    password: &[u8],
    format_key: &[u8],
    saved_login: &crate::SavedLoginProjection<'_>,
) -> Result<(ProfileLocation, KeyFile, OfficialHomeReport)> {
    initialize_official_home_impl(home, input, password, format_key, Some(saved_login))
}

/// Repair only the omitted known v4 marker of this adapter's receipt-bound
/// managed bootstrap. The coordinator must establish that provenance first.
/// No schema migration, UUID generation, stock-scan completion or foreign
/// profile adoption is implied by this helper.
pub fn ensure_managed_layout_version(
    location: &ProfileLocation,
    keys: &KeyFile,
    options: &crate::WriteOptions,
) -> Result<bool> {
    use rusqlite::OptionalExtension;
    crate::projection::ownership(location, keys, options)?;
    let db = crate::writer::find_database(location, "hardlink.db")?;
    let mut c = crate::writer::open_write(location, &db, keys, options.allow_plaintext)?;
    for table in [
        "image_hardlink_info_v4",
        "file_hardlink_info_v4",
        "video_hardlink_info_v4",
        "dir2id",
    ] {
        crate::projection::check_known_table(&c, table)?;
    }
    crate::projection::check_known_table(&c, "db_info")?;
    let tx = c.transaction()?;
    let version = tx
        .query_row(
            "SELECT ValueInt64 FROM db_info WHERE Key='table_version'",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?;
    match version {
        Some(Some(4)) => return Ok(false),
        Some(_) => return Err(ProfileError::InvalidMetadata),
        None => {}
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM db_info", [], |r| r.get(0))?;
    if count != 0 {
        return Err(ProfileError::WritesUnsupported);
    }
    tx.execute("INSERT INTO db_info(Key,ValueInt64,ValueDouble,ValueStdStr,ValueBlob)VALUES('table_version',4,0,'',x'')",[])?;
    tx.commit()?;
    Ok(true)
}
