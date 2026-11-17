//! Attachment bytes for a content response, read while the response is sent.
//!
//! A file or video can be 512 MiB, and a video player asks for many byte
//! ranges of it. Bytes kept in this store's encrypted file or in an official
//! profile file are therefore streamed: a whole response is checked as it is
//! read, and a range is read on its own once the whole attachment was checked.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek};

/// How many checked attachments are remembered. A full record is cleared.
const VERIFIED_LIMIT: usize = 256;
/// Read step for checks and responses.
const STEP: usize = 256 * 1024;

/// Where an attachment's bytes were found and what they were checked against.
/// A record of this identity means the whole content matched.
#[derive(Clone, PartialEq, Eq, Debug)]
struct ContentIdentity {
    /// `vault:<file name>` for this store's copy, `profile:<path>` otherwise.
    location: String,
    file_length: u64,
    modified: Option<std::time::SystemTime>,
    byte_length: u64,
    sha256: String,
}
/// Attachments whose whole content was checked, by account and asset ID.
#[derive(Default)]
pub(crate) struct VerifiedContent(BTreeMap<(String, String), ContentIdentity>);
use std::collections::BTreeMap;

impl CoreShared {
    fn content_verified(&self, key: &(String, String), identity: &ContentIdentity) -> bool {
        self.verified_content
            .lock()
            .is_ok_and(|verified| verified.0.get(key) == Some(identity))
    }
    fn remember_verified(&self, key: (String, String), identity: ContentIdentity) {
        if let Ok(mut verified) = self.verified_content.lock() {
            if verified.0.len() >= VERIFIED_LIMIT {
                verified.0.clear();
            }
            verified.0.insert(key, identity);
        }
    }
}
use crate::CoreShared;

/// Reads `inner` to its end, then checks the length and SHA-256 of what it
/// returned. A mismatch is the error of the final `read`, so a consumer never
/// sees a clean end of a wrong attachment. A match is remembered.
struct CheckedReader<R> {
    inner: R,
    hash: Sha256,
    length: u64,
    expected_length: u64,
    expected_sha256: String,
    remember: Option<(Arc<CoreShared>, (String, String), ContentIdentity)>,
}
impl<R: Read> Read for CheckedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read == 0 && !buffer.is_empty() {
            if self.length != self.expected_length
                || format!("{:x}", std::mem::take(&mut self.hash).finalize())
                    != self.expected_sha256
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "media_integrity_failed",
                ));
            }
            if let Some((shared, key, identity)) = self.remember.take() {
                shared.remember_verified(key, identity);
            }
            return Ok(0);
        }
        self.length += read as u64;
        if self.length > self.expected_length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "media_integrity_failed",
            ));
        }
        self.hash.update(&buffer[..read]);
        Ok(read)
    }
}
/// Read everything from `input` through a check; returns whether it matched.
fn drain_checked(input: impl Read, length: u64, sha256: &str) -> Result<()> {
    let mut checked = CheckedReader {
        inner: input,
        hash: Sha256::new(),
        length: 0,
        expected_length: length,
        expected_sha256: sha256.into(),
        remember: None,
    };
    let mut buffer = vec![0u8; STEP];
    while checked
        .read(&mut buffer)
        .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?
        > 0
    {}
    Ok(())
}

fn slice_reader(bytes: Vec<u8>, range: Option<(u64, u64)>) -> Box<dyn Read + Send> {
    let mut cursor = std::io::Cursor::new(bytes);
    match range {
        Some((start, end)) => {
            cursor.set_position(start);
            Box::new(cursor.take(end - start + 1))
        }
        None => Box::new(cursor),
    }
}
fn identity_of(
    location: String,
    file: &std::fs::File,
    asset: &MediaAsset,
) -> Result<ContentIdentity> {
    let metadata = file.metadata()?;
    Ok(ContentIdentity {
        location,
        file_length: metadata.len(),
        modified: metadata.modified().ok(),
        byte_length: asset.byte_length.context("media_integrity_failed")?,
        sha256: asset.sha256.clone().context("media_integrity_failed")?,
    })
}

impl Core {
    /// Open one attachment for a content response. `select` receives the
    /// attachment's length and SHA-256 and returns the inclusive byte range to
    /// read, or `None` for all of it. The returned body reads only what was
    /// selected. A read error from it means the bytes failed a check.
    pub fn media_content_stream(
        &self,
        account: &str,
        id: &str,
        select: impl FnOnce(u64, &str) -> Option<(u64, u64)>,
    ) -> Result<MediaContentStream> {
        let metadata = self.store.media_asset(account, id)?;
        self.authorize_media_record(account, &metadata)?;
        ensure!(
            metadata.metadata["preparationExpiresAt"]
                .as_i64()
                .is_none_or(|expiry| expiry > chrono::Utc::now().timestamp()),
            "sticker_preparation_expired"
        );
        let asset: MediaAsset = serde_json::from_value(metadata.metadata.clone())?;
        ensure!(asset.state == "available", "media_not_available");
        let length = asset.byte_length.context("media_integrity_failed")?;
        let sha256 = asset.sha256.clone().context("media_integrity_failed")?;
        let range = select(length, &sha256);
        if let Some((start, end)) = range {
            ensure!(start <= end && end < length, "invalid_media_range");
        }
        let key = (account.to_string(), id.to_string());
        let body: Box<dyn Read + Send> = match self.store.media_asset_payload(account, id)? {
            Some(wechat_store::MediaAssetPayload::File { file, name }) => {
                let purpose = format!("asset:{id}");
                let identity = identity_of(format!("vault:{name}"), &file, &asset)?;
                match range {
                    None => Box::new(CheckedReader {
                        inner: self.vault.media_record_reader(
                            account,
                            &purpose,
                            std::io::BufReader::with_capacity(STEP, file),
                        )?,
                        hash: Sha256::new(),
                        length: 0,
                        expected_length: length,
                        expected_sha256: sha256.clone(),
                        remember: Some((self.shared.clone(), key, identity)),
                    }),
                    Some((start, end)) => {
                        let mut file = file;
                        if !self.content_verified(&key, &identity) {
                            // A range alone cannot be checked: check the whole
                            // record once, in steps, and remember that.
                            drain_checked(
                                self.vault.media_record_reader(
                                    account,
                                    &purpose,
                                    std::io::BufReader::with_capacity(STEP, &file),
                                )?,
                                length,
                                &sha256,
                            )?;
                            file.rewind()?;
                            self.remember_verified(key, identity);
                        }
                        Box::new(self.vault.media_range_reader(
                            &purpose,
                            file,
                            length,
                            start,
                            end - start + 1,
                        )?)
                    }
                }
            }
            Some(wechat_store::MediaAssetPayload::Embedded(sealed)) => {
                // Written by an earlier version; small enough to open whole.
                let mut opened =
                    self.vault
                        .open_media_record(account, &format!("asset:{id}"), &sealed)?;
                let bytes = std::mem::take(&mut *opened);
                ensure!(
                    bytes.len() as u64 == length && digest(&bytes) == sha256,
                    "media_integrity_failed"
                );
                slice_reader(bytes, range)
            }
            None => {
                let profile_key = self
                    .store
                    .media_asset_profile_key(account, id)?
                    .context("media_not_cached")?;
                if matches!(asset.kind, MediaKind::File | MediaKind::Video) {
                    let file = self.profile_content_file(account, &asset, &profile_key, &key)?;
                    let mut file = std::io::BufReader::with_capacity(STEP, file);
                    match range {
                        Some((start, end)) => {
                            file.seek(std::io::SeekFrom::Start(start))?;
                            Box::new(file.take(end - start + 1))
                        }
                        None => Box::new(file.take(length)),
                    }
                } else {
                    slice_reader(
                        self.profile_media_bytes(account, &asset, &profile_key)?,
                        range,
                    )
                }
            }
        };
        self.authorize_media_record(account, &metadata)?;
        Ok(MediaContentStream {
            content_type: asset
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            sha256,
            filename: asset.filename,
            byte_length: length,
            range,
            body,
        })
    }
    /// A file or video attachment in the official profile, opened for reading.
    /// The whole file is checked in steps the first time; while its path,
    /// length and modification time stay the same it is not read again.
    fn profile_content_file(
        &self,
        account: &str,
        asset: &MediaAsset,
        profile_key: &str,
        key: &(String, String),
    ) -> Result<std::fs::File> {
        let remembered = self
            .verified_content
            .lock()
            .ok()
            .and_then(|verified| verified.0.get(key).cloned())
            .filter(|identity| identity.location.starts_with("profile:"));
        if let Some(identity) = remembered {
            let path = std::path::Path::new(&identity.location["profile:".len()..]);
            if let Some(file) = open_unchanged(path, &identity, asset)? {
                return Ok(file);
            }
        }
        let verified = self.profile_hardlink(account, asset, profile_key)?;
        let identity = ContentIdentity {
            location: format!("profile:{}", verified.path.display()),
            file_length: verified.byte_length,
            modified: Some(verified.modified),
            byte_length: verified.byte_length,
            sha256: verified.sha256.clone(),
        };
        let file =
            open_unchanged(&verified.path, &identity, asset)?.context("media_integrity_failed")?;
        self.remember_verified(key.clone(), identity);
        Ok(file)
    }
}
/// Open a previously checked regular file if it still has the recorded
/// length and modification time and the attachment's recorded digest.
fn open_unchanged(
    path: &std::path::Path,
    identity: &ContentIdentity,
    asset: &MediaAsset,
) -> Result<Option<std::fs::File>> {
    if asset.byte_length != Some(identity.byte_length)
        || asset.sha256.as_deref() != Some(identity.sha256.as_str())
    {
        return Ok(None);
    }
    let link = std::fs::symlink_metadata(path)?;
    if !link.is_file() || wechat_platform::is_link(&link) {
        return Ok(None);
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() != identity.file_length
        || metadata.modified().ok() != identity.modified
        || !wechat_platform::same_path_file(path, &file)?
    {
        return Ok(None);
    }
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_reader_ends_cleanly_only_for_the_expected_bytes() {
        let bytes: Vec<u8> = (0..STEP + 99).map(|i| (i % 241) as u8).collect();
        assert!(drain_checked(&bytes[..], bytes.len() as u64, &digest(&bytes)).is_ok());
        assert!(drain_checked(&bytes[1..], bytes.len() as u64, &digest(&bytes)).is_err());
        let mut changed = bytes.clone();
        changed[3] ^= 1;
        assert!(drain_checked(&changed[..], bytes.len() as u64, &digest(&bytes)).is_err());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(drain_checked(&longer[..], bytes.len() as u64, &digest(&bytes)).is_err());
        let mut range = vec![];
        slice_reader(bytes.clone(), Some((5, 9)))
            .read_to_end(&mut range)
            .unwrap();
        assert_eq!(range, bytes[5..=9]);
    }
}
