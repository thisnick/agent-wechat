#![cfg_attr(feature = "openssl-crypto", allow(dead_code))]

use super::{CryptoBackendError, Curve};
use aes::cipher::{
    block_padding::Pkcs7, BlockDecrypt, BlockDecryptMut, BlockEncrypt, BlockEncryptMut, KeyInit,
    KeyIvInit,
};
use aes_gcm::{
    aead::{consts::U12, AeadInPlace},
    Aes128Gcm, AesGcm, Nonce, Tag,
};
use anyhow::{ensure, Result};
use chacha20::cipher::{StreamCipher, StreamCipherSeek};
use digest::Digest;
use hmac::{Hmac, Mac};
use md5::Md5;
use pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding};
use poly1305::Poly1305;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

type Aes192Gcm = AesGcm<aes::Aes192, U12>;
type HmacSha256 = Hmac<Sha256>;
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

struct OsGetRandom;

impl rand_core::RngCore for OsGetRandom {
    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        self.fill_bytes(&mut buf);
        u32::from_le_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill_bytes(&mut buf);
        u64::from_le_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.try_fill_bytes(dest)
            .expect("OS random source unavailable");
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> std::result::Result<(), rand_core::Error> {
        getrandom::fill(dest).map_err(|_| {
            // Non-zero custom error code reserved for OS RNG failures.
            rand_core::Error::from(
                std::num::NonZeroU32::new(rand_core::Error::CUSTOM_START).unwrap(),
            )
        })
    }
}

impl rand_core::CryptoRng for OsGetRandom {}

pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| CryptoBackendError)?;
    Ok(b)
}

pub(crate) fn sha256_array(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

pub(crate) fn md5_array(b: &[u8]) -> [u8; 16] {
    Md5::digest(b).into()
}

pub(crate) fn hmac_sha256(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    ensure!(!key.is_empty(), CryptoBackendError);
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| CryptoBackendError)?;
    mac.update(input);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(crate) fn hmac_sha256_slices(key: &[u8], parts: &[&[u8]]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(!key.is_empty(), CryptoBackendError);
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).map_err(|_| CryptoBackendError)?;
    for part in parts {
        mac.update(part);
    }
    Ok(Zeroizing::new(mac.finalize().into_bytes().to_vec()))
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub(crate) fn gcm_seal(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    plain: &[u8],
) -> Result<(Vec<u8>, [u8; 16])> {
    ensure!(matches!(key.len(), 16 | 24), "Invalid GCM key");
    ensure!(iv.len() == 12, "Invalid GCM nonce");
    let nonce = Nonce::from_slice(iv);
    let mut out = plain.to_vec();
    let result = match key.len() {
        16 => {
            let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| CryptoBackendError)?;
            cipher.encrypt_in_place_detached(nonce, aad, &mut out)
        }
        24 => {
            let cipher = Aes192Gcm::new_from_slice(key).map_err(|_| CryptoBackendError)?;
            cipher.encrypt_in_place_detached(nonce, aad, &mut out)
        }
        _ => unreachable!(),
    };
    let tag = match result {
        Ok(tag) => tag,
        Err(_) => {
            out.zeroize();
            return Err(CryptoBackendError.into());
        }
    };
    Ok((out, tag.into()))
}

pub(crate) fn gcm_open(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    encrypted: &[u8],
    tag: &[u8],
) -> Result<Vec<u8>> {
    ensure!(matches!(key.len(), 16 | 24), "Invalid GCM key");
    ensure!(iv.len() == 12 && tag.len() == 16, "Invalid GCM envelope");
    let nonce = Nonce::from_slice(iv);
    let tag = Tag::from_slice(tag);
    let mut out = encrypted.to_vec();
    let result = match key.len() {
        16 => {
            let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| CryptoBackendError)?;
            cipher.decrypt_in_place_detached(nonce, aad, &mut out, tag)
        }
        24 => {
            let cipher = Aes192Gcm::new_from_slice(key).map_err(|_| CryptoBackendError)?;
            cipher.decrypt_in_place_detached(nonce, aad, &mut out, tag)
        }
        _ => unreachable!(),
    };
    if result.is_err() {
        out.zeroize();
        return Err(CryptoBackendError.into());
    }
    Ok(out)
}

pub(crate) fn aes_128_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Result<Vec<u8>> {
    let enc = Aes128CbcEnc::new_from_slices(key, iv).map_err(|_| CryptoBackendError)?;
    Ok(enc.encrypt_padded_vec_mut::<Pkcs7>(plain))
}

pub(crate) fn aes_128_cbc_decrypt(
    key: &[u8; 16],
    iv: &[u8; 16],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    ensure!(
        !ciphertext.is_empty() && ciphertext.len().is_multiple_of(16),
        CryptoBackendError
    );
    let mut dec = Aes128CbcDec::new_from_slices(key, iv).map_err(|_| CryptoBackendError)?;
    let mut buf = ciphertext.to_vec();
    for chunk in buf.chunks_exact_mut(16) {
        dec.decrypt_block_mut(chunk.into());
    }
    let last_offset = buf.len() - 16;
    let keep = match unpad_pkcs7_block((&buf[last_offset..]).try_into()?) {
        Ok(n) => n,
        Err(e) => {
            buf.zeroize();
            return Err(e);
        }
    };
    buf.truncate(last_offset + keep);
    Ok(buf)
}

fn unpad_pkcs7_block(block: &[u8; 16]) -> Result<usize> {
    let pad = block[15];
    let in_range = (1u8..=16u8).fold(subtle::Choice::from(0u8), |acc, v| acc | pad.ct_eq(&v));
    let start = 16u16.wrapping_sub(u16::from(pad));
    let mut valid = in_range;
    for (i, &b) in block.iter().enumerate() {
        let lt = subtle::Choice::from(((i as u16).wrapping_sub(start) >> 15) as u8);
        let is_pad_byte = !lt;
        valid &= (!is_pad_byte) | b.ct_eq(&pad);
    }
    if bool::from(valid) {
        Ok(16 - usize::from(pad))
    } else {
        Err(CryptoBackendError.into())
    }
}

pub(crate) fn aes_128_ecb_encrypt(key: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let cipher = aes::Aes128::new_from_slice(key).map_err(|_| CryptoBackendError)?;
    let pad = 16 - (plain.len() % 16);
    let mut out = Vec::with_capacity(plain.len() + pad);
    out.extend_from_slice(plain);
    out.resize(plain.len() + pad, pad as u8);
    for chunk in out.chunks_exact_mut(16) {
        cipher.encrypt_block(chunk.into());
    }
    Ok(out)
}

pub(crate) fn aes_128_ecb_decrypt(key: &[u8], encrypted: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !encrypted.is_empty() && encrypted.len().is_multiple_of(16),
        CryptoBackendError
    );
    let cipher = aes::Aes128::new_from_slice(key).map_err(|_| CryptoBackendError)?;
    let mut out = encrypted.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.decrypt_block(chunk.into());
    }
    let last_offset = out.len() - 16;
    let keep = match unpad_pkcs7_block((&out[last_offset..]).try_into()?) {
        Ok(n) => n,
        Err(e) => {
            out.zeroize();
            return Err(e);
        }
    };
    out.truncate(last_offset + keep);
    Ok(out)
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn aes_128_ecb_encrypt_no_pad(key: &[u8], blocks: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(blocks.len().is_multiple_of(16), CryptoBackendError);
    let cipher = aes::Aes128::new_from_slice(key).map_err(|_| CryptoBackendError)?;
    let mut out = Zeroizing::new(blocks.to_vec());
    for chunk in out.chunks_exact_mut(16) {
        cipher.encrypt_block(chunk.into());
    }
    Ok(out)
}

pub(crate) struct Aes128EcbDecryptor {
    cipher: aes::Aes128,
    pending: [u8; 16],
    pending_len: usize,
    held: Option<[u8; 16]>,
    finalized: bool,
}

impl Drop for Aes128EcbDecryptor {
    fn drop(&mut self) {
        self.pending.zeroize();
        if let Some(ref mut block) = self.held {
            block.zeroize();
        }
    }
}

impl Aes128EcbDecryptor {
    pub(crate) fn new(key: &[u8]) -> Result<Self> {
        let cipher = aes::Aes128::new_from_slice(key).map_err(|_| CryptoBackendError)?;
        Ok(Self {
            cipher,
            pending: [0u8; 16],
            pending_len: 0,
            held: None,
            finalized: false,
        })
    }

    pub(crate) fn update(&mut self, input: &[u8], out: &mut [u8]) -> Result<usize> {
        ensure!(!self.finalized, CryptoBackendError);
        if input.is_empty() {
            return Ok(0);
        }
        let total = self
            .pending_len
            .checked_add(input.len())
            .ok_or(CryptoBackendError)?;
        let completed = total / 16;
        let rem_len = total % 16;
        // Hold back only the final complete block when no subsequent bytes have
        // arrived yet; any trailing byte proves every completed block precedes EOF.
        let mut emitted_blocks = usize::from(self.held.is_some());
        if completed > 0 {
            emitted_blocks += if rem_len > 0 {
                completed
            } else {
                completed - 1
            };
        }
        ensure!(out.len() >= emitted_blocks * 16, CryptoBackendError);

        let mut written = 0usize;
        if let Some(mut prev) = self.held.take() {
            self.cipher.decrypt_block((&mut prev).into());
            out[written..written + 16].copy_from_slice(&prev);
            prev.zeroize();
            written += 16;
        }
        if completed == 0 {
            self.pending[self.pending_len..self.pending_len + input.len()].copy_from_slice(input);
            self.pending_len += input.len();
            return Ok(written);
        }

        let mut cursor = input;
        if self.pending_len > 0 {
            let need = 16 - self.pending_len;
            self.pending[self.pending_len..16].copy_from_slice(&cursor[..need]);
            cursor = &cursor[need..];
            self.pending_len = 0;
            let block = self.pending;
            self.pending.zeroize();
            self.held = Some(block);
        }
        let mut chunks = cursor.chunks_exact(16);
        for chunk in &mut chunks {
            let block: [u8; 16] = chunk.try_into()?;
            if let Some(mut prev) = self.held.replace(block) {
                self.cipher.decrypt_block((&mut prev).into());
                out[written..written + 16].copy_from_slice(&prev);
                prev.zeroize();
                written += 16;
            }
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            if let Some(mut prev) = self.held.take() {
                self.cipher.decrypt_block((&mut prev).into());
                out[written..written + 16].copy_from_slice(&prev);
                prev.zeroize();
                written += 16;
            }
            self.pending[..rem.len()].copy_from_slice(rem);
            self.pending_len = rem.len();
        }
        Ok(written)
    }

    pub(crate) fn finalize(&mut self, out: &mut [u8]) -> Result<usize> {
        ensure!(!self.finalized, CryptoBackendError);
        self.finalized = true;
        ensure!(self.pending_len == 0, CryptoBackendError);
        let Some(mut last) = self.held.take() else {
            return Err(CryptoBackendError.into());
        };
        self.cipher.decrypt_block((&mut last).into());
        let plain_len = match unpad_pkcs7_block(&last) {
            Ok(n) => n,
            Err(e) => {
                last.zeroize();
                return Err(e);
            }
        };
        if out.len() < plain_len {
            last.zeroize();
            return Err(CryptoBackendError.into());
        }
        out[..plain_len].copy_from_slice(&last[..plain_len]);
        last.zeroize();
        Ok(plain_len)
    }
}

pub(crate) struct Md5Hasher(Md5);

impl Md5Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(Md5::new()))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        self.0.update(data);
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 16]> {
        Ok(self.0.finalize_reset().into())
    }
}

pub(crate) struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(Sha256::new()))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        self.0.update(data);
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 32]> {
        Ok(self.0.finalize_reset().into())
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
    // OpenSSL EVP_chacha20 interprets the 16-byte IV as a 32-bit LE block
    // counter followed by a 96-bit nonce (RFC 8439).
    let block_counter = u32::from_le_bytes(iv[..4].try_into()?);
    let mut cipher = chacha20::ChaCha20::new(key.into(), (&iv[4..16]).into());
    cipher.seek(u64::from(block_counter) * 64);
    let mut out = Zeroizing::new(input.to_vec());
    cipher
        .try_apply_keystream(&mut out)
        .map_err(|_| CryptoBackendError)?;
    Ok(out)
}

pub(crate) fn poly1305_mac(key: &[u8; 32], parts: &[&[u8]]) -> Result<[u8; 16]> {
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let mut message = Zeroizing::new(Vec::with_capacity(total));
    for part in parts {
        message.extend_from_slice(part);
    }
    let tag = Poly1305::new(key.into()).compute_unpadded(&message);
    Ok(tag.into())
}

pub(crate) fn generate_rsa_1024_public_pem() -> Result<Vec<u8>> {
    let mut rng = OsGetRandom;
    let private = rsa::RsaPrivateKey::new(&mut rng, 1024).map_err(|_| CryptoBackendError)?;
    let public = rsa::RsaPublicKey::from(&private);
    let pem = public
        .to_public_key_pem(LineEnding::LF)
        .map_err(|_| CryptoBackendError)?;
    Ok(pem.into_bytes())
}

pub(crate) enum Ecdh {
    P224(p224::SecretKey),
    P256(p256::SecretKey),
}

impl Ecdh {
    pub(crate) fn generate(curve: Curve) -> Result<Self> {
        let mut rng = OsGetRandom;
        Ok(match curve {
            Curve::P224 => Self::P224(p224::SecretKey::random(&mut rng)),
            Curve::P256 => Self::P256(p256::SecretKey::random(&mut rng)),
        })
    }

    pub(crate) fn from_private(curve: Curve, private: &[u8]) -> Result<Self> {
        ensure!(!private.is_empty(), CryptoBackendError);
        match curve {
            Curve::P224 => {
                ensure!(private.len() <= 28, CryptoBackendError);
                let mut padded = Zeroizing::new([0u8; 28]);
                padded[28 - private.len()..].copy_from_slice(private);
                let key = p224::SecretKey::from_slice(&*padded).map_err(|_| CryptoBackendError)?;
                Ok(Self::P224(key))
            }
            Curve::P256 => {
                ensure!(private.len() <= 32, CryptoBackendError);
                let mut padded = Zeroizing::new([0u8; 32]);
                padded[32 - private.len()..].copy_from_slice(private);
                let key = p256::SecretKey::from_slice(&*padded).map_err(|_| CryptoBackendError)?;
                Ok(Self::P256(key))
            }
        }
    }

    pub(crate) fn public(&self) -> Result<Vec<u8>> {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        Ok(match self {
            Self::P224(key) => key.public_key().to_encoded_point(false).as_bytes().to_vec(),
            Self::P256(key) => key.public_key().to_encoded_point(false).as_bytes().to_vec(),
        })
    }

    pub(crate) fn derive(&self, point: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::P224(key) => {
                let peer =
                    p224::PublicKey::from_sec1_bytes(point).map_err(|_| CryptoBackendError)?;
                let shared = p224::ecdh::diffie_hellman(key.to_nonzero_scalar(), peer.as_affine());
                Ok(shared.raw_secret_bytes().to_vec())
            }
            Self::P256(key) => {
                let peer =
                    p256::PublicKey::from_sec1_bytes(point).map_err(|_| CryptoBackendError)?;
                let shared = p256::ecdh::diffie_hellman(key.to_nonzero_scalar(), peer.as_affine());
                Ok(shared.raw_secret_bytes().to_vec())
            }
        }
    }
}

pub(crate) fn verify_signature(pem: &str, data: &[u8], signature: &[u8]) -> Result<()> {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    let key = VerifyingKey::from_public_key_pem(pem)
        .map_err(|_| anyhow::anyhow!("Unsupported signature pin curve"))?;
    let sig = Signature::from_der(signature).map_err(|_| CryptoBackendError)?;
    // Accept both low-S and high-S DER ECDSA signatures, matching OpenSSL's
    // ECDSA_verify behavior against pinned server certificates.
    let normalized = sig.normalize_s().unwrap_or(sig);
    key.verify(data, &normalized)
        .map_err(|_| anyhow::anyhow!("Pinned server signature verification failed"))?;
    Ok(())
}

#[cfg(test)]
pub(crate) struct TestP256Signer {
    key: p256::ecdsa::SigningKey,
}

#[cfg(test)]
impl TestP256Signer {
    pub(crate) fn generate() -> Result<Self> {
        let mut rng = OsGetRandom;
        Ok(Self {
            key: p256::ecdsa::SigningKey::random(&mut rng),
        })
    }

    pub(crate) fn public_pem(&self) -> Result<String> {
        self.key
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| CryptoBackendError.into())
    }

    pub(crate) fn sign_sha256(&self, data: &[u8]) -> Result<Vec<u8>> {
        use p256::ecdsa::{signature::Signer, DerSignature};
        let sig: DerSignature = self.key.sign(data);
        Ok(sig.as_bytes().to_vec())
    }
}
