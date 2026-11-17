//! AEAD binds protected records to an account and purpose. The key is independent
//! of client API tokens and is never regenerated when protected records exist.
use anyhow::{bail, Context, Result};
use openssl::{
    rand::rand_bytes,
    symm::{decrypt_aead, Cipher, Crypter, Mode},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use zeroize::Zeroizing;

pub struct Vault {
    key: Zeroizing<Vec<u8>>,
}
const MAX_CONTROL_RECORD_BYTES: usize = 32 * 1024 * 1024;
/// Plaintext read or written per step when a media record is streamed.
const STREAM_CHUNK: usize = 1024 * 1024;
const RECORD_OVERHEAD: usize = 29;
fn media_purpose(purpose: &str) -> bool {
    ["source:", "asset:"].iter().any(|prefix| {
        purpose
            .strip_prefix(prefix)
            .is_some_and(|id| !id.is_empty())
    })
}
impl Vault {
    pub fn open(path: &Path, allow_create: bool) -> Result<Self> {
        if !path.exists() && allow_create {
            let parent = path.parent().context("storage key needs parent")?;
            crate::private_dir(parent)?;
            let mut key = Zeroizing::new(vec![0u8; 32]);
            rand_bytes(&mut key)?;
            match wechat_platform::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(wechat_platform::NOFOLLOW)
                .open(path)
            {
                Ok(mut f) => {
                    f.write_all(&key)?;
                    f.sync_all()?;
                    wechat_platform::sync_dir(parent)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e).context("cannot create storage key"),
            }
        }
        let mut file = wechat_platform::OpenOptions::new()
            .read(true)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(path)
            .context("storage key unavailable; existing data preserved")?;
        let meta = file.metadata()?;
        if !meta.is_file() || !wechat_platform::private_file(&file)? || meta.len() != 32 {
            bail!("storage key must be an owner-only 32-byte file");
        }
        let mut key = Zeroizing::new(vec![0u8; 32]);
        file.read_exact(&mut key)?;
        Ok(Self { key })
    }
    pub fn seal(&self, account: &str, purpose: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.seal_bounded(account, purpose, plaintext, MAX_CONTROL_RECORD_BYTES)
    }
    /// Large records are limited to owned source and verified attachment bytes.
    /// Control state and send plans retain the smaller generic record limit.
    pub fn seal_media(&self, account: &str, purpose: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        self.seal_bounded(account, purpose, plaintext, wechat_media::MAX_FILE_BYTES)
    }
    fn seal_bounded(
        &self,
        account: &str,
        purpose: &str,
        plaintext: &[u8],
        max: usize,
    ) -> Result<Vec<u8>> {
        if plaintext.len() > max {
            bail!("protected record exceeds limit");
        }
        let mut nonce = [0u8; 12];
        rand_bytes(&mut nonce)?;
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let cipher = Cipher::aes_256_gcm();
        let mut crypter = Crypter::new(cipher, Mode::Encrypt, &self.key, Some(&nonce))?;
        crypter.aad_update(aad.as_bytes())?;
        let mut output = vec![0; RECORD_OVERHEAD + plaintext.len() + cipher.block_size()];
        output[0] = 1;
        output[1..13].copy_from_slice(&nonce);
        let count = crypter.update(plaintext, &mut output[RECORD_OVERHEAD..])?;
        let rest = crypter.finalize(&mut output[RECORD_OVERHEAD + count..])?;
        crypter.get_tag(&mut output[13..RECORD_OVERHEAD])?;
        output.truncate(RECORD_OVERHEAD + count + rest);
        Ok(output)
    }
    pub fn open_record(
        &self,
        account: &str,
        purpose: &str,
        record: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        self.open_bounded(account, purpose, record, MAX_CONTROL_RECORD_BYTES)
    }
    pub fn open_media_record(
        &self,
        account: &str,
        purpose: &str,
        record: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        self.open_bounded(account, purpose, record, wechat_media::MAX_FILE_BYTES)
    }
    fn open_bounded(
        &self,
        account: &str,
        purpose: &str,
        record: &[u8],
        max: usize,
    ) -> Result<Zeroizing<Vec<u8>>> {
        if record.len() < RECORD_OVERHEAD || record[0] != 1 || record.len() > max + RECORD_OVERHEAD
        {
            bail!("invalid protected record");
        }
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let plain = decrypt_aead(
            Cipher::aes_256_gcm(),
            &self.key,
            Some(&record[1..13]),
            aad.as_bytes(),
            &record[29..],
            &record[13..29],
        )
        .map_err(|_| anyhow::anyhow!("storage unlock/integrity failed; existing data preserved"))?;
        Ok(Zeroizing::new(plain))
    }
}

impl Vault {
    /// Encrypt `input` into `output` as one media record, byte-for-byte the same
    /// format as [`Self::seal_media`], without holding the plaintext in memory.
    /// The tag is written last, so `output` must be seekable and start empty.
    /// Returns the plaintext length and its lowercase SHA-256. More than `max`
    /// plaintext bytes fail with `protected record exceeds limit`.
    pub fn seal_media_stream(
        &self,
        account: &str,
        purpose: &str,
        input: &mut dyn Read,
        output: &mut (impl Write + Seek),
        max: u64,
    ) -> Result<(u64, String)> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        let max = max.min(wechat_media::MAX_FILE_BYTES as u64);
        let mut nonce = [0u8; 12];
        rand_bytes(&mut nonce)?;
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let cipher = Cipher::aes_256_gcm();
        let mut crypter = Crypter::new(cipher, Mode::Encrypt, &self.key, Some(&nonce))?;
        crypter.aad_update(aad.as_bytes())?;
        let mut header = [0u8; RECORD_OVERHEAD];
        header[0] = 1;
        header[1..13].copy_from_slice(&nonce);
        output.write_all(&header)?;
        let mut plain = Zeroizing::new(vec![0u8; STREAM_CHUNK]);
        let mut sealed = vec![0u8; STREAM_CHUNK + cipher.block_size()];
        let (mut total, mut hash) = (0u64, Sha256::new());
        loop {
            let read = match input.read(&mut plain) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            total += read as u64;
            if total > max {
                bail!("protected record exceeds limit");
            }
            hash.update(&plain[..read]);
            let count = crypter.update(&plain[..read], &mut sealed)?;
            output.write_all(&sealed[..count])?;
        }
        let count = crypter.finalize(&mut sealed)?;
        output.write_all(&sealed[..count])?;
        let mut tag = [0u8; 16];
        crypter.get_tag(&mut tag)?;
        output.seek(SeekFrom::Start(13))?;
        output.write_all(&tag)?;
        output.seek(SeekFrom::End(0))?;
        Ok((total, format!("{:x}", hash.finalize())))
    }
    /// Decrypt one media record from `input` into `output` without holding it.
    /// The tag is only checked at the end: on an error, everything already
    /// written to `output` is unverified and must be discarded. Returns the
    /// plaintext length.
    pub fn open_media_stream(
        &self,
        account: &str,
        purpose: &str,
        input: &mut dyn Read,
        output: &mut dyn Write,
    ) -> Result<u64> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        let mut header = [0u8; RECORD_OVERHEAD];
        input
            .read_exact(&mut header)
            .map_err(|_| anyhow::anyhow!("invalid protected record"))?;
        if header[0] != 1 {
            bail!("invalid protected record");
        }
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let cipher = Cipher::aes_256_gcm();
        let mut crypter = Crypter::new(cipher, Mode::Decrypt, &self.key, Some(&header[1..13]))?;
        crypter.aad_update(aad.as_bytes())?;
        crypter.set_tag(&header[13..RECORD_OVERHEAD])?;
        let mut sealed = vec![0u8; STREAM_CHUNK];
        let mut plain = Zeroizing::new(vec![0u8; STREAM_CHUNK + cipher.block_size()]);
        let mut total = 0u64;
        loop {
            let read = match input.read(&mut sealed) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            total += read as u64;
            if total > wechat_media::MAX_FILE_BYTES as u64 {
                bail!("invalid protected record");
            }
            let count = crypter.update(&sealed[..read], &mut plain)?;
            output.write_all(&plain[..count])?;
        }
        let count = crypter.finalize(&mut plain).map_err(|_| {
            anyhow::anyhow!("storage unlock/integrity failed; existing data preserved")
        })?;
        output.write_all(&plain[..count])?;
        Ok(total)
    }
}

/// Plaintext of one media record, decrypted as it is read, in the same format
/// [`Vault::open_media_stream`] reads. The tag is checked when the end of the
/// record is reached: a failed check is the error of that final `read`, so
/// everything read before `read` returns 0 is unverified.
pub struct MediaRecordReader<R> {
    input: R,
    crypter: Crypter,
    sealed: Vec<u8>,
    plain: Zeroizing<Vec<u8>>,
    start: usize,
    end: usize,
    total: u64,
    finished: bool,
}
impl<R: Read> Read for MediaRecordReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.start < self.end {
                let count = buffer.len().min(self.end - self.start);
                buffer[..count].copy_from_slice(&self.plain[self.start..self.start + count]);
                self.start += count;
                return Ok(count);
            }
            if self.finished {
                return Ok(0);
            }
            let read = self.input.read(&mut self.sealed)?;
            let count = if read == 0 {
                self.finished = true;
                self.crypter.finalize(&mut self.plain).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "storage unlock/integrity failed; existing data preserved",
                    )
                })?
            } else {
                self.total += read as u64;
                if self.total > wechat_media::MAX_FILE_BYTES as u64 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid protected record",
                    ));
                }
                self.crypter
                    .update(&self.sealed[..read], &mut self.plain)
                    .map_err(std::io::Error::other)?
            };
            self.start = 0;
            self.end = count;
        }
    }
}

/// Plaintext bytes `offset..offset + length` of one media record, decrypted
/// without reading the rest of it. The record is AES-256-GCM, whose keystream
/// for plaintext block `i` is AES-CTR at counter `2 + i`; the range is
/// decrypted with that counter. The tag covers the whole record, so it is NOT
/// checked here: use this only for a record whose whole content was verified
/// and that has not changed since.
pub struct MediaRangeReader<R> {
    input: R,
    crypter: Crypter,
    sealed: Vec<u8>,
    plain: Zeroizing<Vec<u8>>,
    start: usize,
    end: usize,
    /// Leading plaintext bytes of the first block that precede `offset`.
    skip: usize,
    /// Ciphertext bytes still to read.
    unread: u64,
}
impl<R: Read> Read for MediaRangeReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.start < self.end {
                let count = buffer.len().min(self.end - self.start);
                buffer[..count].copy_from_slice(&self.plain[self.start..self.start + count]);
                self.start += count;
                return Ok(count);
            }
            if self.unread == 0 {
                return Ok(0);
            }
            let step = self.sealed.len().min(self.unread as usize);
            self.input.read_exact(&mut self.sealed[..step])?;
            self.unread -= step as u64;
            let count = self
                .crypter
                .update(&self.sealed[..step], &mut self.plain)
                .map_err(std::io::Error::other)?;
            self.start = self.skip.min(count);
            self.skip -= self.start;
            self.end = count;
        }
    }
}

impl Vault {
    /// A reader of one media record's plaintext; see [`MediaRecordReader`].
    pub fn media_record_reader<R: Read>(
        &self,
        account: &str,
        purpose: &str,
        mut input: R,
    ) -> Result<MediaRecordReader<R>> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        let mut header = [0u8; RECORD_OVERHEAD];
        input
            .read_exact(&mut header)
            .map_err(|_| anyhow::anyhow!("invalid protected record"))?;
        if header[0] != 1 {
            bail!("invalid protected record");
        }
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let cipher = Cipher::aes_256_gcm();
        let mut crypter = Crypter::new(cipher, Mode::Decrypt, &self.key, Some(&header[1..13]))?;
        crypter.aad_update(aad.as_bytes())?;
        crypter.set_tag(&header[13..RECORD_OVERHEAD])?;
        Ok(MediaRecordReader {
            input,
            crypter,
            sealed: vec![0u8; STREAM_CHUNK],
            plain: Zeroizing::new(vec![0u8; STREAM_CHUNK + cipher.block_size()]),
            start: 0,
            end: 0,
            total: 0,
            finished: false,
        })
    }
    /// A reader of `length` plaintext bytes at `offset` of a verified media
    /// record holding `plaintext_length` bytes; see [`MediaRangeReader`].
    pub fn media_range_reader<R: Read + Seek>(
        &self,
        purpose: &str,
        mut input: R,
        plaintext_length: u64,
        offset: u64,
        length: u64,
    ) -> Result<MediaRangeReader<R>> {
        if !media_purpose(purpose) {
            bail!("invalid protected media purpose");
        }
        let end = offset.checked_add(length).context("invalid media range")?;
        if length == 0
            || end > plaintext_length
            || plaintext_length > wechat_media::MAX_FILE_BYTES as u64
        {
            bail!("invalid media range");
        }
        let mut header = [0u8; RECORD_OVERHEAD];
        input.seek(SeekFrom::Start(0))?;
        input
            .read_exact(&mut header)
            .map_err(|_| anyhow::anyhow!("invalid protected record"))?;
        if header[0] != 1
            || input.seek(SeekFrom::End(0))? != RECORD_OVERHEAD as u64 + plaintext_length
        {
            bail!("invalid protected record");
        }
        let first = offset / 16;
        let last = (end - 1) / 16;
        let mut counter = [0u8; 16];
        counter[..12].copy_from_slice(&header[1..13]);
        // GCM increments only the low 32 bits; a record this size never wraps them.
        counter[12..].copy_from_slice(&u32::try_from(2 + first)?.to_be_bytes());
        u32::try_from(2 + last)?;
        let crypter = Crypter::new(
            Cipher::aes_256_ctr(),
            Mode::Decrypt,
            &self.key,
            Some(&counter),
        )?;
        input.seek(SeekFrom::Start(RECORD_OVERHEAD as u64 + first * 16))?;
        Ok(MediaRangeReader {
            input,
            crypter,
            sealed: vec![0u8; STREAM_CHUNK],
            plain: Zeroizing::new(vec![0u8; STREAM_CHUNK + 16]),
            start: 0,
            end: 0,
            skip: (offset % 16) as usize,
            unread: end - first * 16,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_readers_return_whole_records_and_exact_ranges() {
        let t = tempfile::tempdir().unwrap();
        let vault = Vault::open(&t.path().join("private/key"), true).unwrap();
        let plain: Vec<u8> = (0..STREAM_CHUNK * 2 + 4321)
            .map(|i| (i * 13 % 251) as u8)
            .collect();
        let sealed = vault.seal_media("one", "asset:a", &plain).unwrap();
        let mut whole = vec![];
        vault
            .media_record_reader("one", "asset:a", std::io::Cursor::new(&sealed))
            .unwrap()
            .read_to_end(&mut whole)
            .unwrap();
        assert_eq!(whole, plain);
        let length = plain.len() as u64;
        for (offset, count) in [
            (0, 1),
            (0, length),
            (15, 2),
            (16, 16),
            (17, 100_000),
            (STREAM_CHUNK as u64 - 3, 7),
            (length - 1, 1),
            (length - 5000, 5000),
        ] {
            let mut range = vec![];
            vault
                .media_range_reader(
                    "asset:a",
                    std::io::Cursor::new(&sealed),
                    length,
                    offset,
                    count,
                )
                .unwrap()
                .read_to_end(&mut range)
                .unwrap();
            assert_eq!(
                range,
                plain[offset as usize..(offset + count) as usize],
                "{offset}+{count}"
            );
        }
        // Out-of-bounds ranges and a record of another length are refused.
        assert!(vault
            .media_range_reader("asset:a", std::io::Cursor::new(&sealed), length, length, 1)
            .is_err());
        assert!(vault
            .media_range_reader("asset:a", std::io::Cursor::new(&sealed), length + 1, 0, 1)
            .is_err());
        // A changed record fails its tag at the end of a whole read.
        let mut changed = sealed.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        let mut reader = vault
            .media_record_reader("one", "asset:a", std::io::Cursor::new(&changed))
            .unwrap();
        assert!(reader.read_to_end(&mut vec![]).is_err());
        // So does a record read under another binding.
        assert!(vault
            .media_record_reader("two", "asset:a", std::io::Cursor::new(&sealed))
            .unwrap()
            .read_to_end(&mut vec![])
            .is_err());
    }
    #[test]
    fn streamed_media_records_match_whole_records_and_keep_their_bindings() {
        let t = tempfile::tempdir().unwrap();
        let vault = Vault::open(&t.path().join("private/key"), true).unwrap();
        // Larger than one stream step, and not a multiple of it.
        let plain: Vec<u8> = (0..STREAM_CHUNK * 2 + 4321)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut sealed = std::io::Cursor::new(Vec::new());
        let (length, sha) = vault
            .seal_media_stream("a", "source:one", &mut &plain[..], &mut sealed, u64::MAX)
            .unwrap();
        assert_eq!(length, plain.len() as u64);
        assert_eq!(sha, format!("{:x}", Sha256::digest(&plain)));
        let sealed = sealed.into_inner();
        // A streamed record opens as a whole record, and the reverse.
        assert_eq!(
            &*vault.open_media_record("a", "source:one", &sealed).unwrap(),
            &plain[..]
        );
        let whole = vault.seal_media("a", "source:one", &plain).unwrap();
        let mut opened = Vec::new();
        assert_eq!(
            vault
                .open_media_stream("a", "source:one", &mut &whole[..], &mut opened)
                .unwrap(),
            plain.len() as u64
        );
        assert_eq!(opened, plain);
        // Tampering, another account or purpose, and an empty record all fail.
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        for (account, purpose, record) in [
            ("a", "source:one", &tampered),
            ("b", "source:one", &sealed),
            ("a", "source:two", &sealed),
        ] {
            assert!(vault
                .open_media_stream(account, purpose, &mut &record[..], &mut Vec::new())
                .is_err());
        }
        assert!(vault
            .open_media_stream("a", "source:one", &mut &[][..], &mut Vec::new())
            .is_err());
        // The plaintext limit is enforced while streaming, and purposes are checked.
        let error = vault
            .seal_media_stream(
                "a",
                "source:one",
                &mut &plain[..],
                &mut std::io::Cursor::new(Vec::new()),
                100,
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "protected record exceeds limit");
        assert!(vault
            .seal_media_stream(
                "a",
                "state",
                &mut &plain[..],
                &mut std::io::Cursor::new(Vec::new()),
                u64::MAX
            )
            .is_err());
    }
    #[test]
    fn integrity_account_purpose_and_key_are_bound() {
        let t = tempfile::tempdir().unwrap();
        let vault = Vault::open(&t.path().join("private/key"), true).unwrap();
        let mut sealed = vault.seal("a", "state", b"synthetic secret").unwrap();
        assert_eq!(
            &*vault.open_record("a", "state", &sealed).unwrap(),
            b"synthetic secret"
        );
        assert!(vault.open_record("b", "state", &sealed).is_err());
        assert!(vault.open_record("a", "cursor", &sealed).is_err());
        let other = Vault::open(&t.path().join("private/other"), true).unwrap();
        assert!(other.open_record("a", "state", &sealed).is_err());
        sealed[29] ^= 1;
        assert!(vault.open_record("a", "state", &sealed).is_err());
    }
    #[test]
    fn larger_media_records_retain_control_limits_format_and_identity_binding() {
        let t = tempfile::tempdir().unwrap();
        let vault = Vault::open(&t.path().join("private/key"), true).unwrap();
        // Exercise the old boundary without allocating the new maximum.
        let plain = vec![7; MAX_CONTROL_RECORD_BYTES + 1];
        assert!(vault.seal("a", "state", &plain).is_err());
        assert!(vault.seal_media("a", "state", b"small").is_err());
        let sealed = vault.seal_media("a", "source:owned", &plain).unwrap();
        assert_eq!(sealed.len(), plain.len() + RECORD_OVERHEAD);
        assert!(vault.open_record("a", "source:owned", &sealed).is_err());
        assert!(vault
            .open_media_record("b", "source:owned", &sealed)
            .is_err());
        assert!(vault
            .open_media_record("a", "asset:owned", &sealed)
            .is_err());
        assert!(vault.open_media_record("a", "state", &sealed).is_err());
        assert_eq!(
            &**vault
                .open_media_record("a", "source:owned", &sealed)
                .unwrap(),
            &plain
        );
        let small = vault.seal("a", "asset:small", b"legacy cache").unwrap();
        assert_eq!(
            &**vault.open_media_record("a", "asset:small", &small).unwrap(),
            b"legacy cache"
        );
        assert!(vault.seal_bounded("a", "state", b"four", 3).is_err());
        assert!(vault.open_bounded("a", "asset:small", &small, 3).is_err());
    }
    #[test]
    fn missing_key_is_not_replaced_for_existing_state() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("missing");
        assert!(Vault::open(&p, false).is_err());
        assert!(!p.exists());
    }
}
