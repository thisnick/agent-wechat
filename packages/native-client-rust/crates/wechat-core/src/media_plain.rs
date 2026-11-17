//! Large attachment bytes kept in private temporary files.
//!
//! A file or original-quality video can be 512 MiB. Sends, downloads, profile
//! writeback and the encrypted cache move such bytes through one private file
//! and fixed-size steps instead of holding them in memory. Images, voice notes
//! and other representations capped near 25 MiB stay in memory.
use super::*;
use md5::Md5;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// Write step for files created here.
const STEP: usize = 256 * 1024;

/// Plaintext in a file whose length and digests are known. A file this created
/// is private and temporary: it and its directory are removed on drop. An
/// [`PlainFile::existing`] file, such as a checked profile copy, is only read.
pub(crate) struct PlainFile {
    _directory: Option<wechat_platform::PrivateTempDir>,
    pub(crate) path: std::path::PathBuf,
    pub(crate) byte_length: u64,
    pub(crate) md5: String,
    pub(crate) sha256: String,
}

/// A writer that counts and hashes what passes through it.
pub(crate) struct HashingWriter<W> {
    inner: W,
    md5: Md5,
    sha256: Sha256,
    length: u64,
}
impl<W: Write> HashingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            md5: Md5::new(),
            sha256: Sha256::new(),
            length: 0,
        }
    }
    /// The inner writer, the byte count, and the MD5 and SHA-256 in hex.
    pub(crate) fn finish(self) -> (W, u64, String, String) {
        (
            self.inner,
            self.length,
            format!("{:x}", self.md5.finalize()),
            format!("{:x}", self.sha256.finalize()),
        )
    }
}
impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(data)?;
        self.md5.update(&data[..written]);
        self.sha256.update(&data[..written]);
        self.length += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl PlainFile {
    /// Create a new private file and fill it through `write`, hashing every
    /// byte. On any error nothing is left behind.
    pub(crate) fn write_with(
        prefix: &str,
        write: impl FnOnce(&mut dyn Write) -> Result<()>,
    ) -> Result<Self> {
        let directory = wechat_platform::private_tempdir(prefix)?;
        let path = directory.path().join("payload");
        let file = wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let mut output = HashingWriter::new(std::io::BufWriter::with_capacity(STEP, file));
        write(&mut output)?;
        output.flush()?;
        let (buffered, byte_length, md5, sha256) = output.finish();
        drop(buffered.into_inner().map_err(|error| error.into_error())?);
        Ok(Self {
            _directory: Some(directory),
            path,
            byte_length,
            md5,
            sha256,
        })
    }
    /// A file someone else owns whose length and digests were already checked,
    /// read where it is. It is never removed.
    pub(crate) fn existing(
        path: std::path::PathBuf,
        byte_length: u64,
        md5: String,
        sha256: String,
    ) -> Self {
        Self {
            _directory: None,
            path,
            byte_length,
            md5,
            sha256,
        }
    }
    /// Open the file, refusing one whose length changed.
    pub(crate) fn open(&self) -> Result<std::fs::File> {
        let file = std::fs::File::open(&self.path)?;
        ensure!(
            file.metadata()?.len() == self.byte_length,
            "media_integrity_failed"
        );
        Ok(file)
    }
    /// The whole file in memory, for representations limited to `max` bytes.
    /// The bytes are checked against the recorded SHA-256 again.
    pub(crate) fn read_bounded(&self, max: usize) -> Result<Vec<u8>> {
        ensure!(self.byte_length <= max as u64, "source_size_limit");
        let mut bytes = Vec::with_capacity(self.byte_length as usize);
        self.open()?
            .take(self.byte_length + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == self.byte_length && digest(&bytes) == self.sha256,
            "media_integrity_failed"
        );
        Ok(bytes)
    }
    pub(crate) fn profile(&self) -> wechat_profile::MediaPayload<'_> {
        wechat_profile::MediaPayload::File {
            path: &self.path,
            length: self.byte_length,
        }
    }
    /// The movie container type, reading only box headers.
    pub(crate) fn video_container(&self) -> Result<&'static str> {
        wechat_media::inspect_video_reader(&mut self.open()?, self.byte_length)
    }
}

/// Attachment bytes held in memory or in a private file.
#[derive(Clone, Copy)]
pub(crate) enum Payload<'a> {
    Bytes(&'a [u8]),
    File(&'a PlainFile),
}
impl Payload<'_> {
    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::File(file) => file.byte_length,
        }
    }
    pub(crate) fn sha256(&self) -> String {
        match self {
            Self::Bytes(bytes) => digest(bytes),
            Self::File(file) => file.sha256.clone(),
        }
    }
    pub(crate) fn md5(&self) -> String {
        match self {
            Self::Bytes(bytes) => format!("{:x}", Md5::digest(bytes)),
            Self::File(file) => file.md5.clone(),
        }
    }
    pub(crate) fn profile(&self) -> wechat_profile::MediaPayload<'_> {
        match self {
            Self::Bytes(bytes) => wechat_profile::MediaPayload::Bytes(bytes),
            Self::File(file) => file.profile(),
        }
    }
    /// The bytes in memory, when they are held there.
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::File(_) => None,
        }
    }
}

/// Attachment bytes owned in memory or in a private file.
pub(crate) enum OwnedPayload {
    Bytes(Vec<u8>),
    File(PlainFile),
}
impl OwnedPayload {
    pub(crate) fn as_payload(&self) -> Payload<'_> {
        match self {
            Self::Bytes(bytes) => Payload::Bytes(bytes),
            Self::File(file) => Payload::File(file),
        }
    }
}

impl Core {
    /// Decrypt upload `id` into a new private file. Its length and SHA-256
    /// must match the upload's record; its MD5 is computed on the way.
    pub(crate) fn source_file(
        &self,
        account: &str,
        id: &str,
    ) -> Result<(OwnedMediaSource, PlainFile)> {
        let mut source = None;
        let file = PlainFile::write_with("wechat-media-source-", |output| {
            let (meta, mut reader) = self.store.media_source_reader(account, id)?;
            self.vault
                .open_media_stream(account, &format!("source:{id}"), &mut reader, output)?;
            source = Some(meta);
            Ok(())
        })?;
        let source = source.context("media_source_missing")?;
        ensure!(
            file.byte_length == source.byte_length && file.sha256 == source.sha256,
            "source_integrity_failed"
        );
        Ok((source, file))
    }
    /// This store's own copy of `asset`, decrypted into a private file in steps
    /// and checked against the asset's recorded size and SHA-256. `None` when
    /// this store keeps no copy, as for an attachment read from the profile.
    pub(crate) fn asset_file(
        &self,
        account: &str,
        asset: &MediaAsset,
    ) -> Result<Option<PlainFile>> {
        let Some(payload) = self.store.media_asset_payload(account, &asset.id)? else {
            return Ok(None);
        };
        let purpose = format!("asset:{}", asset.id);
        let file = PlainFile::write_with("wechat-media-asset-", |output| {
            match payload {
                wechat_store::MediaAssetPayload::File { file, .. } => {
                    self.vault.open_media_stream(
                        account,
                        &purpose,
                        &mut std::io::BufReader::with_capacity(STEP, file),
                        output,
                    )?;
                }
                // Written by an earlier version; small enough to open whole.
                wechat_store::MediaAssetPayload::Embedded(sealed) => {
                    output.write_all(&self.vault.open_media_record(account, &purpose, &sealed)?)?;
                }
            }
            Ok(())
        })?;
        ensure!(
            asset.byte_length == Some(file.byte_length)
                && asset.sha256.as_deref() == Some(file.sha256.as_str()),
            "media_integrity_failed"
        );
        Ok(Some(file))
    }
    /// Seal `file` into this store's own copy of asset `id` without reading it
    /// into memory, then save `metadata` for it.
    pub(crate) fn save_asset_file(
        &self,
        account: &str,
        id: &str,
        metadata: &serde_json::Value,
        file: &PlainFile,
    ) -> Result<()> {
        self.store
            .save_media_asset_streamed(account, id, metadata, |output| {
                let (length, sha256) = self.vault.seal_media_stream(
                    account,
                    &format!("asset:{id}"),
                    &mut file.open()?,
                    output,
                    wechat_media::MAX_FILE_BYTES as u64,
                )?;
                ensure!(
                    length == file.byte_length && sha256 == file.sha256,
                    "media_integrity_failed"
                );
                Ok(())
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_files_record_digests_and_refuse_changed_bytes() {
        let bytes: Vec<u8> = (0..STEP * 2 + 5).map(|i| (i % 249) as u8).collect();
        let file = PlainFile::write_with("wechat-plain-test-", |output| {
            for chunk in bytes.chunks(10_000) {
                output.write_all(chunk)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(file.byte_length, bytes.len() as u64);
        assert_eq!(file.sha256, digest(&bytes));
        assert_eq!(file.md5, format!("{:x}", Md5::digest(&bytes)));
        assert_eq!(file.read_bounded(bytes.len()).unwrap(), bytes);
        assert_eq!(
            file.read_bounded(bytes.len() - 1).unwrap_err().to_string(),
            "source_size_limit"
        );
        let mut changed = bytes.clone();
        changed[7] ^= 1;
        std::fs::write(&file.path, &changed).unwrap();
        assert_eq!(
            file.read_bounded(bytes.len()).unwrap_err().to_string(),
            "media_integrity_failed"
        );
        std::fs::write(&file.path, &changed[1..]).unwrap();
        assert!(file.open().is_err());
        let path = file.path.clone();
        drop(file);
        assert!(!path.exists());
        // A file described as existing is read in place and never removed.
        let directory = wechat_platform::private_tempdir("wechat-plain-test-").unwrap();
        let kept = directory.path().join("kept");
        std::fs::write(&kept, &bytes).unwrap();
        let existing = PlainFile::existing(
            kept.clone(),
            bytes.len() as u64,
            digest(&bytes),
            digest(&bytes),
        );
        assert_eq!(
            existing.open().unwrap().metadata().unwrap().len(),
            bytes.len() as u64
        );
        drop(existing);
        assert!(kept.exists());
        // A failed write leaves nothing behind.
        let mut left = None;
        assert!(PlainFile::write_with("wechat-plain-test-", |output| {
            output.write_all(b"partial")?;
            left = Some(());
            bail!("stopped")
        })
        .is_err());
        assert!(left.is_some());
    }
}
