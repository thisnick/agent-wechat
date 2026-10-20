//! Business encryption is separate from authenticated MMTLS records.
use crate::{crypto::*, protobuf as p, MAX_MESSAGE_BYTES};
use anyhow::{bail, ensure, Result};
use openssl::{nid::Nid, symm::Mode};
use zeroize::Zeroize;

pub struct PublicProfile {
    pub version: u16,
    pub point: Vec<u8>,
    pub signature_pem: String,
}
pub fn profile_for_region(region: u32) -> Result<PublicProfile> {
    let version = match region {
        1 => 10014,
        2 => 50005,
        _ => bail!("Unsupported business certificate region"),
    };
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/business-public-keys.json"))?;
    let pin = pins["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["certVersion"].as_u64() == Some(version))
        .ok_or_else(|| anyhow::anyhow!("Missing business pin"))?;
    Ok(PublicProfile {
        version: version as u16,
        point: hex::decode(pin["ecdhPointHex"].as_str().unwrap())?,
        signature_pem: pin["ecdsaPEM"].as_str().unwrap().to_string(),
    })
}
pub struct Prelogin {
    pub request: Vec<u8>,
    local: Ecdh,
    plain: Vec<u8>,
    derived: Vec<u8>,
    encrypted_payload: Vec<u8>,
    signature_pem: String,
    has_auth_key: bool,
}
impl Drop for Prelogin {
    fn drop(&mut self) {
        self.plain.zeroize();
        self.derived.zeroize();
    }
}
impl Prelogin {
    pub fn prepare(plain: &[u8], profile: &PublicProfile, auth_key: &[u8]) -> Result<Self> {
        ensure!(
            plain.len() <= MAX_MESSAGE_BYTES,
            "Prelogin plaintext too large"
        );
        ensure!(
            profile.point.len() == 65 && profile.point[0] == 4,
            "Invalid business pin"
        );
        ensure!(
            auth_key.is_empty() || auth_key.len() >= 24,
            "Invalid saved Hybrid auth key"
        );
        let local = Ecdh::generate(Nid::X9_62_PRIME256V1)?;
        let point = local.public()?;
        let prefix = [b"1415".as_slice(), &point].concat();
        let aad = sha256(&prefix);
        let mut shared = sha256(&local.derive(&profile.point)?);
        let mut random32 = random::<32>()?;
        let encrypted_random = envelope_seal(&random32, &shared[..24], &aad, random()?)?;
        shared.zeroize();
        let auth_block = if auth_key.is_empty() {
            vec![]
        } else {
            envelope_seal(&random32, &auth_key[..24], &aad, random()?)?
        };
        let mut extracted = hmac(b"security hdkf expand", &random32)?;
        let derived = expand(&extracted, &aad, 56)?;
        random32.zeroize();
        extracted.zeroize();
        let payload_aad = sha256(&[prefix, encrypted_random.clone(), auth_block.clone()].concat());
        let encrypted_payload = envelope_seal(plain, &derived[..24], &payload_aad, random()?)?;
        let request = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::message(&[p::uint(1, 415), p::bytes(2, &point)])?),
            p::bytes(3, &encrypted_random),
            p::bytes(4, &auth_block),
            p::bytes(5, &encrypted_payload),
        ])?;
        Ok(Self {
            request,
            local,
            plain: plain.to_vec(),
            derived,
            encrypted_payload,
            signature_pem: profile.signature_pem.clone(),
            has_auth_key: !auth_key.is_empty(),
        })
    }
    pub fn decrypt_reply(&self, reply: &[u8]) -> Result<Vec<u8>> {
        let f = p::decode(reply)?;
        let key = p::decode(p::data(&f, 1)?)?;
        ensure!(p::scalar(&key, 1)? == 415, "Unsupported business curve");
        let point = p::data(&key, 2)?;
        let credential = p::scalar(&f, 2)?;
        ensure!(
            point.len() == 65
                && point[0] == 4
                && [1, 2].contains(&credential)
                && (credential != 2 || self.has_auth_key),
            "Unsupported business credential"
        );
        let ciphertext = p::data(&f, 3)?;
        if credential == 1 {
            verify_signature(
                &self.signature_pem,
                &sha256(&[self.encrypted_payload.as_slice(), ciphertext].concat()),
                p::data(&f, 4)?,
            )?;
        }
        let mut shared = sha256(&self.local.derive(point)?);
        let aad = sha256(
            &[
                &self.derived[24..],
                &self.plain,
                b"415",
                point,
                credential.to_string().as_bytes(),
            ]
            .concat(),
        );
        let plain = envelope_open(ciphertext, &shared[..24], &aad);
        shared.zeroize();
        plain
    }
}
pub struct PacketPolicy<'a> {
    pub client_version: u32,
    pub uin: u32,
    pub cookie: &'a [u8],
    pub cgi: u16,
    pub original_length: usize,
    pub pre_encryption_length: usize,
    pub key_version: u16,
    pub cert_version: u16,
    pub checksum: u32,
    pub compression: u8,
    pub encryption: u8,
    pub notice_id: u32,
    pub sequence: u16,
}
pub fn pack(policy: PacketPolicy<'_>, encrypted: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        policy.cookie.len() <= 15 && policy.compression <= 3 && policy.encryption <= 15,
        "Invalid business policy"
    );
    ensure!(
        policy.original_length <= MAX_MESSAGE_BYTES
            && policy.pre_encryption_length <= MAX_MESSAGE_BYTES,
        "Business payload too large"
    );
    let tail = [
        policy.client_version.to_be_bytes().to_vec(),
        policy.uin.to_be_bytes().to_vec(),
        policy.cookie.to_vec(),
        p::varint(policy.cgi as u64),
        p::varint(policy.original_length as u64),
        p::varint(policy.pre_encryption_length as u64),
        p::varint(policy.key_version as u64),
        p::varint(policy.cert_version as u64),
        p::varint(policy.checksum as u64),
        vec![0xff],
        p::varint(policy.notice_id as u64),
        vec![0, 0],
        p::varint(0),
        policy.sequence.to_be_bytes().to_vec(),
    ]
    .concat();
    let length = tail.len() + 3;
    ensure!(length <= 63, "Unsupported extended business header");
    let flags = ((length as u16) << 2)
        | policy.compression as u16
        | ((policy.cookie.len() as u16) << 8)
        | ((policy.encryption as u16) << 12);
    Ok([
        vec![0xbf],
        flags.to_le_bytes().to_vec(),
        tail,
        encrypted.to_vec(),
    ]
    .concat())
}
pub fn prelogin_packet(
    prelogin: &Prelogin,
    cgi: u16,
    key_version: u16,
    cookie: &[u8],
) -> Result<Vec<u8>> {
    pack(
        PacketPolicy {
            client_version: 0,
            uin: 0,
            cookie,
            cgi,
            original_length: prelogin.plain.len(),
            pre_encryption_length: prelogin.plain.len(),
            key_version,
            cert_version: 0,
            checksum: 0,
            compression: 2,
            encryption: 12,
            notice_id: crate::notice::notice_id(&prelogin.request)?,
            sequence: u16::from_be_bytes(random()?),
        },
        &prelogin.request,
    )
}
pub struct Envelope {
    pub return_code: i32,
    pub cgi: u64,
    pub original_length: usize,
    pub pre_encryption_length: usize,
    pub compression: u8,
    pub encryption: u8,
    pub cookie: Vec<u8>,
    pub payload: Vec<u8>,
}
pub fn split(input: &[u8]) -> Result<Envelope> {
    ensure!(input.len() >= 10, "Truncated business response");
    let base = usize::from(input[0] == 0xbf);
    let flags = u16::from_le_bytes(input[base..base + 2].try_into()?);
    let header = ((flags >> 2) & 63) as usize;
    let cookie_len = ((flags >> 8) & 15) as usize;
    let cookie_offset = base + 10;
    ensure!(
        header >= cookie_offset + cookie_len && header <= input.len(),
        "Invalid business boundary"
    );
    let return_code = i32::from_be_bytes(input[base + 2..base + 6].try_into()?);
    let mut offset = cookie_offset + cookie_len;
    let cgi = p::read_varint(&input[..header], &mut offset)?;
    let original_length = usize::try_from(p::read_varint(&input[..header], &mut offset)?)?;
    let pre_encryption_length = usize::try_from(p::read_varint(&input[..header], &mut offset)?)?;
    p::read_varint(&input[..header], &mut offset)?;
    p::read_varint(&input[..header], &mut offset)?;
    if base == 0 {
        ensure!(offset == header, "Unknown business response extension");
    }
    ensure!(
        original_length <= MAX_MESSAGE_BYTES && pre_encryption_length <= MAX_MESSAGE_BYTES,
        "Response too large"
    );
    Ok(Envelope {
        return_code,
        cgi,
        original_length,
        pre_encryption_length,
        compression: (flags & 3) as u8,
        encryption: (flags >> 12) as u8,
        cookie: input[cookie_offset..cookie_offset + cookie_len].to_vec(),
        payload: input[header..].to_vec(),
    })
}
pub fn checksum(uin: u32, payload: &[u8]) -> u32 {
    let mut input = (payload.len() as u32).to_be_bytes().to_vec();
    input.extend(md5(&uin.to_be_bytes()));
    let prefix = md5(&input);
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in prefix.iter().chain(payload) {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
pub struct AuthenticatedPacket {
    pub packet: Vec<u8>,
    cgi: u16,
    mode: u8,
    key: Vec<u8>,
}
impl Drop for AuthenticatedPacket {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
impl AuthenticatedPacket {
    pub fn prepare(state: &crate::AccountState, cgi: u16, plain: &[u8]) -> Result<Self> {
        state.validate()?;
        ensure!(
            plain.len() <= MAX_MESSAGE_BYTES,
            "Authenticated request too large"
        );
        let mode = if !state.request_key.is_empty() && !state.response_key.is_empty() {
            13
        } else {
            5
        };
        let zipped = if mode == 5 {
            deflate(plain)?
        } else {
            plain.to_vec()
        };
        let compressed = mode == 5 && zipped.len() < plain.len();
        let body = if compressed { zipped.as_slice() } else { plain };
        let key = if mode == 13 {
            ensure!(
                state.request_key.len() == 24 && state.response_key.len() == 24,
                "Invalid main session keys"
            );
            state.response_key.clone()
        } else {
            ensure!(state.session_key.len() == 16, "Invalid origin session key");
            state.session_key.clone()
        };
        let encrypted = if mode == 13 {
            envelope_seal(body, &state.request_key, b"", random()?)?
        } else {
            cbc(body, &key, Mode::Encrypt)?
        };
        let packet = pack(
            PacketPolicy {
                client_version: state.device.client_version as u32,
                uin: state.uin,
                cookie: &state.cookie,
                cgi,
                original_length: plain.len(),
                pre_encryption_length: body.len(),
                key_version: 0,
                cert_version: 39,
                checksum: checksum(state.uin, plain),
                compression: if compressed { 1 } else { 2 },
                encryption: mode,
                notice_id: crate::notice::notice_id(&encrypted)?,
                sequence: u16::from_be_bytes(random()?),
            },
            &encrypted,
        )?;
        Ok(Self {
            packet,
            cgi,
            mode,
            key,
        })
    }
    /// Call only on the plaintext returned by the authenticated MMTLS transport.
    pub fn decode_verified_transport_reply(&self, input: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let envelope = split(input)?;
        ensure!(
            envelope.return_code == 0,
            "Native envelope returned {}",
            envelope.return_code
        );
        ensure!(
            envelope.cgi == self.cgi as u64
                && envelope.encryption == self.mode
                && [1, 2].contains(&envelope.compression),
            "Unsupported authenticated response policy"
        );
        let body = if self.mode == 13 {
            envelope_open(&envelope.payload, &self.key, b"")?
        } else {
            cbc(&envelope.payload, &self.key, Mode::Decrypt)?
        };
        ensure!(
            body.len() == envelope.pre_encryption_length,
            "Authenticated body length mismatch"
        );
        let plain = if envelope.compression == 1 {
            inflate(&body)?
        } else {
            body
        };
        ensure!(
            plain.len() == envelope.original_length,
            "Authenticated plaintext length mismatch"
        );
        Ok((plain, envelope.cookie))
    }
}
pub fn account_keys(inner: &Ecdh, auth: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let f = p::decode(auth)?;
    let flags = match p::optional(&f, 19)? {
        Some(p::Value::Varint(v)) => *v,
        None => 0,
        _ => bail!("Invalid auth flags"),
    };
    let raw = p::unbuffer(p::data(&f, 3)?)?;
    let session = if flags & 4 != 0 {
        let e = p::decode(p::data(&f, 2)?)?;
        ensure!(p::scalar(&e, 1)? == 713, "Unsupported account curve");
        let point = p::unbuffer(p::data(&e, 2)?)?;
        ensure!(point.len() == 57 && point[0] == 4, "Invalid account point");
        let mut secret = inner.derive(&point)?;
        ensure!(secret.len() == 28, "Invalid P224 secret");
        let mut key = md5(&secret);
        secret.zeroize();
        let result = cbc(&raw, &key, Mode::Decrypt);
        key.zeroize();
        result?
    } else {
        raw
    };
    ensure!(
        !session.is_empty() && session.len() <= 65536,
        "Invalid derived session key"
    );
    let get = |n| -> Result<Vec<u8>> {
        Ok(match p::optional_data(&f, n)? {
            Some(b) => p::unbuffer(b)?,
            None => vec![],
        })
    };
    Ok((session, get(23)?, get(24)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_checksum_vectors() {
        for (uin, payload, expected) in [
            (0, vec![], 0x4e970913),
            (1, b"a".to_vec(), 0x39d20759),
            (0x12345678, b"abc".to_vec(), 0x69430a30),
            (u32::MAX, (0..=255).collect(), 0xf74e868e),
        ] {
            assert_eq!(checksum(uin, &payload), expected);
        }
    }
    #[test]
    fn prelogin_profiles_validate() {
        for region in [1, 2] {
            let profile = profile_for_region(region).unwrap();
            assert!(
                Prelogin::prepare(b"synthetic", &profile, &[])
                    .unwrap()
                    .request
                    .len()
                    > 100
            );
        }
        assert!(profile_for_region(3).is_err());
    }
    #[test]
    fn encrypted_envelope_rejects_aad() {
        let key = [3; 24];
        let aad = sha256(b"a");
        let e = envelope_seal(b"synthetic", &key, &aad, [4; 12]).unwrap();
        assert_eq!(envelope_open(&e, &key, &aad).unwrap(), b"synthetic");
        assert!(envelope_open(&e, &key, &sha256(b"b")).is_err());
    }
    #[test]
    fn synthetic_signed_peer_and_saved_credential() {
        use openssl::{
            ec::{EcGroup, EcKey},
            hash::MessageDigest,
            pkey::PKey,
            sign::Signer,
        };
        let static_key = Ecdh::generate(Nid::X9_62_PRIME256V1).unwrap();
        let signing = PKey::from_ec_key(
            EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap(),
        )
        .unwrap();
        let profile = PublicProfile {
            version: 1,
            point: static_key.public().unwrap(),
            signature_pem: String::from_utf8(signing.public_key_to_pem().unwrap()).unwrap(),
        };
        for credential in [1, 2] {
            let auth = if credential == 2 {
                vec![11; 380]
            } else {
                vec![]
            };
            let request = b"synthetic login";
            let local = Prelogin::prepare(request, &profile, &auth).unwrap();
            let f = p::decode(&local.request).unwrap();
            let key = p::decode(p::data(&f, 2).unwrap()).unwrap();
            let point = p::data(&key, 2).unwrap();
            let prefix = [b"1415".as_slice(), point].concat();
            let aad = sha256(&prefix);
            let random = envelope_open(
                p::data(&f, 3).unwrap(),
                &sha256(&static_key.derive(point).unwrap())[..24],
                &aad,
            )
            .unwrap();
            let derived =
                expand(&hmac(b"security hdkf expand", &random).unwrap(), &aad, 56).unwrap();
            assert_eq!(
                envelope_open(
                    p::data(&f, 5).unwrap(),
                    &derived[..24],
                    &sha256(
                        &[
                            prefix,
                            p::data(&f, 3).unwrap().to_vec(),
                            p::data(&f, 4).unwrap().to_vec()
                        ]
                        .concat()
                    )
                )
                .unwrap(),
                request
            );
            if credential == 2 {
                assert_eq!(
                    envelope_open(p::data(&f, 4).unwrap(), &auth[..24], &aad).unwrap(),
                    random
                );
            }
            let ephemeral = Ecdh::generate(Nid::X9_62_PRIME256V1).unwrap();
            let server_point = ephemeral.public().unwrap();
            let response_aad = sha256(
                &[
                    &derived[24..],
                    request,
                    b"415",
                    &server_point,
                    credential.to_string().as_bytes(),
                ]
                .concat(),
            );
            let ciphertext = envelope_seal(
                b"synthetic authenticated reply",
                &sha256(&ephemeral.derive(point).unwrap())[..24],
                &response_aad,
                [4; 12],
            )
            .unwrap();
            let mut fields = vec![
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 415), p::bytes(2, &server_point)]).unwrap(),
                ),
                p::uint(2, credential),
                p::bytes(3, &ciphertext),
            ];
            if credential == 1 {
                let mut signer = Signer::new(MessageDigest::sha256(), &signing).unwrap();
                signer
                    .update(&sha256(&[p::data(&f, 5).unwrap(), &ciphertext].concat()))
                    .unwrap();
                fields.push(p::bytes(4, &signer.sign_to_vec().unwrap()));
            }
            let response = p::message(&fields).unwrap();
            assert_eq!(
                local.decrypt_reply(&response).unwrap(),
                b"synthetic authenticated reply"
            );
            let mut corrupt = response;
            let n = corrupt.len();
            corrupt[n - 1] ^= 1;
            assert!(local.decrypt_reply(&corrupt).is_err());
        }
    }
}
