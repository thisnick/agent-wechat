//! Read-only identity fields from the inspected MMKV v4 layout. The format key
//! is caller-provided; no artifact-derived key or account material is shipped.
use crate::{ProfileError, Result};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub struct ProfileIdentity {
    pub username: String,
    pub uin: u32,
    pub device_id: Vec<u8>,
}
impl ProfileIdentity {
    /// The complete persisted installation ID remains in MMKV. The inspected
    /// native BaseRequest transmits only its first sixteen bytes.
    pub fn request_device_id(&self) -> Result<&[u8]> {
        if !(16..=256).contains(&self.device_id.len()) {
            return Err(ProfileError::InvalidMetadata);
        }
        Ok(&self.device_id[..16])
    }
}
fn var(input: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0;
    for i in 0..10 {
        let byte = *input.get(*at).ok_or(ProfileError::InvalidMetadata)?;
        *at += 1;
        if i == 9 && byte > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        value |= ((byte & 127) as u64) << (i * 7);
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(ProfileError::InvalidMetadata)
}
fn span<'a>(input: &'a [u8], at: &mut usize) -> Result<&'a [u8]> {
    let n = usize::try_from(var(input, at)?).map_err(|_| ProfileError::ResourceLimit)?;
    let end = at.checked_add(n).ok_or(ProfileError::ResourceLimit)?;
    let value = input.get(*at..end).ok_or(ProfileError::InvalidMetadata)?;
    *at = end;
    Ok(value)
}
fn string(input: &[u8]) -> Result<Vec<u8>> {
    let mut at = 0;
    let value = span(input, &mut at)?.to_vec();
    if at != input.len() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(value)
}
/// CRC32 checks the captured ciphertext span; it is not authentication. Identity
/// values must also match the independently authenticated native account.
pub fn read_mmkv_identity(
    data: &[u8],
    metadata: &[u8],
    format_key: &[u8],
) -> Result<ProfileIdentity> {
    if format_key.len() != 16
        || data.len() < 4
        || data.len() > 4 * 1024 * 1024 + 4
        || metadata.len() < 40
        || metadata.len() > 4096
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let number = |offset| {
        u32::from_le_bytes(
            metadata[offset..offset + 4]
                .try_into()
                .expect("metadata bound"),
        )
    };
    if number(4) != 4 || metadata.len() >= 108 && number(104) != 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let actual = number(28) as usize;
    let ciphertext = data
        .get(4..4 + actual)
        .ok_or(ProfileError::InvalidMetadata)?;
    if crc32fast::hash(ciphertext) != number(0) {
        return Err(ProfileError::InvalidMetadata);
    }
    let plain = Zeroizing::new(
        openssl::symm::decrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            format_key,
            Some(&metadata[12..28]),
            ciphertext,
        )
        .map_err(|_| ProfileError::InvalidMetadata)?,
    );
    let mut at = 0;
    var(&plain, &mut at)?;
    let mut entries = BTreeMap::new();
    let mut count = 0;
    while at < plain.len() {
        let key = std::str::from_utf8(span(&plain, &mut at)?)
            .map_err(|_| ProfileError::InvalidMetadata)?;
        if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
            return Err(ProfileError::InvalidMetadata);
        }
        let value = span(&plain, &mut at)?;
        if value.is_empty() {
            entries.remove(key);
        } else {
            entries.insert(key, value);
        }
        count += 1;
        if count > 65536 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    let get = |name: &str| {
        entries
            .get(name)
            .copied()
            .ok_or(ProfileError::InvalidMetadata)
    };
    let username = String::from_utf8(string(get("mmkv_key_user_name")?)?)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let mut at = 0;
    let raw = get("mmkv_key_latest_login_uin")?;
    let uin = u32::try_from(var(raw, &mut at)?).map_err(|_| ProfileError::InvalidMetadata)?;
    if at != raw.len()
        || uin == 0
        || username.is_empty()
        || username.len() > 256
        || username.chars().any(char::is_control)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let device_id = string(get("key_wechat_device_id")?)?;
    if device_id.len() < 16 || device_id.len() > 256 {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(ProfileIdentity {
        username,
        uin,
        device_id,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn append(value: u64, out: &mut Vec<u8>) {
        let mut value = value;
        while value >= 128 {
            out.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn framed(value: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        append(value.len() as u64, &mut out);
        out.extend(value);
        out
    }
    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let mut plain = vec![0];
        for (key, value) in [
            ("mmkv_key_user_name", framed(b"synthetic_self")),
            ("mmkv_key_latest_login_uin", vec![7]),
            ("key_wechat_device_id", framed(b"synthetic-device-16")),
        ] {
            plain.extend(framed(key.as_bytes()));
            plain.extend(framed(&value));
        }
        let cipher = openssl::symm::encrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            &[9; 16],
            Some(&[3; 16]),
            &plain,
        )
        .unwrap();
        let mut metadata = vec![0; 112];
        metadata[..4].copy_from_slice(&crc32fast::hash(&cipher).to_le_bytes());
        metadata[4..8].copy_from_slice(&4u32.to_le_bytes());
        metadata[12..28].copy_from_slice(&[3; 16]);
        metadata[28..32].copy_from_slice(&(cipher.len() as u32).to_le_bytes());
        let mut data = vec![0; 4];
        data.extend(cipher);
        (data, metadata)
    }
    #[test]
    fn identity_requires_consistent_version_crc_key_and_complete_scalar() {
        let (mut data, mut metadata) = fixture();
        let id = read_mmkv_identity(&data, &metadata, &[9; 16]).unwrap();
        assert_eq!(id.username, "synthetic_self");
        assert_eq!(id.uin, 7);
        assert!(read_mmkv_identity(&data, &metadata, &[8; 16]).is_err());
        data[4] ^= 1;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
        data[4] ^= 1;
        metadata[104] = 1;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
        metadata[104] = 0;
        metadata[4] = 5;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
    }
}

/// Decoded v4 records are secrets; Debug and serde intentionally unavailable.
pub struct MmkvRecords {
    entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
    plain: Zeroizing<Vec<u8>>,
}
impl MmkvRecords {
    pub fn raw_value(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(|v| v.as_slice())
    }
    pub fn string_value(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        self.raw_value(name)
            .map(string)
            .transpose()
            .map(|v| v.map(Zeroizing::new))
    }
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
fn append_var(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8)
}
fn append_span(value: &[u8], out: &mut Vec<u8>) {
    append_var(value.len() as u64, out);
    out.extend(value)
}
/// Native string/Data encoding used inside each MMKV value record.
pub fn mmkv_string_value(value: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if value.len() > 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut output = Zeroizing::new(Vec::new());
    append_span(value, &mut output);
    Ok(output)
}
pub fn decode_mmkv_records(data: &[u8], metadata: &[u8], key: &[u8]) -> Result<MmkvRecords> {
    if key.len() != 16
        || data.len() < 4
        || data.len() > 4 * 1024 * 1024 + 4
        || metadata.len() < 112
        || metadata.len() > 4096
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let n = |o| u32::from_le_bytes(metadata[o..o + 4].try_into().expect("bounded"));
    if n(4) != 4 || metadata[104..112].iter().any(|b| *b != 0) {
        return Err(ProfileError::InvalidMetadata);
    }
    let length = n(28) as usize;
    let cipher = data
        .get(4..4 + length)
        .ok_or(ProfileError::InvalidMetadata)?;
    if crc32fast::hash(cipher) != n(0) {
        return Err(ProfileError::InvalidMetadata);
    }
    let plain = Zeroizing::new(
        openssl::symm::decrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            key,
            Some(&metadata[12..28]),
            cipher,
        )
        .map_err(|_| ProfileError::InvalidMetadata)?,
    );
    let mut at = 0;
    var(&plain, &mut at)?;
    let mut entries = BTreeMap::new();
    let mut records = 0;
    while at < plain.len() {
        let name = std::str::from_utf8(span(&plain, &mut at)?)
            .map_err(|_| ProfileError::InvalidMetadata)?;
        if name.is_empty() || name.len() > 4096 || name.chars().any(char::is_control) {
            return Err(ProfileError::InvalidMetadata);
        }
        let value = span(&plain, &mut at)?;
        if value.is_empty() {
            entries.remove(name);
        } else {
            entries.insert(name.to_owned(), Zeroizing::new(value.to_vec()));
        }
        records += 1;
        if records > 65536 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    Ok(MmkvRecords { entries, plain })
}
/// Complete staged pair. Publication across these files requires caller recovery;
/// this object alone never modifies any filesystem or native sync checkpoint.
pub struct StagedMmkv {
    data: Zeroizing<Vec<u8>>,
    metadata: Vec<u8>,
    pub changed_keys: Vec<String>,
    expected_data_sha256: Option<[u8; 32]>,
    expected_meta_sha256: Option<[u8; 32]>,
}
impl StagedMmkv {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn metadata(&self) -> &[u8] {
        &self.metadata
    }
}
/// Append while preserving IV/sequence/flags and the complete old ciphertext
/// prefix. A full data file grows to the next 4096-byte page, at most 4 MiB;
/// records are never compacted or discarded. Identical values are byte-idempotent.
/// Values are already MMKV-encoded; None is a tombstone. Unknown keys survive.
pub fn stage_mmkv_values(
    data: &[u8],
    metadata: &[u8],
    key: &[u8],
    updates: &[(String, Option<Vec<u8>>)],
) -> Result<StagedMmkv> {
    let records = decode_mmkv_records(data, metadata, key)?;
    let mut plain = records.plain.clone();
    let mut changed = vec![];
    let mut names = std::collections::BTreeSet::new();
    for (name, value) in updates {
        if name.is_empty()
            || name.len() > 4096
            || name.chars().any(char::is_control)
            || !names.insert(name)
        {
            return Err(ProfileError::InvalidMetadata);
        }
        if value
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 1024 * 1024)
        {
            return Err(ProfileError::InvalidMetadata);
        }
        if records.raw_value(name) == value.as_deref() {
            continue;
        }
        append_span(name.as_bytes(), &mut plain);
        append_span(value.as_deref().unwrap_or_default(), &mut plain);
        if plain.len() + 4 > data.len().max(4 * 1024 * 1024) {
            return Err(ProfileError::ResourceLimit);
        }
        changed.push(name.clone());
    }
    let capacity = if plain.len() > data.len() - 4 {
        let rounded = (plain.len() + 4).div_ceil(4096) * 4096;
        if rounded > 4 * 1024 * 1024 {
            return Err(ProfileError::ResourceLimit);
        }
        rounded
    } else {
        data.len()
    };
    let cipher = Zeroizing::new(
        openssl::symm::encrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            key,
            Some(&metadata[12..28]),
            &plain,
        )
        .map_err(|_| ProfileError::InvalidMetadata)?,
    );
    let old = u32::from_le_bytes(metadata[28..32].try_into().expect("bounded")) as usize;
    if cipher.get(..old) != data.get(4..4 + old) {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut output = Zeroizing::new(data.to_vec());
    output.resize(capacity, 0);
    output[4..4 + cipher.len()].copy_from_slice(&cipher);
    let mut meta = metadata.to_vec();
    meta[..4].copy_from_slice(&crc32fast::hash(&cipher).to_le_bytes());
    meta[28..32].copy_from_slice(&(cipher.len() as u32).to_le_bytes());
    let checked = decode_mmkv_records(&output, &meta, key)?;
    for (name, value) in updates {
        if checked.raw_value(name) != value.as_deref() {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    Ok(StagedMmkv {
        data: output,
        metadata: meta,
        changed_keys: changed,
        expected_data_sha256: Some(mmkv_hash(data)),
        expected_meta_sha256: Some(mmkv_hash(metadata)),
    })
}
/// New nonexpiring v4 pair with explicit caller-generated key and IV. Fixed
/// capacity is the total page-aligned data-file size, including its 4-byte
/// reserved header. The metadata occupies one 4096-byte page, matching the
/// inspected native files. Maximum data capacity is 4 MiB.
pub fn create_mmkv_v4(key: &[u8], iv: [u8; 16], capacity: usize) -> Result<StagedMmkv> {
    if key.len() != 16
        || !(4096..=4 * 1024 * 1024).contains(&capacity)
        || !capacity.is_multiple_of(4096)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let cipher = openssl::symm::encrypt(
        openssl::symm::Cipher::aes_128_cfb128(),
        key,
        Some(&iv),
        &[0],
    )
    .map_err(|_| ProfileError::InvalidMetadata)?;
    let mut data = Zeroizing::new(vec![0; capacity]);
    data[4] = cipher[0];
    let mut meta = vec![0; 4096];
    meta[..4].copy_from_slice(&crc32fast::hash(&cipher).to_le_bytes());
    meta[4..8].copy_from_slice(&4u32.to_le_bytes());
    meta[12..28].copy_from_slice(&iv);
    meta[28..32].copy_from_slice(&1u32.to_le_bytes());
    Ok(StagedMmkv {
        data,
        metadata: meta,
        changed_keys: vec![],
        expected_data_sha256: None,
        expected_meta_sha256: None,
    })
}
/// Stage the proved native SyncKey string field in an explicitly selected account
/// store. This does not guess the path, write the legacy file, or advance a cursor.
pub fn stage_mmkv_sync_key(
    data: &[u8],
    metadata: &[u8],
    key: &[u8],
    sync_key: &[u8],
) -> Result<StagedMmkv> {
    if sync_key.is_empty() || sync_key.len() > 1024 * 1024 {
        return Err(ProfileError::InvalidMetadata);
    }
    let value = mmkv_string_value(sync_key)?;
    stage_mmkv_values(
        data,
        metadata,
        key,
        &[("mmkv_key_sync_key".into(), Some(value.to_vec()))],
    )
}

fn mmkv_hash(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MmkvPublication {
    pub data_replaced: bool,
    pub metadata_replaced: bool,
    pub already_applied: bool,
}
fn safe_pair_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
    let canonical = std::fs::canonicalize(parent)?;
    if canonical != parent {
        return Err(ProfileError::UnsupportedFile);
    }
    let name = path.file_name().ok_or(ProfileError::InvalidQuery)?;
    let target = canonical.join(name);
    for p in [
        &target,
        &target.with_file_name(format!("{}.crc", name.to_string_lossy())),
    ] {
        let m = std::fs::symlink_metadata(p)?;
        if !m.is_file() || wechat_platform::is_link(&m) {
            return Err(ProfileError::UnsupportedFile);
        }
    }
    Ok(target)
}
fn replace_mmkv_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|e| ProfileError::Io(e.error))?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
/// Publish a previously staged append while the caller holds exclusive profile
/// ownership and its durable intent. Data is replaced first: the old metadata
/// still validates the unchanged ciphertext prefix. Metadata publication exposes
/// the new span. Replay accepts the intermediate new-data/old-metadata state.
/// This is not atomic with database, credential or sidecar cursor transactions.
pub fn publish_mmkv_append(
    path: impl AsRef<std::path::Path>,
    staged: &StagedMmkv,
    options: &crate::WriteOptions,
) -> Result<MmkvPublication> {
    publish_mmkv_append_guarded(path, staged, options, || true)
}
/// Recheck an external authorization fence before each independent data/metadata
/// commit. If the fence closes between commits, the caller retains its sealed
/// intent and the ordinary append replay rules accept the data-first state.
pub fn publish_mmkv_append_guarded(
    path: impl AsRef<std::path::Path>,
    staged: &StagedMmkv,
    options: &crate::WriteOptions,
    mut allowed: impl FnMut() -> bool,
) -> Result<MmkvPublication> {
    if !options.exclusive_ownership_confirmed {
        return Err(ProfileError::SnapshotRequired);
    }
    if options.durable_intent_id.is_empty() || options.durable_intent_id.len() > 256 {
        return Err(ProfileError::InvalidQuery);
    }
    let old_data = staged
        .expected_data_sha256
        .ok_or(ProfileError::InvalidMetadata)?;
    let old_meta = staged
        .expected_meta_sha256
        .ok_or(ProfileError::InvalidMetadata)?;
    let path = safe_pair_path(path.as_ref())?;
    let meta = path.with_file_name(format!(
        "{}.crc",
        path.file_name()
            .ok_or(ProfileError::InvalidQuery)?
            .to_string_lossy()
    ));
    let current_data = mmkv_hash(&std::fs::read(&path)?);
    let current_meta = mmkv_hash(&std::fs::read(&meta)?);
    let new_data = mmkv_hash(staged.data());
    let new_meta = mmkv_hash(staged.metadata());
    if current_data == new_data && current_meta == new_meta {
        return Ok(MmkvPublication {
            data_replaced: false,
            metadata_replaced: false,
            already_applied: true,
        });
    }
    if current_meta != old_meta || (current_data != old_data && current_data != new_data) {
        return Err(ProfileError::SourceChanged);
    }
    let replace_data = current_data != new_data;
    if replace_data {
        if !allowed() {
            return Err(ProfileError::SourceChanged);
        }
        replace_mmkv_file(&path, staged.data())?;
    }
    if mmkv_hash(&std::fs::read(&meta)?) != old_meta
        || mmkv_hash(&std::fs::read(&path)?) != new_data
    {
        return Err(ProfileError::SourceChanged);
    }
    if !allowed() {
        return Err(ProfileError::SourceChanged);
    }
    replace_mmkv_file(&meta, staged.metadata())?;
    Ok(MmkvPublication {
        data_replaced: replace_data,
        metadata_replaced: true,
        already_applied: false,
    })
}

/// The inspected per-account v2 store derives its CFB key from the account name,
/// independently of global format keys, database passwords and login credentials.
/// This is a format derivation, not a means of authenticating an account.
pub fn account_mmkv_key(username: &str) -> Result<Zeroizing<Vec<u8>>> {
    use md5::{Digest, Md5};
    crate::projection::identity(username)?;
    Ok(Zeroizing::new(Md5::digest(username.as_bytes()).to_vec()))
}
/// Select the observed account login store only after exact username/UIN binding.
pub fn account_sync_store_path(
    location: &crate::ProfileLocation,
    identity: &ProfileIdentity,
) -> Result<std::path::PathBuf> {
    if location.account_directory()
        != crate::official_account_directory(&identity.username, identity.uin)?
    {
        return Err(ProfileError::AccountMismatch);
    }
    Ok(location.root().join("config/login_configv2"))
}
/// Read the desktop cursor from a caller-confirmed stopped/coherent account.
/// The binary cursor is independent of saved-login and business-session keys.
pub fn read_account_sync_key(
    location: &crate::ProfileLocation,
    identity: &ProfileIdentity,
    options: &crate::ReadOptions,
) -> Result<Zeroizing<Vec<u8>>> {
    if !options.stopped_or_consistent_snapshot {
        return Err(ProfileError::SnapshotRequired);
    }
    account_sync_store_path(location, identity)?;
    let data = crate::media::read_cache(
        location.root(),
        std::path::Path::new("config/login_configv2"),
        4 * 1024 * 1024,
    )?;
    let metadata = crate::media::read_cache(
        location.root(),
        std::path::Path::new("config/login_configv2.crc"),
        4096,
    )?;
    let key = account_mmkv_key(&identity.username)?;
    let records = decode_mmkv_records(&data, &metadata, &key)?;
    Ok(records
        .string_value("mmkv_key_sync_key")?
        .unwrap_or_default())
}
/// Stage the native account store's cursor. A coordinator must first durably apply
/// every relevant command and journal this sealed generation before publication.
pub fn stage_account_sync_key(
    data: &[u8],
    metadata: &[u8],
    username: &str,
    cursor: &[u8],
) -> Result<StagedMmkv> {
    let key = account_mmkv_key(username)?;
    stage_mmkv_sync_key(data, metadata, &key, cursor)
}

/// Publish a journaled newly created pair without replacing any existing bytes.
/// Replay accepts only exact staged files, including a data-only crash state.
/// The caller must authorize the missing store (e.g. its managed bootstrap receipt)
/// and create/own the canonical parent directory before calling this function.
pub fn publish_mmkv_create(
    path: impl AsRef<std::path::Path>,
    staged: &StagedMmkv,
    options: &crate::WriteOptions,
) -> Result<MmkvPublication> {
    use std::{fs, io::Write, path::Path};
    if !options.exclusive_ownership_confirmed {
        return Err(ProfileError::SnapshotRequired);
    }
    if options.durable_intent_id.is_empty() || options.durable_intent_id.len() > 256 {
        return Err(ProfileError::InvalidQuery);
    }
    if staged.expected_data_sha256.is_some() || staged.expected_meta_sha256.is_some() {
        return Err(ProfileError::InvalidMetadata);
    }
    let path = path.as_ref();
    let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
    if fs::canonicalize(parent)? != parent {
        return Err(ProfileError::UnsupportedFile);
    }
    let meta = path.with_file_name(format!(
        "{}.crc",
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or(ProfileError::InvalidQuery)?
    ));
    fn exists_exact(path: &Path, bytes: &[u8]) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(m) => {
                if !m.is_file() || wechat_platform::is_link(&m) {
                    return Err(ProfileError::UnsupportedFile);
                }
                if m.len() != bytes.len() as u64 {
                    return Err(ProfileError::SourceChanged);
                }
                let old = fs::read(path)?;
                if old != bytes {
                    return Err(ProfileError::SourceChanged);
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    let data_exists = exists_exact(path, staged.data())?;
    let meta_exists = exists_exact(&meta, staged.metadata())?;
    fn create(path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)
            .map_err(|e| ProfileError::Io(e.error))?;
        wechat_platform::sync_dir(parent)?;
        Ok(())
    }
    if !data_exists {
        create(path, staged.data())?;
    }
    if !meta_exists {
        create(&meta, staged.metadata())?;
    }
    Ok(MmkvPublication {
        data_replaced: !data_exists,
        metadata_replaced: !meta_exists,
        already_applied: data_exists && meta_exists,
    })
}
