//! File-upload message reservation. This authenticated request can affect the
//! account's message state; callers must persist its receipt and never retry an
//! uncertain response automatically.
use crate::{protobuf as p, wire::base_request, AccountState};
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroize;

pub const MAX_RESERVED_FILE_BYTES: usize = crate::media::MAX_LARGE_FILE_BYTES;
const MAX_FILE_UPLOAD_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadReservation {
    pub file_upload_token: String,
    pub overwrite_newmsgid: u64,
}
impl Drop for FileUploadReservation {
    fn drop(&mut self) {
        self.file_upload_token.zeroize();
        self.overwrite_newmsgid.zeroize();
    }
}
impl fmt::Debug for FileUploadReservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileUploadReservation")
            .field("receipt", &"[redacted]")
            .finish()
    }
}
impl FileUploadReservation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.overwrite_newmsgid != 0
                && self.file_upload_token.len() <= MAX_FILE_UPLOAD_TOKEN_BYTES,
            "Invalid file upload reservation"
        );
        let encoded = self
            .file_upload_token
            .strip_prefix("v1_")
            .ok_or_else(|| anyhow::anyhow!("Unsupported file upload token"))?;
        ensure!(!encoded.is_empty(), "Empty file upload token");
        let mut decoded = STANDARD.decode(encoded)?;
        let nonempty = !decoded.is_empty();
        decoded.zeroize();
        ensure!(nonempty, "Empty file upload token");
        Ok(())
    }
}

pub(crate) fn file_metadata<'a>(
    file_name: &'a str,
    plaintext_bytes: usize,
    md5: &str,
) -> Result<(&'a str, String)> {
    ensure!(
        !file_name.is_empty()
            && file_name.len() <= 255
            && !file_name.chars().any(char::is_control)
            && !matches!(file_name, "." | "..")
            && !file_name.contains(['/', '\\']),
        "Expected a single filename component"
    );
    ensure!(
        plaintext_bytes > 0 && plaintext_bytes <= MAX_RESERVED_FILE_BYTES,
        "Invalid reserved file size"
    );
    ensure!(
        md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid file MD5"
    );
    let extension = file_name
        .rsplit_once('.')
        .filter(|(base, _)| !base.is_empty())
        .map(|(_, extension)| extension)
        .unwrap_or("");
    Ok((extension, md5.to_ascii_lowercase()))
}

pub fn file_upload_request(
    state: &AccountState,
    destination: &str,
    progress_client_id: &str,
    file_name: &str,
    plaintext_bytes: usize,
    md5: &str,
) -> Result<Vec<u8>> {
    for (value, max) in [(destination, 256), (progress_client_id, 1024)] {
        ensure!(
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control),
            "Invalid file upload destination or progress identity"
        );
    }
    let (extension, md5) = file_metadata(file_name, plaintext_bytes, md5)?;
    let info = p::message(&[
        p::bytes(1, file_name.as_bytes()),
        p::bytes(2, extension.as_bytes()),
        p::bytes(3, md5.as_bytes()),
        p::uint(4, plaintext_bytes as u64),
    ])?;
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, destination.as_bytes()),
        p::bytes(3, progress_client_id.as_bytes()),
        p::uint(4, 1),
        p::bytes(5, &info),
        p::uint(7, 0),
        p::uint(12, 0),
    ])
}

/// Decode the receipt from one authenticated CGI6691 response. This response
/// has no client-ID echo; the caller must retain its association with the intent.
pub fn decode_file_upload_reservation(input: &[u8]) -> Result<FileUploadReservation> {
    ensure!(
        input.len() <= 64 * 1024,
        "File upload reservation response too large"
    );
    let fields = p::decode(input)?;
    let base = p::decode(p::data(&fields, 1)?)?;
    let code = p::scalar(&base, 1)?;
    ensure!(
        code == 0,
        "File upload reservation returned {}",
        code as u32 as i32
    );
    let receipt = FileUploadReservation {
        file_upload_token: std::str::from_utf8(p::data(&fields, 2)?)?.to_owned(),
        overwrite_newmsgid: p::scalar(&fields, 3)?,
    };
    receipt.validate()?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(token: &[u8], id: u64) -> Vec<u8> {
        p::message(&[
            p::bytes(1, &p::int32(1, 0)),
            p::bytes(2, token),
            p::uint(3, id),
        ])
        .unwrap()
    }

    #[test]
    fn receipt_roundtrip_redacts_and_retains_full_id() {
        let reservation =
            decode_file_upload_reservation(&reply(b"v1_c3ludGhldGlj", u64::MAX)).unwrap();
        assert_eq!(reservation.overwrite_newmsgid, u64::MAX);
        let stored = serde_json::to_vec(&reservation).unwrap();
        let restored: FileUploadReservation = serde_json::from_slice(&stored).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.file_upload_token, reservation.file_upload_token);
        assert!(!format!("{restored:?}").contains("c3ludGhldGlj"));
        assert!(!format!("{restored:?}").contains(&u64::MAX.to_string()));
    }

    #[test]
    fn receipt_rejects_ambiguous_missing_invalid_and_rejected_fields() {
        for input in [
            reply(b"v1_c3ludGhldGlj", 0),
            reply(b"v2_c3ludGhldGlj", 1),
            reply(b"v1_", 1),
            reply(b"v1_!!!", 1),
            reply(&vec![b'a'; MAX_FILE_UPLOAD_TOKEN_BYTES + 1], 1),
            p::message(&[p::bytes(1, &p::int32(1, -13))]).unwrap(),
            p::message(&[
                p::bytes(1, &p::uint(1, 1 << 32)),
                p::bytes(2, b"v1_c3ludGhldGlj"),
                p::uint(3, 1),
            ])
            .unwrap(),
            p::message(&[p::bytes(1, &p::int32(1, 0)), p::uint(3, 1)]).unwrap(),
            [reply(b"v1_c3ludGhldGlj", 1), p::uint(3, 2)].concat(),
            [reply(b"v1_c3ludGhldGlj", 1), p::bytes(2, b"v1_YQ==")].concat(),
        ] {
            assert!(decode_file_upload_reservation(&input).is_err());
        }
    }

    #[test]
    fn metadata_bounds_and_extension() {
        assert_eq!(
            file_metadata("sample.TXT", MAX_RESERVED_FILE_BYTES, &"A".repeat(32)).unwrap(),
            ("TXT", "a".repeat(32))
        );
        for name in ["", ".", "..", "a/b", "a\\b", "a\n.txt"] {
            assert!(file_metadata(name, 1, &"0".repeat(32)).is_err());
        }
        for size in [0, MAX_RESERVED_FILE_BYTES + 1] {
            assert!(file_metadata("sample.txt", size, &"0".repeat(32)).is_err());
        }
        assert!(file_metadata("sample.txt", 1, &"z".repeat(32)).is_err());
        assert!(file_metadata(&"a".repeat(256), 1, &"0".repeat(32)).is_err());
    }
}
