//! AEAD binds protected records to an account and purpose. The key is independent
//! of client API tokens and is never regenerated when protected records exist.
use anyhow::{bail, Context, Result};
use openssl::{
    rand::rand_bytes,
    symm::{decrypt_aead, Cipher, Crypter, Mode},
};
use std::{
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

pub struct Vault {
    key: Zeroizing<Vec<u8>>,
}
const MAX_CONTROL_RECORD_BYTES: usize = 32 * 1024 * 1024;
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

#[cfg(test)]
mod tests {
    use super::*;
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
