//! Cryptographic primitives shared by MMTLS, business envelopes, media transfers
//! and call packets.
//!
//! By default (`openssl-crypto`), primitives are backed by vendored OpenSSL.
//! Enabling `pure-rust-crypto` with `default-features = false` switches to
//! pure-Rust implementations with no OpenSSL build dependency. If both
//! `openssl-crypto` and `pure-rust-crypto` are enabled via Cargo feature
//! unification (for example, when default features stay enabled or another
//! crate in the dependency graph enables `openssl-crypto`), `openssl-crypto`
//! takes precedence.
use crate::MAX_MESSAGE_BYTES;
use anyhow::{ensure, Result};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::{
    fmt,
    io::{Read, Write},
};
use zeroize::{Zeroize, Zeroizing};

#[cfg(not(any(feature = "openssl-crypto", feature = "pure-rust-crypto")))]
compile_error!(
    "wechat-protocol requires at least one crypto backend: enable `openssl-crypto` or `pure-rust-crypto`"
);

#[cfg(feature = "openssl-crypto")]
mod openssl_backend;

#[cfg(all(
    feature = "pure-rust-crypto",
    any(test, not(feature = "openssl-crypto"))
))]
mod pure_rust_backend;

#[cfg(feature = "openssl-crypto")]
use openssl_backend as active;

#[cfg(all(feature = "pure-rust-crypto", not(feature = "openssl-crypto")))]
use pure_rust_backend as active;

#[cfg(all(test, feature = "openssl-crypto", feature = "pure-rust-crypto"))]
mod parity_tests;

/// Marker error emitted by the active cryptographic backend without exposing
/// key material or backend-specific error details.
#[derive(Debug)]
pub(crate) struct CryptoBackendError;

impl fmt::Display for CryptoBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cryptographic backend operation failed")
    }
}

impl std::error::Error for CryptoBackendError {}

/// Named elliptic curves used by WeChat transport and authentication key
/// agreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// NIST P-224 (`secp224r1`), used for account authentication key agreement.
    P224,
    /// NIST P-256 (`prime256v1`), used for MMTLS and HybridECDH prelogin.
    P256,
}

/// Generate `N` cryptographically secure random bytes.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    active::random()
}

/// Compute the SHA-256 digest of `b`.
pub fn sha256(b: &[u8]) -> Vec<u8> {
    active::sha256_array(b).to_vec()
}

pub(crate) fn sha256_array(b: &[u8]) -> [u8; 32] {
    active::sha256_array(b)
}

/// Compute the MD5 digest of `b`.
pub fn md5(b: &[u8]) -> Vec<u8> {
    active::md5_array(b).to_vec()
}

pub(crate) fn md5_array(b: &[u8]) -> [u8; 16] {
    active::md5_array(b)
}

/// Compute HMAC-SHA256 over `input` using `key`.
pub fn hmac(key: &[u8], input: &[u8]) -> Result<Vec<u8>> {
    active::hmac_sha256(key, input)
}

pub(crate) fn hmac_sha256_slices(key: &[u8], parts: &[&[u8]]) -> Result<Zeroizing<Vec<u8>>> {
    active::hmac_sha256_slices(key, parts)
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    active::constant_time_eq(a, b)
}

/// HKDF-Expand (RFC 5869) using HMAC-SHA256.
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

/// Compress `plain` with zlib within [`MAX_MESSAGE_BYTES`].
pub fn deflate(plain: &[u8]) -> Result<Vec<u8>> {
    ensure!(plain.len() <= MAX_MESSAGE_BYTES, "Plaintext too large");
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(plain)?;
    Ok(e.finish()?)
}

/// Decompress zlib data, enforcing [`MAX_MESSAGE_BYTES`] and rejecting trailing bytes.
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

/// Encrypt and authenticate `plain` with AES-128-GCM or AES-192-GCM.
pub fn gcm_seal(key: &[u8], iv: &[u8], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, [u8; 16])> {
    active::gcm_seal(key, iv, aad, plain)
}

/// Decrypt and authenticate `encrypted` with AES-128-GCM or AES-192-GCM.
pub fn gcm_open(
    key: &[u8],
    iv: &[u8],
    aad: &[u8],
    encrypted: &[u8],
    tag: &[u8],
) -> Result<Vec<u8>> {
    active::gcm_open(key, iv, aad, encrypted, tag)
}

/// Compress and seal a business payload using AES-192-GCM.
pub fn envelope_seal(plain: &[u8], key: &[u8], aad: &[u8], iv: [u8; 12]) -> Result<Vec<u8>> {
    ensure!(key.len() == 24, "Expected AES192 key");
    let (body, tag) = gcm_seal(key, &iv, aad, &deflate(plain)?)?;
    Ok([body, iv.to_vec(), tag.to_vec()].concat())
}

/// Open and decompress an AES-192-GCM business envelope.
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

fn normalized_cbc_key(key: &[u8]) -> Result<Zeroizing<[u8; 16]>> {
    ensure!(!key.is_empty() && key.len() <= 65536, "Invalid origin key");
    let mut normalized = Zeroizing::new([0u8; 16]);
    let n = key.len().min(16);
    normalized[..n].copy_from_slice(&key[..n]);
    Ok(normalized)
}

/// Encrypt `plain` with AES-128-CBC (PKCS#7 padding), normalizing `key` to 16
/// zero-padded bytes used as both key and IV.
pub fn cbc_encrypt(plain: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        plain.len() <= MAX_MESSAGE_BYTES + 65536,
        "CBC body too large"
    );
    let normalized = normalized_cbc_key(key)?;
    active::aes_128_cbc_encrypt(&normalized, &normalized, plain)
}

/// Decrypt `ciphertext` with AES-128-CBC (PKCS#7 padding), normalizing `key` to
/// 16 zero-padded bytes used as both key and IV.
pub fn cbc_decrypt(ciphertext: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        ciphertext.len() <= MAX_MESSAGE_BYTES + 65536,
        "CBC body too large"
    );
    ensure!(
        !ciphertext.is_empty() && ciphertext.len().is_multiple_of(16),
        "Invalid CBC ciphertext"
    );
    let normalized = normalized_cbc_key(key)?;
    active::aes_128_cbc_decrypt(&normalized, &normalized, ciphertext)
}

/// Encrypt or decrypt with AES-128-CBC (PKCS#7 padding) using OpenSSL's `Mode`.
/// Requires the `openssl-crypto` feature; portable callers can use
/// [`cbc_encrypt`] and [`cbc_decrypt`].
#[cfg(feature = "openssl-crypto")]
pub fn cbc(plain: &[u8], key: &[u8], mode: openssl::symm::Mode) -> Result<Vec<u8>> {
    match mode {
        openssl::symm::Mode::Encrypt => cbc_encrypt(plain, key),
        openssl::symm::Mode::Decrypt => cbc_decrypt(plain, key),
    }
}

pub(crate) fn aes_128_cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Result<Vec<u8>> {
    active::aes_128_cbc_encrypt(key, iv, plain)
}

pub(crate) fn aes_128_ecb_encrypt(key: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    ensure!(key.len() == 16, "Invalid CDN key");
    active::aes_128_ecb_encrypt(key, plain)
}

pub(crate) fn aes_128_ecb_decrypt(key: &[u8], encrypted: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        key.len() == 16 && !encrypted.is_empty() && encrypted.len().is_multiple_of(16),
        "Invalid CDN ciphertext"
    );
    active::aes_128_ecb_decrypt(key, encrypted)
}

pub(crate) fn aes_128_ecb_encrypt_no_pad(key: &[u8], blocks: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        key.len() == 16 && blocks.len().is_multiple_of(16),
        "Invalid CDN ciphertext"
    );
    active::aes_128_ecb_encrypt_no_pad(key, blocks)
}

pub(crate) struct Aes128EcbDecryptor(active::Aes128EcbDecryptor);

impl Aes128EcbDecryptor {
    pub(crate) fn new(key: &[u8]) -> Result<Self> {
        ensure!(key.len() == 16, "Invalid CDN key");
        Ok(Self(active::Aes128EcbDecryptor::new(key)?))
    }

    pub(crate) fn update(&mut self, input: &[u8], out: &mut [u8]) -> Result<usize> {
        self.0.update(input, out)
    }

    pub(crate) fn finalize(&mut self, out: &mut [u8]) -> Result<usize> {
        self.0.finalize(out)
    }
}

pub(crate) struct Md5Hasher(active::Md5Hasher);

impl Md5Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(active::Md5Hasher::new()?))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        self.0.update(data)
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 16]> {
        self.0.finish()
    }
}

pub(crate) struct Sha256Hasher(active::Sha256Hasher);

impl Sha256Hasher {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self(active::Sha256Hasher::new()?))
    }

    pub(crate) fn update(&mut self, data: &[u8]) -> Result<()> {
        self.0.update(data)
    }

    pub(crate) fn finish(&mut self) -> Result<[u8; 32]> {
        self.0.finish()
    }
}

pub(crate) fn chacha20_xor(
    key: &[u8; 32],
    nonce: [u8; 8],
    counter: u64,
    input: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    active::chacha20_xor(key, nonce, counter, input)
}

pub(crate) fn poly1305_mac(key: &[u8; 32], parts: &[&[u8]]) -> Result<[u8; 16]> {
    active::poly1305_mac(key, parts)
}

pub(crate) fn generate_rsa_1024_public_pem() -> Result<Vec<u8>> {
    active::generate_rsa_1024_public_pem()
}

/// Ephemeral or fixed-scalar ECDH key pair for NIST P-224 or P-256.
pub struct Ecdh {
    inner: active::Ecdh,
}

impl Ecdh {
    /// Generate a fresh ECDH key pair for `curve`.
    pub fn generate_for(curve: Curve) -> Result<Self> {
        Ok(Self {
            inner: active::Ecdh::generate(curve)?,
        })
    }

    /// Construct an ECDH key pair from a big-endian private scalar on `curve`.
    pub fn from_private_for(curve: Curve, private: &[u8]) -> Result<Self> {
        Ok(Self {
            inner: active::Ecdh::from_private(curve, private)?,
        })
    }

    /// Generate a fresh ECDH key pair using an OpenSSL curve `Nid`.
    /// Requires the `openssl-crypto` feature; portable callers can use
    /// [`Self::generate_for`].
    #[cfg(feature = "openssl-crypto")]
    pub fn generate(nid: openssl::nid::Nid) -> Result<Self> {
        Ok(Self {
            inner: openssl_backend::Ecdh::generate_nid(nid)?,
        })
    }

    /// Construct an ECDH key pair from a big-endian private scalar using an
    /// OpenSSL curve `Nid`. Requires the `openssl-crypto` feature; portable
    /// callers can use [`Self::from_private_for`].
    #[cfg(feature = "openssl-crypto")]
    pub fn from_private(nid: openssl::nid::Nid, private: &[u8]) -> Result<Self> {
        Ok(Self {
            inner: openssl_backend::Ecdh::from_private_nid(nid, private)?,
        })
    }

    /// Return the uncompressed SEC1 public point (`0x04 || x || y`).
    pub fn public(&self) -> Result<Vec<u8>> {
        self.inner.public()
    }

    /// Derive the fixed-width shared secret `x`-coordinate with a peer's SEC1 point.
    pub fn derive(&self, point: &[u8]) -> Result<Vec<u8>> {
        self.inner.derive(point)
    }
}

/// Verify an ASN.1 DER ECDSA-SHA256 signature against a pinned P-256 SPKI PEM
/// public key.
pub fn verify_signature(pem: &str, data: &[u8], signature: &[u8]) -> Result<()> {
    active::verify_signature(pem, data, signature)
}

#[cfg(test)]
pub(crate) struct TestP256Signer(active::TestP256Signer);

#[cfg(test)]
impl TestP256Signer {
    pub(crate) fn generate() -> Result<Self> {
        Ok(Self(active::TestP256Signer::generate()?))
    }

    pub(crate) fn public_pem(&self) -> Result<String> {
        self.0.public_pem()
    }

    pub(crate) fn sign_sha256(&self, data: &[u8]) -> Result<Vec<u8>> {
        self.0.sign_sha256(data)
    }
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
        for curve in [Curve::P224, Curve::P256] {
            let a = Ecdh::from_private_for(curve, &[1]).unwrap();
            let b = Ecdh::from_private_for(curve, &[2]).unwrap();
            assert_eq!(
                a.derive(&b.public().unwrap()).unwrap(),
                b.derive(&a.public().unwrap()).unwrap()
            );
        }
    }

    #[cfg(feature = "openssl-crypto")]
    #[test]
    fn openssl_ecdh_wrappers_keep_arbitrary_nid_and_leading_zero_scalar_compat() {
        use openssl::nid::Nid;
        let a = Ecdh::generate(Nid::SECP384R1).unwrap();
        let b = Ecdh::generate(Nid::SECP384R1).unwrap();
        assert_eq!(
            a.derive(&b.public().unwrap()).unwrap(),
            b.derive(&a.public().unwrap()).unwrap()
        );
        let mut padded_33 = [0u8; 33];
        padded_33[32] = 1;
        let wide = Ecdh::from_private(Nid::X9_62_PRIME256V1, &padded_33).unwrap();
        let narrow = Ecdh::from_private_for(Curve::P256, &[1]).unwrap();
        assert_eq!(wide.public().unwrap(), narrow.public().unwrap());
    }

    #[test]
    fn session_known_answer() {
        let key: Vec<u8> = (0..24).collect();
        let nonce = std::array::from_fn(|i| i as u8);
        let fixture = hex::decode("9e65093555920046fdfa90d437e2f0b922d46623768c7870b390381087bb0d11fa5af671c9b301db000102030405060708090a0b0d4ecbc764b639df9549f03800165ea6").unwrap();
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
