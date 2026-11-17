//! Uploads whose plaintext stays in a file and is encrypted one chunk at a time.
//!
//! CDN objects use AES-128-ECB with PKCS#7 padding, so each 16-byte ciphertext
//! block depends only on the plaintext block at the same offset. A chunk that
//! starts on a block boundary is encrypted on its own from that plaintext
//! range, and the result is byte-for-byte the slice of the whole-object
//! ciphertext. A [`PreparedFileUpload`] is therefore read from disk as it is
//! sent instead of being encrypted into memory first, and every request is the
//! same as for the equivalent in-memory [`PreparedUpload`].
use super::*;
use std::{
    io::{Seek, SeekFrom},
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

fn ciphertext_len(plaintext: usize) -> usize {
    (plaintext / 16 + 1) * 16
}

/// One object ready to upload from a private file. The file is read in steps
/// while the object is sent; its length, MD5 and SHA-256 are checked again on
/// the way, and the request that ends the object is only made once every byte
/// read matched. Persist the key and file key before the first request, as
/// for [`PreparedUpload`].
#[derive(Clone)]
pub struct PreparedFileUpload {
    /// CDN object type to upload as.
    pub kind: MediaQuality,
    /// AES-128 key the object is encrypted with.
    pub aes_key: Vec<u8>,
    /// Client file key that correlates the upload's requests.
    pub file_key: String,
    /// Size of the plaintext in bytes.
    pub plaintext_bytes: u32,
    /// Lowercase hex MD5 of the plaintext.
    pub md5: String,
    /// Lowercase hex SHA-256 of the plaintext.
    pub sha256: String,
    /// File name sent with the upload.
    pub file_name: String,
    /// Chat the upload is for.
    pub destination: String,
    file: Arc<Mutex<std::fs::File>>,
}
impl Drop for PreparedFileUpload {
    fn drop(&mut self) {
        self.aes_key.zeroize();
        self.file_key.zeroize();
    }
}
impl fmt::Debug for PreparedFileUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedFileUpload")
            .field("kind", &self.kind)
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("material", &"[redacted]")
            .finish()
    }
}
impl PreparedFileUpload {
    /// Prepare an upload of `file`. `length`, `md5` and `sha256` describe the
    /// file; the caller computed them while writing it. A file of another
    /// length is refused now, and changed bytes before the final request.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
            plaintext_bytes: u32::try_from(plaintext_bytes)?,
            md5: md5.to_ascii_lowercase(),
            sha256: sha256.to_ascii_lowercase(),
            file_name: file_name.into(),
            destination: destination.into(),
            file: Arc::new(Mutex::new(file)),
        })
    }
    /// Length of the encrypted object, including its padding block.
    pub fn encrypted_len(&self) -> usize {
        ciphertext_len(self.plaintext_bytes as usize)
    }
    /// The same upload held in memory, for transports that send one object per
    /// request. The file is read whole and checked against its digests.
    pub fn to_memory(&self) -> Result<PreparedUpload> {
        let mut raw = Zeroizing::new(vec![0u8; self.plaintext_bytes as usize]);
        self.read_at(0, &mut raw)?;
        let prepared = PreparedUpload::with_material(
            &raw,
            self.kind,
            &self.file_name,
            &self.destination,
            &self.aes_key,
            &self.file_key,
        )?;
        ensure!(
            prepared.md5 == self.md5 && prepared.sha256 == self.sha256,
            "Prepared upload digest mismatch"
        );
        Ok(prepared)
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| anyhow::anyhow!("Upload source unavailable"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buffer)
            .map_err(|_| anyhow::anyhow!("Upload source changed"))
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

/// Either kind of prepared upload, for the shared request builders.
#[derive(Clone, Copy)]
pub(super) enum UploadRef<'a> {
    Memory(&'a PreparedUpload),
    File(&'a PreparedFileUpload),
}
impl<'a> UploadRef<'a> {
    pub(super) fn kind(self) -> MediaQuality {
        match self {
            Self::Memory(p) => p.kind,
            Self::File(p) => p.kind,
        }
    }
    pub(super) fn aes_key(self) -> &'a [u8] {
        match self {
            Self::Memory(p) => &p.aes_key,
            Self::File(p) => &p.aes_key,
        }
    }
    pub(super) fn file_key(self) -> &'a str {
        match self {
            Self::Memory(p) => &p.file_key,
            Self::File(p) => &p.file_key,
        }
    }
    pub(super) fn plaintext_bytes(self) -> u32 {
        match self {
            Self::Memory(p) => p.plaintext_bytes,
            Self::File(p) => p.plaintext_bytes,
        }
    }
    pub(super) fn md5(self) -> &'a str {
        match self {
            Self::Memory(p) => &p.md5,
            Self::File(p) => &p.md5,
        }
    }
    pub(super) fn sha256(self) -> &'a str {
        match self {
            Self::Memory(p) => &p.sha256,
            Self::File(p) => &p.sha256,
        }
    }
    pub(super) fn file_name(self) -> &'a str {
        match self {
            Self::Memory(p) => &p.file_name,
            Self::File(p) => &p.file_name,
        }
    }
    pub(super) fn destination(self) -> &'a str {
        match self {
            Self::Memory(p) => &p.destination,
            Self::File(p) => &p.destination,
        }
    }
    pub(super) fn encrypted_len(self) -> usize {
        match self {
            Self::Memory(p) => p.encrypted.len(),
            Self::File(p) => p.encrypted_len(),
        }
    }
    /// Checks made before any request. An in-memory upload's ciphertext is
    /// fully checked; a file is checked while it is sent instead.
    pub(super) fn validate(self) -> Result<()> {
        match self {
            Self::Memory(p) => p.validate(),
            Self::File(p) => {
                ensure!(
                    p.aes_key.len() == 16
                        && p.file_key.len() == 32
                        && p.file_key.bytes().all(|b| b.is_ascii_hexdigit())
                        && p.plaintext_bytes > 0
                        && p.plaintext_bytes as usize <= upload_plaintext_limit(p.kind),
                    "Invalid prepared upload"
                );
                Ok(())
            }
        }
    }
    /// The encrypted object in order, one chunk at a time.
    pub(super) fn chunks(self) -> CiphertextChunks<'a> {
        CiphertextChunks {
            upload: self,
            position: 0,
            md5: None,
            sha256: None,
        }
    }
    /// The uploaded object's receipt for this upload.
    pub(super) fn uploaded(
        self,
        file_id: String,
        width: Option<u32>,
        height: Option<u32>,
    ) -> UploadedMedia {
        UploadedMedia {
            descriptor: MediaDescriptor {
                file_id,
                aes_key: self.aes_key().to_vec(),
                quality: self.kind(),
                expected_bytes: Some(self.plaintext_bytes() as u64),
                expected_md5: Some(self.md5().into()),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: self.plaintext_bytes(),
            md5: self.md5().into(),
            sha256: self.sha256().into(),
            file_name: self.file_name().into(),
            width,
            height,
        }
    }
}

/// Produces an upload's ciphertext from the start, in order. For a file, the
/// plaintext is hashed as it is read, and the chunk holding the end of the
/// object is returned only when the length and both digests match.
pub(super) struct CiphertextChunks<'a> {
    upload: UploadRef<'a>,
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
        let total = self.upload.encrypted_len();
        ensure!(
            size > 0 && size.is_multiple_of(16) && self.position < total,
            "Invalid upload range"
        );
        let end = (self.position + size).min(total);
        let prepared = match self.upload {
            UploadRef::Memory(prepared) => {
                let chunk = Zeroizing::new(prepared.encrypted[self.position..end].to_vec());
                self.position = end;
                return Ok(chunk);
            }
            UploadRef::File(prepared) => prepared,
        };
        let plaintext = prepared.plaintext_bytes as usize;
        let read = end.min(plaintext).saturating_sub(self.position);
        let mut block = Zeroizing::new(vec![0u8; end - self.position]);
        prepared.read_at(self.position as u64, &mut block[..read])?;
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
                md5 == prepared.md5 && sha256 == prepared.sha256,
                "Prepared upload digest mismatch"
            );
        }
        let mut crypter = Crypter::new(
            Cipher::aes_128_ecb(),
            Mode::Encrypt,
            &prepared.aes_key,
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
pub(super) mod tests {
    use super::*;
    use std::io::Write;

    fn material() -> ([u8; 16], String) {
        ([9; 16], "a".repeat(32))
    }
    fn whole(upload: UploadRef<'_>, chunk: usize) -> Vec<u8> {
        let mut chunks = upload.chunks();
        let mut out = vec![];
        while chunks.position() < upload.encrypted_len() {
            out.extend_from_slice(&chunks.next(chunk).unwrap());
        }
        out
    }
    pub(in super::super) struct FileSource {
        _directory: wechat_platform::PrivateTempDir,
        pub(in super::super) path: std::path::PathBuf,
    }
    /// A file upload of `raw` with `kind`, using the given material.
    pub(in super::super) fn file_upload_with(
        raw: &[u8],
        kind: MediaQuality,
        key: &[u8],
        file_key: &str,
    ) -> (FileSource, PreparedFileUpload) {
        let directory = wechat_platform::private_tempdir("wechat-upload-source-test-").unwrap();
        let path = directory.path().join("source");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(raw)
            .unwrap();
        let prepared = PreparedFileUpload::new(
            std::fs::File::open(&path).unwrap(),
            raw.len() as u64,
            &hex::encode(md5(raw)),
            &hex::encode(sha256(raw)),
            kind,
            "a.bin",
            "wxid_peer",
            key,
            file_key,
        )
        .unwrap();
        (
            FileSource {
                _directory: directory,
                path,
            },
            prepared,
        )
    }
    fn file_upload(raw: &[u8]) -> (FileSource, PreparedFileUpload) {
        let (key, file_key) = material();
        file_upload_with(raw, MediaQuality::File, &key, &file_key)
    }

    #[test]
    fn file_chunks_match_the_in_memory_upload_at_every_boundary() {
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
            assert_eq!(
                from_file.encrypted_len(),
                memory.encrypted.len(),
                "length {length}"
            );
            for chunk in [16, 4096, UPLOAD_CHUNK] {
                assert_eq!(whole(UploadRef::Memory(&memory), chunk), memory.encrypted);
                assert_eq!(
                    whole(UploadRef::File(&from_file), chunk),
                    memory.encrypted,
                    "file {length}/{chunk}"
                );
            }
            let copied = from_file.to_memory().unwrap();
            assert_eq!(copied.encrypted, memory.encrypted);
            assert_eq!(
                (copied.md5.clone(), copied.sha256.clone()),
                (memory.md5.clone(), memory.sha256.clone())
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
        let mut chunks = UploadRef::File(&prepared).chunks();
        chunks.next(UPLOAD_CHUNK).unwrap();
        let error = chunks.next(UPLOAD_CHUNK).unwrap_err();
        assert_eq!(error.to_string(), "Prepared upload digest mismatch");
        assert!(prepared.to_memory().is_err());
        // Truncated: the read itself fails.
        std::fs::write(&file.path, &raw[..66_000]).unwrap();
        let mut chunks = UploadRef::File(&prepared).chunks();
        chunks.next(UPLOAD_CHUNK).unwrap();
        let error = chunks.next(UPLOAD_CHUNK).unwrap_err();
        assert_eq!(error.to_string(), "Upload source changed");
        // A file whose length differs is refused when prepared.
        let (key, file_key) = material();
        let error = PreparedFileUpload::new(
            std::fs::File::open(&file.path).unwrap(),
            raw.len() as u64,
            &hex::encode(md5(&raw)),
            &hex::encode(sha256(&raw)),
            MediaQuality::File,
            "a.bin",
            "wxid_peer",
            &key,
            &file_key,
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "Upload source changed");
    }
}
