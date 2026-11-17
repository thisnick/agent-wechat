//! Upload plaintext that is encrypted one chunk at a time.
//!
//! CDN objects use AES-128-ECB with PKCS#7 padding, so each 16-byte ciphertext
//! block depends only on the plaintext block at the same offset. A chunk that
//! starts on a block boundary is encrypted on its own from that plaintext
//! range, and the result is byte-for-byte the slice of the whole-object
//! ciphertext. Large files are therefore read from disk as they are sent
//! instead of being encrypted into memory first.
use super::*;
use std::{
    io::{Seek, SeekFrom},
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

/// Where an upload's plaintext comes from.
#[derive(Clone)]
pub(super) enum UploadSource {
    Memory(Arc<Zeroizing<Vec<u8>>>),
    /// A private file the caller has already checked. Its length, MD5 and
    /// SHA-256 are checked again while the object is sent.
    File(Arc<Mutex<std::fs::File>>),
}
impl UploadSource {
    /// Fill `buffer` with the plaintext at `offset`. A short source is an error.
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        match self {
            Self::Memory(bytes) => {
                let start = usize::try_from(offset)?;
                let end = start
                    .checked_add(buffer.len())
                    .filter(|end| *end <= bytes.len())
                    .ok_or_else(|| anyhow::anyhow!("Upload source changed"))?;
                buffer.copy_from_slice(&bytes[start..end]);
            }
            Self::File(file) => {
                let mut file = file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Upload source unavailable"))?;
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(buffer)
                    .map_err(|_| anyhow::anyhow!("Upload source changed"))?;
            }
        }
        Ok(())
    }
}

fn ciphertext_len(plaintext: usize) -> usize {
    (plaintext / 16 + 1) * 16
}

impl PreparedUpload {
    /// Prepare an upload whose plaintext stays in `file`. `length`, `md5` and
    /// `sha256` describe the file; the caller computed them while writing it.
    /// They are checked again as the object is sent: the chunk that ends the
    /// object is only produced once every byte read matched, so a changed or
    /// truncated file fails before the final request.
    #[allow(clippy::too_many_arguments)]
    pub fn from_file(
        file: std::fs::File,
        length: u64,
        md5: &str,
        sha256: &str,
        kind: MediaQuality,
        file_name: &str,
        destination: &str,
        key: &[u8],
        file_key: &str,
    ) -> Result<Self> {
        let plaintext_bytes = usize::try_from(length)?;
        check_upload_profile(plaintext_bytes, kind, file_name, destination, key, file_key)?;
        ensure!(
            md5.len() == 32
                && md5.bytes().all(|b| b.is_ascii_hexdigit())
                && sha256.len() == 64
                && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid upload source digest"
        );
        ensure!(file.metadata()?.len() == length, "Upload source changed");
        Ok(Self {
            kind,
            aes_key: key.to_vec(),
            file_key: file_key.to_string(),
            source: UploadSource::File(Arc::new(Mutex::new(file))),
            plaintext_bytes: u32::try_from(plaintext_bytes)?,
            md5: md5.to_ascii_lowercase(),
            sha256: sha256.to_ascii_lowercase(),
            file_name: file_name.into(),
            destination: destination.into(),
        })
    }
    pub(super) fn memory_source(raw: &[u8]) -> UploadSource {
        UploadSource::Memory(Arc::new(Zeroizing::new(raw.to_vec())))
    }
    /// The whole plaintext, when it is held in memory.
    pub(super) fn memory_plaintext(&self) -> Option<&[u8]> {
        match &self.source {
            UploadSource::Memory(bytes) => Some(bytes.as_slice()),
            UploadSource::File(_) => None,
        }
    }
    /// Length of the encrypted object, including its padding block.
    pub fn encrypted_len(&self) -> usize {
        ciphertext_len(self.plaintext_bytes as usize)
    }
    /// The whole encrypted object. Only for transports that send one object
    /// per request, such as the simulator; it holds the object in memory.
    pub fn encrypted_bytes(&self) -> Result<Zeroizing<Vec<u8>>> {
        let mut chunks = self.ciphertext_chunks();
        chunks.next(self.encrypted_len())
    }
    /// The encrypted object in order, one chunk at a time.
    pub(super) fn ciphertext_chunks(&self) -> CiphertextChunks<'_> {
        CiphertextChunks {
            prepared: self,
            position: 0,
            md5: None,
            sha256: None,
        }
    }
    /// Check an in-memory source completely. A file source is checked while
    /// it is sent instead, so it is not read twice.
    pub(super) fn check_memory_source(&self) -> Result<()> {
        if let Some(raw) = self.memory_plaintext() {
            ensure!(
                raw.len() == self.plaintext_bytes as usize
                    && hex::encode(md5(raw)) == self.md5
                    && hex::encode(sha256(raw)) == self.sha256,
                "Prepared upload digest mismatch"
            );
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn replace_memory_source(&mut self, raw: &[u8]) {
        self.source = Self::memory_source(raw);
    }
}

/// Checks shared by every way of preparing an upload.
pub(super) fn check_upload_profile(
    length: usize,
    kind: MediaQuality,
    file_name: &str,
    destination: &str,
    key: &[u8],
    file_key: &str,
) -> Result<()> {
    ensure!(
        length > 0
            && length <= upload_plaintext_limit(kind)
            && [
                MediaQuality::File,
                MediaQuality::LargeFile,
                MediaQuality::Original,
                MediaQuality::Video
            ]
            .contains(&kind),
        "Unsupported upload profile"
    );
    ensure!(
        !file_name.is_empty()
            && file_name.len() <= 255
            && file_name != "."
            && file_name != ".."
            && !file_name.contains(['/', '\\'])
            && !file_name.chars().any(|c| c.is_control()),
        "Invalid upload filename"
    );
    ensure!(
        key.len() == 16 && file_key.len() == 32 && file_key.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid durable upload material"
    );
    encrypt_recipient(destination)?;
    Ok(())
}

/// Produces an upload's ciphertext from the start, in order. The plaintext is
/// hashed as it is read; the chunk holding the end of the object is returned
/// only when the length and both digests match the prepared values.
pub(super) struct CiphertextChunks<'a> {
    prepared: &'a PreparedUpload,
    position: usize,
    md5: Option<Hasher>,
    sha256: Option<Hasher>,
}
impl CiphertextChunks<'_> {
    /// Ciphertext offset of the next chunk.
    pub(super) fn position(&self) -> usize {
        self.position
    }
    /// The next `size` ciphertext bytes, or fewer at the end of the object.
    /// `size` must be a multiple of the 16-byte block size.
    pub(super) fn next(&mut self, size: usize) -> Result<Zeroizing<Vec<u8>>> {
        let total = self.prepared.encrypted_len();
        ensure!(
            size > 0 && size.is_multiple_of(16) && self.position < total,
            "Invalid upload range"
        );
        let plaintext = self.prepared.plaintext_bytes as usize;
        let end = (self.position + size).min(total);
        let read = end.min(plaintext).saturating_sub(self.position);
        let mut block = Zeroizing::new(vec![0u8; end - self.position]);
        self.prepared
            .source
            .read_at(self.position as u64, &mut block[..read])?;
        let md5 = match &mut self.md5 {
            Some(hasher) => hasher,
            None => self.md5.insert(Hasher::new(MessageDigest::md5())?),
        };
        md5.update(&block[..read])?;
        let sha256 = match &mut self.sha256 {
            Some(hasher) => hasher,
            None => self.sha256.insert(Hasher::new(MessageDigest::sha256())?),
        };
        sha256.update(&block[..read])?;
        if end == total {
            // PKCS#7: the final block is completed with copies of its pad length.
            let pad = (total - plaintext) as u8;
            block[read..].fill(pad);
            let md5 = hex::encode(self.md5.take().unwrap().finish()?);
            let sha256 = hex::encode(self.sha256.take().unwrap().finish()?);
            ensure!(
                md5 == self.prepared.md5 && sha256 == self.prepared.sha256,
                "Prepared upload digest mismatch"
            );
        }
        let mut crypter = Crypter::new(
            Cipher::aes_128_ecb(),
            Mode::Encrypt,
            &self.prepared.aes_key,
            None,
        )?;
        crypter.pad(false);
        let mut encrypted = Zeroizing::new(vec![0u8; block.len() + 16]);
        let count = crypter.update(&block, &mut encrypted)?;
        let rest = crypter.finalize(&mut encrypted[count..])?;
        encrypted.truncate(count + rest);
        ensure!(encrypted.len() == block.len(), "Invalid upload range");
        self.position = end;
        Ok(encrypted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn material() -> ([u8; 16], String) {
        ([9; 16], "a".repeat(32))
    }
    fn whole(prepared: &PreparedUpload, chunk: usize) -> Vec<u8> {
        let mut chunks = prepared.ciphertext_chunks();
        let mut out = vec![];
        while chunks.position() < prepared.encrypted_len() {
            out.extend_from_slice(&chunks.next(chunk).unwrap());
        }
        out
    }
    struct FileSource {
        _directory: wechat_platform::PrivateTempDir,
        path: std::path::PathBuf,
    }
    fn prepare_file(raw: &[u8], file: &FileSource) -> Result<PreparedUpload> {
        let (key, file_key) = material();
        PreparedUpload::from_file(
            std::fs::File::open(&file.path)?,
            raw.len() as u64,
            &hex::encode(md5(raw)),
            &hex::encode(sha256(raw)),
            MediaQuality::File,
            "a.bin",
            "wxid_peer",
            &key,
            &file_key,
        )
    }
    fn file_upload(raw: &[u8]) -> (FileSource, PreparedUpload) {
        let directory = wechat_platform::private_tempdir("wechat-upload-source-test-").unwrap();
        let path = directory.path().join("source");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(raw)
            .unwrap();
        let file = FileSource {
            _directory: directory,
            path,
        };
        let prepared = prepare_file(raw, &file).unwrap();
        (file, prepared)
    }
    #[test]
    fn chunked_ciphertext_matches_whole_object_encryption_at_every_boundary() {
        let (key, file_key) = material();
        for length in [
            1,
            15,
            16,
            17,
            31,
            32,
            33,
            65535,
            65536,
            65537,
            131072 - 16,
            131072,
        ] {
            let raw: Vec<u8> = (0..length).map(|i| (i * 7 % 251) as u8).collect();
            let expected = ecb_encrypt(&raw, &key).unwrap();
            let memory = PreparedUpload::with_material(
                &raw,
                MediaQuality::File,
                "a.bin",
                "wxid_peer",
                &key,
                &file_key,
            )
            .unwrap();
            let (_file, from_file) = file_upload(&raw);
            assert_eq!(memory.encrypted_len(), expected.len(), "length {length}");
            for chunk in [16, 4096, UPLOAD_CHUNK] {
                assert_eq!(whole(&memory, chunk), expected, "memory {length}/{chunk}");
                assert_eq!(whole(&from_file, chunk), expected, "file {length}/{chunk}");
            }
            assert_eq!(*memory.encrypted_bytes().unwrap(), expected);
            assert_eq!(
                decrypt_cdn(&whole(&from_file, 4096), &key).unwrap(),
                raw,
                "roundtrip {length}"
            );
        }
    }

    #[test]
    fn a_changed_or_truncated_file_never_yields_the_final_chunk() {
        let raw: Vec<u8> = (0..70_000).map(|i| (i % 256) as u8).collect();
        let (file, prepared) = file_upload(&raw);
        // Changed after preparation: earlier chunks are produced, the last is refused.
        let mut changed = raw.clone();
        changed[69_999] ^= 1;
        std::fs::write(&file.path, &changed).unwrap();
        let mut chunks = prepared.ciphertext_chunks();
        chunks.next(UPLOAD_CHUNK).unwrap();
        let error = chunks.next(UPLOAD_CHUNK).unwrap_err();
        assert_eq!(error.to_string(), "Prepared upload digest mismatch");
        // Truncated: the read itself fails.
        std::fs::write(&file.path, &raw[..66_000]).unwrap();
        let mut chunks = prepared.ciphertext_chunks();
        chunks.next(UPLOAD_CHUNK).unwrap();
        let error = chunks.next(UPLOAD_CHUNK).unwrap_err();
        assert_eq!(error.to_string(), "Upload source changed");
        // A file whose length differs is refused when prepared.
        let error = prepare_file(&raw, &file).err().unwrap();
        assert_eq!(error.to_string(), "Upload source changed");
    }
}
