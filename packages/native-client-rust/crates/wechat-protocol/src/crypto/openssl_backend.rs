use super::Curve;
use anyhow::{ensure, Result};
use openssl::{
    bn::{BigNum, BigNumContext},
    derive::Deriver,
    ec::{EcGroup, EcKey, EcPoint, PointConversionForm},
    hash::{hash, Hasher, MessageDigest},
    memcmp,
    nid::Nid,
    pkey::{Id, PKey, Private},
    rand::rand_bytes,
    rsa::Rsa,
    sign::{Signer, Verifier},
    symm::{Cipher, Crypter, Mode},
};
use zeroize::{Zeroize, Zeroizing};

fn curve_nid(curve: Curve) -> Nid {
    match curve {
        Curve::P224 => Nid::SECP224R1,
        Curve::P256 => Nid::X9_62_PRIME256V1,
    }
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    rand_bytes(&mut b)?;
    Ok(b)
}

pub(crate) fn sha256_array(b: &[u8]) -> [u8; 32] {
    openssl::sha::sha256(b)
}

pub(crate) fn md5_array(b: &[u8]) -> [u8; 16] {
    let digest = hash(MessageDigest::md5(), b).expect("OpenSSL MD5");
    digest.as_ref().try_into().expect("MD5 digest length")
}

pub(crate) fn hmac_sha256(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    let key = PKey::hmac(key)?;
    let mut s = Signer::new(MessageDigest::sha256(), &key)?;
    s.update(input)?;
    Ok(s.sign_to_vec()?)
}

pub(crate) fn hmac_sha256_slices(key: &[u8], parts: &[&[u8]]) -> Result<Zeroizing<Vec<u8>>> {
    let key = PKey::hmac(key)?;
    let mut s = Signer::new(MessageDigest::sha256(), &key)?;
    for part in parts {
        s.update(part)?;
    }
    Ok(Zeroizing::new(s.sign_to_vec()?))
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && memcmp::eq(a, b)
}

pub(crate) fn gcm_seal(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    plain: &[u8],
) -> Result<(Vec<u8>, [u8; 16])> {
    let cipher = match key.len() {
        16 => Cipher::aes_128_gcm(),
        24 => Cipher::aes_192_gcm(),
        _ => anyhow::bail!("Invalid GCM key"),
    };
    ensure!(iv.len() == 12, "Invalid GCM nonce");
    let mut c = Crypter::new(cipher, Mode::Encrypt, key, Some(iv))?;
    c.aad_update(aad)?;
    let mut out = vec![0; plain.len() + cipher.block_size()];
    let n = c.update(plain, &mut out)?;
    let m = c.finalize(&mut out[n..])?;
    out.truncate(n + m);
    let mut tag = [0; 16];
    c.get_tag(&mut tag)?;
    Ok((out, tag))
}

pub(crate) fn gcm_open(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    encrypted: &[u8],
    tag: &[u8],
) -> Result<Vec<u8>> {
    let cipher = match key.len() {
        16 => Cipher::aes_128_gcm(),
        24 => Cipher::aes_192_gcm(),
        _ => anyhow::bail!("Invalid GCM key"),
    };
    ensure!(iv.len() == 12 && tag.len() == 16, "Invalid GCM envelope");
    let mut c = Crypter::new(cipher, Mode::Decrypt, key, Some(iv))?;
    c.aad_update(aad)?;
    c.set_tag(tag)?;
    let mut out = vec![0; encrypted.len() + cipher.block_size()];
    let n = c.update(encrypted, &mut out)?;
    let m = match c.finalize(&mut out[n..]) {
        Ok(n) => n,
        Err(e) => {
            out.zeroize();
            return Err(e.into());
        }
    };
    out.truncate(n + m);
    Ok(out)
}

pub(crate) fn aes_128_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Result<Vec<u8>> {
    Ok(openssl::symm::encrypt(
        Cipher::aes_128_cbc(),
        key,
        Some(iv),
        plain,
    )?)
}

pub(crate) fn aes_128_cbc_decrypt(
    key: &[u8; 16],
    iv: &[u8; 16],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    Ok(openssl::symm::decrypt(
        Cipher::aes_128_cbc(),
        key,
        Some(iv),
        ciphertext,
    )?)
}

pub(crate) fn aes_128_ecb_encrypt(key: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    Ok(openssl::symm::encrypt(
        Cipher::aes_128_ecb(),
        key,
        None,
        plain,
    )?)
}

pub(crate) fn aes_128_ecb_decrypt(key: &[u8], encrypted: &[u8]) -> Result<Vec<u8>> {
    Ok(openssl::symm::decrypt(
        Cipher::aes_128_ecb(),
        key,
        None,
        encrypted,
    )?)
}

pub(crate) fn aes_128_ecb_encrypt_no_pad(key: &[u8], blocks: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut encoder = Crypter::new(Cipher::aes_128_ecb(), Mode::Encrypt, key, None)?;
    encoder.pad(false);
    let mut out = Zeroizing::new(vec![0u8; blocks.len() + 16]);
    let n = encoder.update(blocks, &mut out)?;
    let m = encoder.finalize(&mut out[n..])?;
    out.truncate(n + m);
    ensure!(out.len() == blocks.len(), "Unexpected ECB block size");
    Ok(out)
}

pub(crate) struct Aes128EcbDecryptor(Crypter);

impl Aes128EcbDecryptor {
    pub(crate) fn new(key: &[u8]) -> Result<Self> {
        Ok(Self(Crypter::new(
            Cipher::aes_128_ecb(),
            Mode::Decrypt,
            key,
            None,
        )?))
    }

    pub(crate) fn update(&mut self, input: &[u8], out: &mut [u8]) -> Result<usize> {
        Ok(self.0.update(input, out)?)
    }

    pub(crate) fn finalize(&mut self, out: &mut [u8]) -> Result<usize> {
        Ok(self.0.finalize(out)?)
    }
}

pub(crate) struct Md5Hasher(Hasher);

impl Md5Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(Hasher::new(MessageDigest::md5())?))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        Ok(self.0.update(data)?)
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 16]> {
        Ok(self.0.finish()?.as_ref().try_into()?)
    }
}

pub(crate) struct Sha256Hasher(Hasher);

impl Sha256Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(Hasher::new(MessageDigest::sha256())?))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        Ok(self.0.update(data)?)
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 32]> {
        Ok(self.0.finish()?.as_ref().try_into()?)
    }
}

pub(crate) fn chacha20_xor(
    key: &[u8; 32],
    nonce: [u8; 8],
    counter: u64,
    input: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let mut iv = Zeroizing::new([0u8; 16]);
    iv[..8].copy_from_slice(&counter.to_le_bytes());
    iv[8..].copy_from_slice(&nonce);
    let cipher = Cipher::chacha20();
    let mut crypter = Crypter::new(cipher, Mode::Encrypt, key, Some(&*iv))?;
    crypter.pad(false);
    let mut out = Zeroizing::new(vec![0; input.len() + cipher.block_size()]);
    let n = crypter.update(input, &mut out)?;
    let end = crypter.finalize(&mut out[n..])?;
    out.truncate(n + end);
    ensure!(out.len() == input.len(), "Unexpected call cipher size");
    Ok(out)
}

pub(crate) fn poly1305_mac(key: &[u8; 32], parts: &[&[u8]]) -> Result<[u8; 16]> {
    let key = PKey::private_key_from_raw_bytes(key, Id::POLY1305)?;
    let mut signer = Signer::new_without_digest(&key)?;
    for part in parts {
        signer.update(part)?;
    }
    let mut tag = [0u8; 16];
    ensure!(signer.sign(&mut tag)? == 16, "Unexpected call tag size");
    Ok(tag)
}

pub(crate) fn generate_rsa_1024_public_pem() -> Result<Vec<u8>> {
    let rsa = Rsa::generate(1024)?;
    Ok(PKey::from_rsa(rsa)?.public_key_to_pem()?)
}

pub(crate) struct Ecdh {
    key: PKey<Private>,
    nid: Nid,
}

impl Ecdh {
    pub(crate) fn generate_nid(nid: Nid) -> Result<Self> {
        let group = EcGroup::from_curve_name(nid)?;
        Ok(Self {
            key: PKey::from_ec_key(EcKey::generate(&group)?)?,
            nid,
        })
    }

    pub(crate) fn from_private_nid(nid: Nid, private: &[u8]) -> Result<Self> {
        let group = EcGroup::from_curve_name(nid)?;
        let scalar = BigNum::from_slice(private)?;
        let mut ctx = BigNumContext::new()?;
        let mut public = EcPoint::new(&group)?;
        public.mul_generator2(&group, &scalar, &mut ctx)?;
        let key = EcKey::from_private_components(&group, &scalar, &public)?;
        key.check_key()?;
        Ok(Self {
            key: PKey::from_ec_key(key)?,
            nid,
        })
    }

    pub(crate) fn generate(curve: Curve) -> Result<Self> {
        Self::generate_nid(curve_nid(curve))
    }

    pub(crate) fn from_private(curve: Curve, private: &[u8]) -> Result<Self> {
        let max_len = match curve {
            Curve::P224 => 28,
            Curve::P256 => 32,
        };
        ensure!(
            !private.is_empty() && private.len() <= max_len,
            "Invalid ECDH scalar"
        );
        Self::from_private_nid(curve_nid(curve), private)
    }

    pub(crate) fn public(&self) -> Result<Vec<u8>> {
        let key = self.key.ec_key()?;
        let mut ctx = BigNumContext::new()?;
        Ok(key
            .public_key()
            .to_bytes(key.group(), PointConversionForm::UNCOMPRESSED, &mut ctx)?)
    }

    pub(crate) fn derive(&self, point: &[u8]) -> Result<Vec<u8>> {
        let group = EcGroup::from_curve_name(self.nid)?;
        let mut ctx = BigNumContext::new()?;
        let p = EcPoint::from_bytes(&group, point, &mut ctx)?;
        let peer = EcKey::from_public_key(&group, &p)?;
        peer.check_key()?;
        let peer = PKey::from_ec_key(peer)?;
        let mut d = Deriver::new(&self.key)?;
        d.set_peer(&peer)?;
        Ok(d.derive_to_vec()?)
    }
}

pub(crate) fn verify_signature(pem: &str, data: &[u8], signature: &[u8]) -> Result<()> {
    let key = PKey::public_key_from_pem(pem.as_bytes())?;
    let ec = key.ec_key()?;
    ensure!(
        ec.group().curve_name() == Some(Nid::X9_62_PRIME256V1),
        "Unsupported signature pin curve"
    );
    let mut v = Verifier::new(MessageDigest::sha256(), &key)?;
    v.update(data)?;
    ensure!(
        v.verify(signature)?,
        "Pinned server signature verification failed"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) struct TestP256Signer {
    key: PKey<Private>,
}

#[cfg(test)]
impl TestP256Signer {
    pub(crate) fn generate() -> Result<Self> {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
        Ok(Self {
            key: PKey::from_ec_key(EcKey::generate(&group)?)?,
        })
    }

    pub(crate) fn public_pem(&self) -> Result<String> {
        Ok(String::from_utf8(self.key.public_key_to_pem()?)?)
    }

    pub(crate) fn sign_sha256(&self, data: &[u8]) -> Result<Vec<u8>> {
        let mut signer = Signer::new(MessageDigest::sha256(), &self.key)?;
        signer.update(data)?;
        Ok(signer.sign_to_vec()?)
    }
}
