use crate::MAX_MESSAGE_BYTES;
use anyhow::{ensure, Result};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use openssl::{
    bn::{BigNum, BigNumContext},
    derive::Deriver,
    ec::{EcGroup, EcKey, EcPoint, PointConversionForm},
    hash::{hash, MessageDigest},
    nid::Nid,
    pkey::{PKey, Private},
    rand::rand_bytes,
    sign::{Signer, Verifier},
    symm::{Cipher, Crypter, Mode},
};
use std::io::{Read, Write};
use zeroize::Zeroize;

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0; N];
    rand_bytes(&mut b)?;
    Ok(b)
}
pub fn sha256(b: &[u8]) -> Vec<u8> {
    hash(MessageDigest::sha256(), b)
        .expect("OpenSSL SHA256")
        .to_vec()
}
pub fn md5(b: &[u8]) -> Vec<u8> {
    hash(MessageDigest::md5(), b).expect("OpenSSL MD5").to_vec()
}
pub fn hmac(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    let key = PKey::hmac(key)?;
    let mut s = Signer::new(MessageDigest::sha256(), &key)?;
    s.update(input)?;
    Ok(s.sign_to_vec()?)
}
pub fn expand(secret: &[u8], info: &[u8], size: usize) -> Result<Vec<u8>> {
    ensure!(size > 0 && size <= 255 * 32, "Invalid HKDF size");
    let mut last = Vec::new();
    let mut out = Vec::new();
    for i in 1..=255u8 {
        last = hmac(secret, &[last.as_slice(), info, &[i]].concat())?;
        out.extend_from_slice(&last);
        if out.len() >= size {
            break;
        }
    }
    out.truncate(size);
    last.zeroize();
    Ok(out)
}
pub fn deflate(plain: &[u8]) -> Result<Vec<u8>> {
    ensure!(plain.len() <= MAX_MESSAGE_BYTES, "Plaintext too large");
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(plain)?;
    Ok(e.finish()?)
}
pub fn inflate(zipped: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(zipped);
    let mut out = Vec::new();
    decoder
        .by_ref()
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut out)?;
    ensure!(
        out.len() <= MAX_MESSAGE_BYTES,
        "Decompression limit exceeded"
    );
    ensure!(
        decoder.total_in() as usize == zipped.len(),
        "Trailing compressed bytes"
    );
    Ok(out)
}
pub fn gcm_seal(key: &[u8], iv: &[u8], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, [u8; 16])> {
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
pub fn gcm_open(
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
pub fn envelope_seal(plain: &[u8], key: &[u8], aad: &[u8], iv: [u8; 12]) -> Result<Vec<u8>> {
    ensure!(key.len() == 24, "Expected AES192 key");
    let (body, tag) = gcm_seal(key, &iv, aad, &deflate(plain)?)?;
    Ok([body, iv.to_vec(), tag.to_vec()].concat())
}
pub fn envelope_open(input: &[u8], key: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        input.len() >= 29 && input.len() <= MAX_MESSAGE_BYTES + 65536,
        "Invalid encrypted envelope"
    );
    let n = input.len();
    let zipped = gcm_open(
        key,
        &input[n - 28..n - 16],
        aad,
        &input[..n - 28],
        &input[n - 16..],
    )?;
    inflate(&zipped)
}
pub fn cbc(plain: &[u8], key: &[u8], mode: Mode) -> Result<Vec<u8>> {
    ensure!(!key.is_empty() && key.len() <= 65536, "Invalid origin key");
    ensure!(
        plain.len() <= MAX_MESSAGE_BYTES + 65536,
        "CBC body too large"
    );
    if matches!(mode, Mode::Decrypt) {
        ensure!(
            !plain.is_empty() && plain.len().is_multiple_of(16),
            "Invalid CBC ciphertext"
        );
    }
    let mut normalized = [0; 16];
    let n = key.len().min(16);
    normalized[..n].copy_from_slice(&key[..n]);
    let result = if matches!(mode, Mode::Decrypt) {
        openssl::symm::decrypt(Cipher::aes_128_cbc(), &normalized, Some(&normalized), plain)
    } else {
        openssl::symm::encrypt(Cipher::aes_128_cbc(), &normalized, Some(&normalized), plain)
    };
    normalized.zeroize();
    Ok(result?)
}

pub struct Ecdh {
    key: PKey<Private>,
    nid: Nid,
}
impl Ecdh {
    pub fn generate(nid: Nid) -> Result<Self> {
        let group = EcGroup::from_curve_name(nid)?;
        Ok(Self {
            key: PKey::from_ec_key(EcKey::generate(&group)?)?,
            nid,
        })
    }
    pub fn from_private(nid: Nid, private: &[u8]) -> Result<Self> {
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
    pub fn public(&self) -> Result<Vec<u8>> {
        let key = self.key.ec_key()?;
        let mut ctx = BigNumContext::new()?;
        Ok(key
            .public_key()
            .to_bytes(key.group(), PointConversionForm::UNCOMPRESSED, &mut ctx)?)
    }
    pub fn derive(&self, point: &[u8]) -> Result<Vec<u8>> {
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
pub fn verify_signature(pem: &str, data: &[u8], signature: &[u8]) -> Result<()> {
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
mod tests {
    use super::*;
    #[test]
    fn rfc5869() {
        let prk = hex::decode("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5")
            .unwrap();
        assert_eq!(
            hex::encode(expand(&prk, &hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap(), 42).unwrap()),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }
    #[test]
    fn p224_p256_shared_secrets() {
        for nid in [Nid::SECP224R1, Nid::X9_62_PRIME256V1] {
            let a = Ecdh::from_private(nid, &[1]).unwrap();
            let b = Ecdh::from_private(nid, &[2]).unwrap();
            assert_eq!(
                a.derive(&b.public().unwrap()).unwrap(),
                b.derive(&a.public().unwrap()).unwrap()
            );
        }
    }
    #[test]
    fn session_known_answer() {
        let key: Vec<u8> = (0..24).collect();
        let nonce = std::array::from_fn(|i| i as u8);
        let fixture=hex::decode("9e65093555920046fdfa90d437e2f0b922d46623768c7870b390381087bb0d11fa5af671c9b301db000102030405060708090a0b0d4ecbc764b639df9549f03800165ea6").unwrap();
        assert_eq!(
            envelope_seal(b"synthetic native session request", &key, b"", nonce).unwrap(),
            fixture
        );
        assert_eq!(
            envelope_open(&fixture, &key, b"").unwrap(),
            b"synthetic native session request"
        );
        for i in [0, fixture.len() - 28, fixture.len() - 1] {
            let mut bad = fixture.clone();
            bad[i] ^= 1;
            assert!(envelope_open(&bad, &key, b"").is_err());
        }
    }
}
