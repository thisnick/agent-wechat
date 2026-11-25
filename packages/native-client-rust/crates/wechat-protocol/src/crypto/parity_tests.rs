//! Deterministic cross-backend differential parity tests comparing OpenSSL and
//! pure-Rust implementations byte-for-byte across all protocol primitives.
use super::{openssl_backend as ossl, pure_rust_backend as pure, Curve};

#[test]
fn hashes_hmac_and_streaming_hashers_match_byte_for_byte() {
    let inputs: &[&[u8]] = &[
        b"",
        b"a",
        b"synthetic wechat payload",
        &[0xab; 64],
        &[0xcd; 1024 + 17],
    ];
    for input in inputs {
        assert_eq!(ossl::sha256_array(input), pure::sha256_array(input));
        assert_eq!(ossl::md5_array(input), pure::md5_array(input));

        let mut om = ossl::Md5Hasher::new().unwrap();
        let mut pm = pure::Md5Hasher::new().unwrap();
        let mut os = ossl::Sha256Hasher::new().unwrap();
        let mut ps = pure::Sha256Hasher::new().unwrap();
        for chunk in input.chunks(13) {
            om.update(chunk).unwrap();
            pm.update(chunk).unwrap();
            os.update(chunk).unwrap();
            ps.update(chunk).unwrap();
        }
        assert_eq!(om.finish().unwrap(), pm.finish().unwrap());
        assert_eq!(os.finish().unwrap(), ps.finish().unwrap());
    }

    assert!(ossl::hmac_sha256(b"", b"x").is_err());
    assert!(pure::hmac_sha256(b"", b"x").is_err());
    for key in [&b"short"[..], &[0x42u8; 32], &[0x99u8; 100]] {
        for input in inputs {
            assert_eq!(
                ossl::hmac_sha256(key, input).unwrap(),
                pure::hmac_sha256(key, input).unwrap()
            );
        }
        let parts: &[&[u8]] = &[b"domain", &[1, 2, 3, 4], b"suffix"];
        assert_eq!(
            *ossl::hmac_sha256_slices(key, parts).unwrap(),
            *pure::hmac_sha256_slices(key, parts).unwrap()
        );
    }
}

#[test]
fn aes_128_and_192_gcm_match_byte_for_byte() {
    let iv = [0x11u8; 12];
    let aad = b"synthetic aad";
    for key in [&[0x22u8; 16][..], &[0x33u8; 24][..]] {
        for plain in [
            &b""[..],
            &b"x"[..],
            &b"16-byte-exact!!!"[..],
            &[0x5a; 200][..],
        ] {
            let (oc, ot) = ossl::gcm_seal(key, &iv, aad, plain).unwrap();
            let (pc, pt) = pure::gcm_seal(key, &iv, aad, plain).unwrap();
            assert_eq!(oc, pc);
            assert_eq!(ot, pt);
            assert_eq!(ossl::gcm_open(key, &iv, aad, &pc, &pt).unwrap(), plain);
            assert_eq!(pure::gcm_open(key, &iv, aad, &oc, &ot).unwrap(), plain);

            let mut bad_tag = pt;
            bad_tag[0] ^= 1;
            assert!(ossl::gcm_open(key, &iv, aad, &pc, &bad_tag).is_err());
            assert!(pure::gcm_open(key, &iv, aad, &pc, &bad_tag).is_err());
        }
    }
    assert!(pure::gcm_seal(&[0u8; 32], &iv, aad, b"x").is_err());
    assert!(pure::gcm_seal(&[0u8; 16], &[0u8; 11], aad, b"x").is_err());
}

#[test]
fn aes_128_cbc_and_ecb_oneshot_and_streaming_match_byte_for_byte() {
    let key = *b"wxusrname2016cdn";
    let iv = [0x09u8; 16];
    for len in [0usize, 1, 15, 16, 17, 31, 32, 48, 255, 1024 + 7] {
        let plain: Vec<u8> = (0..len)
            .map(|i| (i.wrapping_mul(37) & 0xff) as u8)
            .collect();
        let o_cbc = ossl::aes_128_cbc_encrypt(&key, &iv, &plain).unwrap();
        let p_cbc = pure::aes_128_cbc_encrypt(&key, &iv, &plain).unwrap();
        assert_eq!(o_cbc, p_cbc);
        assert_eq!(ossl::aes_128_cbc_decrypt(&key, &iv, &p_cbc).unwrap(), plain);
        assert_eq!(pure::aes_128_cbc_decrypt(&key, &iv, &o_cbc).unwrap(), plain);

        let o_ecb = ossl::aes_128_ecb_encrypt(&key, &plain).unwrap();
        let p_ecb = pure::aes_128_ecb_encrypt(&key, &plain).unwrap();
        assert_eq!(o_ecb, p_ecb);
        assert_eq!(ossl::aes_128_ecb_decrypt(&key, &p_ecb).unwrap(), plain);
        assert_eq!(pure::aes_128_ecb_decrypt(&key, &o_ecb).unwrap(), plain);

        // Streaming ECB decrypt across misaligned chunk sizes.
        for step in [1usize, 7, 15, 16, 17, 64, o_ecb.len()] {
            let mut o_dec = ossl::Aes128EcbDecryptor::new(&key).unwrap();
            let mut p_dec = pure::Aes128EcbDecryptor::new(&key).unwrap();
            let mut o_out = Vec::new();
            let mut p_out = Vec::new();
            let mut o_buf = vec![0u8; step + 16];
            let mut p_buf = vec![0u8; step + 16];
            for chunk in o_ecb.chunks(step) {
                let on = o_dec.update(chunk, &mut o_buf).unwrap();
                let pn = p_dec.update(chunk, &mut p_buf).unwrap();
                assert_eq!(&o_buf[..on], &p_buf[..pn]);
                o_out.extend_from_slice(&o_buf[..on]);
                p_out.extend_from_slice(&p_buf[..pn]);
            }
            let on = o_dec.finalize(&mut o_buf).unwrap();
            let pn = p_dec.finalize(&mut p_buf).unwrap();
            assert_eq!(&o_buf[..on], &p_buf[..pn]);
            o_out.extend_from_slice(&o_buf[..on]);
            p_out.extend_from_slice(&p_buf[..pn]);
            assert_eq!(o_out, plain);
            assert_eq!(p_out, plain);
        }
    }

    // Bad PKCS#7 padding and truncated blocks fail in both backends.
    let zero_block = ossl::aes_128_ecb_encrypt_no_pad(&key, &[0u8; 16]).unwrap();
    assert_eq!(
        *zero_block,
        *pure::aes_128_ecb_encrypt_no_pad(&key, &[0u8; 16]).unwrap()
    );
    assert!(ossl::aes_128_ecb_decrypt(&key, &zero_block).is_err());
    assert!(pure::aes_128_ecb_decrypt(&key, &zero_block).is_err());

    let mut p_dec = pure::Aes128EcbDecryptor::new(&key).unwrap();
    let mut scratch = [0u8; 32];
    assert_eq!(p_dec.update(&zero_block[..15], &mut scratch).unwrap(), 0);
    assert!(p_dec.finalize(&mut scratch).is_err());
}

#[test]
fn ecdh_p224_and_p256_points_and_shared_secrets_match() {
    for curve in [Curve::P224, Curve::P256] {
        let scalars: &[&[u8]] = &[
            &[1],
            &[1, 123], // triggers leading-zero P-256 shared secret with scalar 1
            &[0x07; 28],
        ];
        for sa in scalars {
            let oa = ossl::Ecdh::from_private(curve, sa).unwrap();
            let pa = pure::Ecdh::from_private(curve, sa).unwrap();
            assert_eq!(oa.public().unwrap(), pa.public().unwrap());

            for sb in scalars {
                let ob = ossl::Ecdh::from_private(curve, sb).unwrap();
                let pb = pure::Ecdh::from_private(curve, sb).unwrap();
                let pub_b = ob.public().unwrap();
                assert_eq!(pub_b, pb.public().unwrap());
                assert_eq!(
                    oa.derive(&pub_b).unwrap(),
                    pa.derive(&pub_b).unwrap(),
                    "curve={curve:?}"
                );
            }
        }
        // Invalid scalars (empty, zero, >= curve order n, oversized) and invalid peer points rejected by both.
        let (over_order, oversized) = match curve {
            Curve::P224 => (vec![0xffu8; 28], [vec![0u8; 28], vec![1u8]].concat()),
            Curve::P256 => (vec![0xffu8; 32], [vec![0u8; 32], vec![1u8]].concat()),
        };
        for bad_scalar in [&b""[..], &[0][..], &over_order, &oversized] {
            assert!(ossl::Ecdh::from_private(curve, bad_scalar).is_err());
            assert!(pure::Ecdh::from_private(curve, bad_scalar).is_err());
        }

        let oa = ossl::Ecdh::from_private(curve, &[1]).unwrap();
        let pa = pure::Ecdh::from_private(curve, &[1]).unwrap();

        // Compressed SEC1 peer points must match identically across backends.
        let peer_uncompressed = ossl::Ecdh::from_private(curve, &[2])
            .unwrap()
            .public()
            .unwrap();
        let coord_len = match curve {
            Curve::P224 => 28,
            Curve::P256 => 32,
        };
        let mut peer_compressed = Vec::with_capacity(1 + coord_len);
        peer_compressed.push(if peer_uncompressed[1 + 2 * coord_len - 1] & 1 == 1 {
            0x03
        } else {
            0x02
        });
        peer_compressed.extend_from_slice(&peer_uncompressed[1..1 + coord_len]);
        assert_eq!(
            oa.derive(&peer_compressed).unwrap(),
            pa.derive(&peer_compressed).unwrap()
        );
        assert_eq!(
            oa.derive(&peer_compressed).unwrap(),
            pa.derive(&peer_uncompressed).unwrap()
        );

        let bad_uncompressed = [0x04]
            .into_iter()
            .chain(vec![0u8; 2 * coord_len])
            .collect::<Vec<_>>();
        for bad_point in [&b""[..], &[0x00][..], &[0x04, 1, 2][..], &bad_uncompressed] {
            assert!(oa.derive(bad_point).is_err());
            assert!(pa.derive(bad_point).is_err());
        }
    }
}

#[test]
fn ecdsa_p256_verification_matches_openssl_across_low_and_high_s_signatures() {
    use p256::ecdsa::Signature;

    // Deterministically construct both a low-S and a high-S DER signature from
    // the same signing key and message, and verify both backends accept both.
    let pure_signer = pure::TestP256Signer::generate().unwrap();
    let pure_pem = pure_signer.public_pem().unwrap();
    let base_der = pure_signer
        .sign_sha256(b"deterministic low-s and high-s")
        .unwrap();
    let base_sig = Signature::from_der(&base_der).unwrap();
    let low_sig = base_sig.normalize_s().unwrap_or(base_sig);
    let (r, low_s) = low_sig.split_scalars();
    let high_sig = Signature::from_scalars(*r, -*low_s).unwrap();
    assert!(low_sig.normalize_s().is_none());
    assert!(high_sig.normalize_s().is_some());
    assert_ne!(low_sig.to_der().as_bytes(), high_sig.to_der().as_bytes());
    for sig_der in [low_sig.to_der(), high_sig.to_der()] {
        ossl::verify_signature(
            &pure_pem,
            b"deterministic low-s and high-s",
            sig_der.as_bytes(),
        )
        .unwrap();
        pure::verify_signature(
            &pure_pem,
            b"deterministic low-s and high-s",
            sig_der.as_bytes(),
        )
        .unwrap();
    }

    // Exercise OpenSSL-generated signatures and tampered signature rejection.
    let ossl_signer = ossl::TestP256Signer::generate().unwrap();
    let ossl_pem = ossl_signer.public_pem().unwrap();
    for i in 0u8..16 {
        let msg = [b"synthetic transcript ".as_slice(), &[i]].concat();
        let sig = ossl_signer.sign_sha256(&msg).unwrap();
        ossl::verify_signature(&ossl_pem, &msg, &sig).unwrap();
        pure::verify_signature(&ossl_pem, &msg, &sig).unwrap();

        let mut bad = sig.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(ossl::verify_signature(&ossl_pem, &msg, &bad).is_err());
        assert!(pure::verify_signature(&ossl_pem, &msg, &bad).is_err());
    }

    // Non-P-256 PEMs (both RSA SPKI and P-224 EC SPKI) must be rejected by both backends.
    let rsa_pem = String::from_utf8(ossl::generate_rsa_1024_public_pem().unwrap()).unwrap();
    let p224_group = openssl::ec::EcGroup::from_curve_name(openssl::nid::Nid::SECP224R1).unwrap();
    let p224_key =
        openssl::pkey::PKey::from_ec_key(openssl::ec::EcKey::generate(&p224_group).unwrap())
            .unwrap();
    let p224_pem = String::from_utf8(p224_key.public_key_to_pem().unwrap()).unwrap();
    let dummy_sig = [0x30, 0x06, 0x02, 0x01, 1, 0x02, 0x01, 1];
    for non_p256_pem in [&rsa_pem, &p224_pem] {
        assert!(ossl::verify_signature(non_p256_pem, b"msg", &dummy_sig).is_err());
        assert!(pure::verify_signature(non_p256_pem, b"msg", &dummy_sig).is_err());
    }

    let sig = pure_signer.sign_sha256(b"cross-verify").unwrap();
    ossl::verify_signature(&pure_pem, b"cross-verify", &sig).unwrap();
    pure::verify_signature(&pure_pem, b"cross-verify", &sig).unwrap();

    // Verify all pinned fixture PEM keys parse identically in both backends.
    for fixture_json in [
        include_str!("../../fixtures/mmtls-public-keys.json"),
        include_str!("../../fixtures/business-public-keys.json"),
    ] {
        let pins: serde_json::Value = serde_json::from_str(fixture_json).unwrap();
        for profile in pins["profiles"].as_array().unwrap() {
            let pem = profile["ecdsaPEM"].as_str().unwrap();
            // Dummy invalid signature should fail signature verification (not PEM parse).
            let err_o =
                ossl::verify_signature(pem, b"msg", &[0x30, 0x06, 0x02, 0x01, 1, 0x02, 0x01, 1])
                    .unwrap_err()
                    .to_string();
            let err_p =
                pure::verify_signature(pem, b"msg", &[0x30, 0x06, 0x02, 0x01, 1, 0x02, 0x01, 1])
                    .unwrap_err()
                    .to_string();
            assert_eq!(err_o, "Pinned server signature verification failed");
            assert_eq!(err_p, "Pinned server signature verification failed");
        }
    }
}

#[test]
fn chacha20_and_poly1305_match_openssl_byte_for_byte() {
    let key = [0x4bu8; 32];
    let nonce = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
    for counter in [0u64, 1, 42, (1u64 << 32) + 7] {
        for len in [0usize, 1, 32, 64, 127, 1484] {
            let input: Vec<u8> = (0..len).map(|i| (i & 0xff) as u8).collect();
            let ox = ossl::chacha20_xor(&key, nonce, counter, &input).unwrap();
            let px = pure::chacha20_xor(&key, nonce, counter, &input).unwrap();
            assert_eq!(*ox, *px, "counter={counter}, len={len}");
        }
    }

    let one = ossl::chacha20_xor(&key, nonce, 0, &[0u8; 32]).unwrap();
    let one_key: [u8; 32] = (&one[..]).try_into().unwrap();
    for len in [0usize, 1, 15, 16, 17, 100, 1484] {
        let cipher: Vec<u8> = (0..len)
            .map(|i| (i.wrapping_mul(19) & 0xff) as u8)
            .collect();
        let zero = 0u64.to_le_bytes();
        let len_le = (cipher.len() as u64).to_le_bytes();
        let parts: &[&[u8]] = &[&zero, &cipher, &len_le];
        assert_eq!(
            ossl::poly1305_mac(&one_key, parts).unwrap(),
            pure::poly1305_mac(&one_key, parts).unwrap()
        );
    }
}

#[test]
fn rsa_1024_spki_pem_from_pure_rust_is_accepted_by_openssl() {
    let pem = pure::generate_rsa_1024_public_pem().unwrap();
    let pkey = openssl::pkey::PKey::public_key_from_pem(&pem).unwrap();
    let rsa = pkey.rsa().unwrap();
    assert_eq!(rsa.size() * 8, 1024);
}

#[test]
fn random_and_constant_time_eq_match_semantics() {
    let a = pure::random::<32>().unwrap();
    let b = pure::random::<32>().unwrap();
    assert_ne!(a, b);
    assert!(pure::constant_time_eq(&a, &a));
    assert_eq!(
        ossl::constant_time_eq(&a, &b),
        pure::constant_time_eq(&a, &b)
    );
    assert!(!pure::constant_time_eq(&a, &a[..31]));
}
