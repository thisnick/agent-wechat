//! AEAD binds protected records to an account and purpose. The key is independent
//! of client API tokens and is never regenerated when protected records exist.
use anyhow::{bail, Context, Result};
use openssl::{
    rand::rand_bytes,
    symm::{decrypt_aead, encrypt_aead, Cipher},
};
use std::{
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

pub struct Vault {
    key: Zeroizing<Vec<u8>>,
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
        if plaintext.len() > 32 * 1024 * 1024 {
            bail!("protected record exceeds limit");
        }
        let mut nonce = [0u8; 12];
        rand_bytes(&mut nonce)?;
        let mut tag = [0u8; 16];
        let aad = format!("agent-wechat:1:{account}:{purpose}");
        let ciphertext = encrypt_aead(
            Cipher::aes_256_gcm(),
            &self.key,
            Some(&nonce),
            aad.as_bytes(),
            plaintext,
            &mut tag,
        )?;
        let mut output = vec![1];
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&tag);
        output.extend(ciphertext);
        Ok(output)
    }
    pub fn open_record(
        &self,
        account: &str,
        purpose: &str,
        record: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if record.len() < 29 || record[0] != 1 || record.len() > 32 * 1024 * 1024 + 29 {
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
    fn missing_key_is_not_replaced_for_existing_state() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("missing");
        assert!(Vault::open(&p, false).is_err());
        assert!(!p.exists());
    }
}
