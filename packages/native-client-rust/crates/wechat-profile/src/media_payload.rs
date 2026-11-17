//! Large file and video bytes for cache writes and reads, handled in steps.
//!
//! A 512 MiB attachment is hashed, copied into the profile and checked again
//! without ever being held in memory. The same checks apply as for bytes: the
//! message XML size and MD5 before any write, and the copy's SHA-256 before it
//! is published under its final name.
use super::*;
use std::io::Write;

/// Read and copy step.
const STEP: usize = 256 * 1024;

/// Plaintext for a profile cache write.
#[derive(Clone, Copy, Debug)]
pub enum MediaPayload<'a> {
    /// Bytes held in memory.
    Bytes(&'a [u8]),
    /// A private file the caller owns and has already checked. `length` is the
    /// file's expected size; any other size is refused.
    File {
        /// The file to read.
        path: &'a Path,
        /// The file's expected size in bytes.
        length: u64,
    },
}
/// Digests of one payload, computed in a single pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PayloadDigests {
    pub length: u64,
    pub md5: String,
    pub sha256: String,
}
impl MediaPayload<'_> {
    /// Size of the payload in bytes.
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::File { length, .. } => *length,
        }
    }
    /// Whether the payload has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// A reader of exactly `len()` bytes.
    pub(crate) fn reader(&self) -> Result<Box<dyn Read + '_>> {
        Ok(match self {
            Self::Bytes(bytes) => Box::new(*bytes),
            Self::File { path, length } => {
                let file = fs::File::open(path)?;
                if file.metadata()?.len() != *length {
                    return Err(ProfileError::SourceChanged);
                }
                Box::new(file.take(*length))
            }
        })
    }
    pub(crate) fn digests(&self) -> Result<PayloadDigests> {
        let digests = digest_reader(&mut self.reader()?)?;
        if digests.length != self.len() {
            return Err(ProfileError::SourceChanged);
        }
        Ok(digests)
    }
    /// The whole payload in memory, for small representations such as a JPEG
    /// thumbnail. A larger payload is refused.
    pub(crate) fn small_bytes(&self, max: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
        if self.len() > max as u64 {
            return Err(ProfileError::ResourceLimit);
        }
        Ok(match self {
            Self::Bytes(bytes) => std::borrow::Cow::Borrowed(*bytes),
            Self::File { .. } => {
                let mut bytes = Vec::with_capacity(self.len() as usize);
                self.reader()?.read_to_end(&mut bytes)?;
                if bytes.len() as u64 != self.len() {
                    return Err(ProfileError::SourceChanged);
                }
                std::borrow::Cow::Owned(bytes)
            }
        })
    }
    /// The movie container type, reading only box headers from a file.
    pub(crate) fn video_container(&self) -> Result<&'static str> {
        match self {
            Self::Bytes(bytes) => wechat_media::inspect_video_container(bytes),
            Self::File { path, length } => {
                wechat_media::inspect_video_reader(&mut fs::File::open(path)?, *length)
            }
        }
        .map_err(|_| ProfileError::InvalidMetadata)
    }
    /// Copy into `output`, returning the SHA-256 of what was copied.
    pub(crate) fn copy_hashed(&self, output: &mut dyn Write) -> Result<String> {
        let mut reader = self.reader()?;
        let (mut sha256, mut length) = (Sha256::new(), 0u64);
        let mut buffer = vec![0u8; STEP];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            sha256.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
            length += read as u64;
        }
        if length != self.len() {
            return Err(ProfileError::SourceChanged);
        }
        Ok(hex::encode(sha256.finalize()))
    }
    /// Whether the open cache file holds exactly this payload.
    pub(crate) fn same_as(&self, file: &mut fs::File) -> Result<bool> {
        let mut reader = self.reader()?;
        let mut expected = vec![0u8; STEP];
        let mut actual = vec![0u8; STEP];
        loop {
            let read = read_full(&mut reader, &mut expected)?;
            if read == 0 {
                // The cached file must end here too.
                return Ok(file.read(&mut actual[..1])? == 0);
            }
            if read_full(file, &mut actual[..read])? != read || actual[..read] != expected[..read] {
                return Ok(false);
            }
        }
    }
}
/// Length, MD5 and SHA-256 of everything `input` returns, read in steps.
pub(crate) fn digest_reader(input: &mut dyn Read) -> Result<PayloadDigests> {
    let (mut md5, mut sha256, mut length) = (Md5::new(), Sha256::new(), 0u64);
    let mut buffer = vec![0u8; STEP];
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        md5.update(&buffer[..read]);
        sha256.update(&buffer[..read]);
        length += read as u64;
    }
    Ok(PayloadDigests {
        length,
        md5: format!("{:x}", md5.finalize()),
        sha256: hex::encode(sha256.finalize()),
    })
}
/// Fill `buffer` unless the input ends first; returns the bytes read.
fn read_full(input: &mut dyn Read, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(filled)
}

/// A file or video hardlink whose content was checked in steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedHardlink {
    /// Absolute path of the checked file.
    pub path: std::path::PathBuf,
    /// Size of the file in bytes.
    pub byte_length: u64,
    /// Lowercase hex MD5 of the file's content.
    pub md5: String,
    /// Lowercase hex SHA-256 of the file's content.
    pub sha256: String,
    /// The file's modification time when checked, used to notice a later change.
    pub modified: std::time::SystemTime,
}

impl ProfileReader {
    /// [`Self::cached_hardlink_file`] without reading the file into memory:
    /// every candidate row is checked by size and content MD5 in steps, and
    /// all candidates must hold the same bytes. Returns the first match.
    pub fn verified_hardlink_file(
        &self,
        kind: HardlinkKind,
        md5: &str,
    ) -> Result<VerifiedHardlink> {
        let directory = kind.directory()?;
        if !digest_hex(md5, 32) {
            return Err(ProfileError::InvalidMetadata);
        }
        let md5 = md5.to_ascii_lowercase();
        let db = self
            .snapshot
            .inventory
            .iter()
            .filter(|d| d.relative_path.rsplit('/').next() == Some("hardlink.db"))
            .collect::<Vec<_>>();
        if db.len() != 1 {
            return Err(ProfileError::NotCached);
        }
        let c = self.snapshot.open(&db[0].relative_path)?;
        let mut q = c.prepare(&format!("SELECT d.username,h.file_name,h.file_size,h.dir2 FROM {} h JOIN dir2id d ON d.rowid=h.dir1 WHERE lower(h.md5)=?1 LIMIT 201", kind.table()))?;
        let rows = q
            .query_map([&md5], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > 200 {
            return Err(ProfileError::ResourceLimit);
        }
        let mut found: Option<VerifiedHardlink> = None;
        for (month, name, size, dir2) in rows {
            if dir2 != 0
                || !valid_component(&month)
                || !valid_component(&name)
                || month.len() != 7
                || month.as_bytes()[4] != b'-'
            {
                return Err(ProfileError::InvalidMetadata);
            }
            let relative = Path::new(directory).join(month).join(name);
            let (mut file, before, path) = match open_cache(
                &self.snapshot.location.root,
                &relative,
                hardlink_limit(kind),
            ) {
                Ok(opened) => opened,
                Err(ProfileError::NotCached) => continue,
                Err(e) => return Err(e),
            };
            let length = before.len();
            let digests = digest_reader(&mut (&mut file).take(length + 1))?;
            cache_stable(&file, &before, &path)?;
            if size < 0 || length != size as u64 || digests.length != length || digests.md5 != md5 {
                return Err(ProfileError::InvalidMetadata);
            }
            if found
                .as_ref()
                .is_some_and(|old| old.sha256 != digests.sha256 || old.byte_length != length)
            {
                return Err(ProfileError::InvalidMetadata);
            }
            if found.is_none() {
                found = Some(VerifiedHardlink {
                    path,
                    byte_length: length,
                    md5: digests.md5,
                    sha256: digests.sha256,
                    modified: before.modified()?,
                });
            }
        }
        found.ok_or(ProfileError::NotCached)
    }
}
