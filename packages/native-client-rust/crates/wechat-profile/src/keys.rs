use crate::{ProfileError, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Component, Path},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub(crate) fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct DatabaseKey {
    pub relative_path: String,
    pub salt: [u8; 16],
    pub(crate) raw_page_key: SecretBytes,
}
impl DatabaseKey {
    pub fn new(relative_path: String, salt: [u8; 16], raw_page_key: [u8; 32]) -> Result<Self> {
        if !valid_relative_path(&relative_path) {
            return Err(ProfileError::InvalidKeysFile);
        }
        Ok(Self {
            relative_path,
            salt,
            raw_page_key: SecretBytes::new(raw_page_key.to_vec()),
        })
    }
}

#[derive(Clone)]
pub struct ImageDatKey {
    pub(crate) aes: SecretBytes,
    pub(crate) xor: u8,
}
impl fmt::Debug for ImageDatKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImageDatKey([REDACTED])")
    }
}
impl ImageDatKey {
    pub fn new(aes: [u8; 16], xor: u8) -> Self {
        Self {
            aes: SecretBytes::new(aes.to_vec()),
            xor,
        }
    }
    /// Inspected Linux V2 DAT format key. This public identity derivation is
    /// separate from database passwords and authenticated network credentials.
    /// The AES key uses sixteen lowercase-hex ASCII bytes, not decoded MD5.
    pub fn derive(uin: u32, username: &str) -> Result<Self> {
        use md5::{Digest, Md5};
        crate::projection::identity(username)?;
        let text = format!("{uin}{username}");
        let hex = format!("{:x}", Md5::digest(text.as_bytes()));
        let mut aes = [0u8; 16];
        aes.copy_from_slice(&hex.as_bytes()[..16]);
        // Native code tries bit shifts0,1,2,3 (not byte shifts).
        let xor = (0..=3)
            .map(|s| (uin >> s) as u8)
            .find(|x| *x != 0)
            .unwrap_or(0);
        Ok(Self::new(aes, xor))
    }
    /// Convert the older authorized cache representation: first 16 ASCII bytes
    /// of the 32-character cache string, not hex-decoding that string.
    pub fn from_legacy_cache(value: &str, xor: u8) -> Result<Self> {
        if value.len() != 32 || !value.is_ascii() {
            return Err(ProfileError::InvalidKeysFile);
        }
        let mut aes = [0u8; 16];
        aes.copy_from_slice(&value.as_bytes()[..16]);
        Ok(Self::new(aes, xor))
    }
}

#[derive(Clone, Debug)]
pub struct KeyFile {
    pub account_binding: String,
    pub databases: BTreeMap<String, DatabaseKey>,
    pub image_dat: Option<ImageDatKey>,
}

#[derive(Serialize, Deserialize, Zeroize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedKeyFile {
    schema_version: u32,
    format: String,
    account_binding: String,
    #[zeroize(skip)]
    sqlcipher: CipherSettings,
    databases: Vec<EncodedDatabaseKey>,
    image_dat: Option<EncodedImageKey>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CipherSettings {
    compatibility: u32,
    page_size: u32,
    kdf_iterations: u32,
}
#[derive(Serialize, Deserialize, Zeroize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedDatabaseKey {
    relative_path: String,
    salt_hex: String,
    raw_page_key_base64: String,
}
#[derive(Serialize, Deserialize, Zeroize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncodedImageKey {
    format_version: u32,
    raw_aes128_key_base64: String,
    xor_byte: u8,
}

pub(crate) fn valid_relative_path(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && !p.contains('\0')
        && p.len() <= 4096
        && Path::new(p)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

impl KeyFile {
    pub fn empty(account_binding: impl Into<String>) -> Self {
        Self {
            account_binding: account_binding.into(),
            databases: BTreeMap::new(),
            image_dat: None,
        }
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let m = fs::symlink_metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProfileError::KeysMissing
            } else {
                e.into()
            }
        })?;
        if !m.is_file() || wechat_platform::is_link(&m) || m.len() > 4 * 1024 * 1024 {
            return Err(ProfileError::InsecureKeysFile);
        }
        if !wechat_platform::private_path(path)? {
            return Err(ProfileError::InsecureKeysFile);
        }
        let mut options = wechat_platform::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let opened = options.open(path)?;
        let m = opened.metadata()?;
        if !m.is_file() || !wechat_platform::private_file(&opened)? {
            return Err(ProfileError::InsecureKeysFile);
        }
        let mut input = Zeroizing::new(Vec::new());
        use std::io::Read;
        opened.take(4 * 1024 * 1024 + 1).read_to_end(&mut input)?;
        if input.len() > 4 * 1024 * 1024 {
            return Err(ProfileError::ResourceLimit);
        }
        Self::from_json(&input)
    }
    pub fn from_json(input: &[u8]) -> Result<Self> {
        if input.len() > 4 * 1024 * 1024 {
            return Err(ProfileError::ResourceLimit);
        }
        let mut e: EncodedKeyFile =
            serde_json::from_slice(input).map_err(|_| ProfileError::InvalidKeysFile)?;
        let result = (|| {
            if e.schema_version != 1
                || e.format != "official-linux"
                || e.account_binding.is_empty()
                || e.account_binding.len() > 512
                || e.account_binding.contains(['/', '\\', '\0'])
                || e.sqlcipher.compatibility != 4
                || e.sqlcipher.page_size != 4096
                || e.sqlcipher.kdf_iterations != 256000
                || e.databases.len() > 4096
            {
                return Err(ProfileError::InvalidKeysFile);
            }
            let mut result = Self::empty(e.account_binding.clone());
            for d in &e.databases {
                let salt: [u8; 16] = hex::decode(&d.salt_hex)
                    .map_err(|_| ProfileError::InvalidKeysFile)?
                    .try_into()
                    .map_err(|_| ProfileError::InvalidKeysFile)?;
                let raw = Zeroizing::new(
                    STANDARD
                        .decode(&d.raw_page_key_base64)
                        .map_err(|_| ProfileError::InvalidKeysFile)?,
                );
                if raw.len() != 32 || !valid_relative_path(&d.relative_path) {
                    return Err(ProfileError::InvalidKeysFile);
                }
                if result
                    .databases
                    .insert(
                        d.relative_path.clone(),
                        DatabaseKey {
                            relative_path: d.relative_path.clone(),
                            salt,
                            raw_page_key: SecretBytes::new(raw.to_vec()),
                        },
                    )
                    .is_some()
                {
                    return Err(ProfileError::InvalidKeysFile);
                }
            }
            if let Some(i) = &e.image_dat {
                let aes = Zeroizing::new(
                    STANDARD
                        .decode(&i.raw_aes128_key_base64)
                        .map_err(|_| ProfileError::InvalidKeysFile)?,
                );
                if i.format_version != 2 || aes.len() != 16 {
                    return Err(ProfileError::InvalidKeysFile);
                }
                result.image_dat = Some(ImageDatKey {
                    aes: SecretBytes::new(aes.to_vec()),
                    xor: i.xor_byte,
                });
            }
            Ok(result)
        })();
        e.zeroize();
        result
    }
    /// The caller must explicitly choose a protected output. This is the only
    /// serializer: secret material never implements public Serialize or Display.
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(ProfileError::InvalidQuery)?;
        let m = fs::metadata(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if m.mode() & 0o077 != 0 {
                return Err(ProfileError::InsecureKeysFile);
            }
        }
        let mut encoded = EncodedKeyFile {
            schema_version: 1,
            format: "official-linux".into(),
            account_binding: self.account_binding.clone(),
            sqlcipher: CipherSettings {
                compatibility: 4,
                page_size: 4096,
                kdf_iterations: 256000,
            },
            databases: self
                .databases
                .values()
                .map(|d| EncodedDatabaseKey {
                    relative_path: d.relative_path.clone(),
                    salt_hex: hex::encode(d.salt),
                    raw_page_key_base64: STANDARD.encode(d.raw_page_key.expose()),
                })
                .collect(),
            image_dat: self.image_dat.as_ref().map(|i| EncodedImageKey {
                format_version: 2,
                raw_aes128_key_base64: STANDARD.encode(i.aes.expose()),
                xor_byte: i.xor,
            }),
        };
        let mut output = Zeroizing::new(
            serde_json::to_vec_pretty(&encoded).map_err(|_| ProfileError::InvalidKeysFile)?,
        );
        encoded.zeroize();
        output.push(b'\n');
        let mut f = wechat_platform::private_tempfile_in(parent, ".wx-keys-")?;
        use std::io::Write;
        f.write_all(&output)?;
        f.as_file().sync_all()?;
        f.persist_noclobber(path)
            .map_err(|e| ProfileError::Io(e.error))?;
        wechat_platform::sync_dir(parent)?;
        Ok(())
    }
}

pub(crate) fn derive_key(password: &[u8], salt: &[u8; 16]) -> SecretBytes {
    let mut key = Zeroizing::new(vec![0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password, salt, 256000, &mut key);
    SecretBytes::new(key.to_vec())
}

pub(crate) fn verify_first_page(page: &[u8], key: &[u8], salt: &[u8; 16]) -> bool {
    use hmac::{Hmac, Mac};
    if page.len() < 4096 || key.len() != 32 || &page[..16] != salt {
        return false;
    }
    let hs = salt.map(|v| v ^ 0x3a);
    let mut hk = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(key, &hs, 2, &mut *hk);
    let Ok(mut mac) = Hmac::<sha2::Sha512>::new_from_slice(&*hk) else {
        return false;
    };
    mac.update(&page[16..4032]);
    mac.update(&1u32.to_le_bytes());
    mac.verify_slice(&page[4032..4096]).is_ok()
}

/// Confirm a server-issued account database password against every actual DB
/// header and the already authorized page-key map. Only 4096-byte headers are
/// read; SQLite never opens these files, and no key/password bytes are returned.
/// The caller must hold exclusive profile ownership or supply a stopped copy.
/// Missing coverage, a different salt generation or one mismatched key fails
/// closed. This does not rekey, repair, replace or switch the attached profile.
pub fn verify_profile_password(
    location: &crate::ProfileLocation,
    keys: &KeyFile,
    password: &[u8],
) -> Result<usize> {
    use std::io::Read;
    if keys.account_binding != location.account_directory() {
        return Err(ProfileError::AccountMismatch);
    }
    if password.len() != 32 {
        return Err(ProfileError::InvalidKeysFile);
    }
    let mut files = vec![];
    crate::snapshot::walk(&location.storage, &mut files, 0, 4096)?;
    if files.is_empty() {
        return Err(ProfileError::KeysMissing);
    }
    let mut verified = 0;
    for path in files {
        let relative = path
            .strip_prefix(&location.storage)
            .ok()
            .and_then(|p| p.to_str())
            .ok_or(ProfileError::UnsupportedFile)?
            .replace('\\', "/");
        let cached = keys
            .databases
            .get(&relative)
            .ok_or_else(|| ProfileError::KeyUnavailable(relative.clone()))?;
        let mut options = wechat_platform::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&path)?;
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(ProfileError::UnsupportedFile);
        }
        let mut page = [0u8; 4096];
        file.read_exact(&mut page)
            .map_err(|_| ProfileError::KeyMismatch(relative.clone()))?;
        let after = fs::metadata(&path)?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(ProfileError::SourceChanged);
        }
        let mut salt = [0; 16];
        salt.copy_from_slice(&page[..16]);
        if salt != cached.salt || !verify_first_page(&page, cached.raw_page_key.expose(), &salt) {
            return Err(ProfileError::KeyMismatch(relative));
        }
        let derived = derive_key(password, &salt);
        if !openssl::memcmp::eq(derived.expose(), cached.raw_page_key.expose())
            || !verify_first_page(&page, derived.expose(), &salt)
        {
            return Err(ProfileError::KeyMismatch(relative));
        }
        verified += 1;
    }
    Ok(verified)
}

pub fn derive_image_dat_key(uin: u32, username: &str) -> Result<ImageDatKey> {
    ImageDatKey::derive(uin, username)
}
