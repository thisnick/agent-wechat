//! Direct custom-sticker upload chunks. This operation does not send a chat message.
use crate::{protobuf as p, NativeClient};
use anyhow::{ensure, Result};

/// Maximum plaintext bytes carried by one direct sticker upload request.
pub const STICKER_UPLOAD_CHUNK_BYTES: usize = 98_304;
/// A validated acknowledgement for one direct upload chunk.
#[derive(Clone, Debug)]
pub struct StickerUploadAcknowledgement {
    /// First byte not yet acknowledged by the service.
    pub next_offset: u32,
    /// Source size echoed by the service.
    pub total_bytes: u32,
    /// Original response, including unassigned optional fields. Protect at rest.
    pub raw: Vec<u8>,
}
/// Encode one direct upload chunk from an owned source. Unlike chat sticker
/// sends this request carries raw bytes immediately, without a BaseRequest or
/// a nested builtin buffer. Offsets are bounded signed-32-bit wire scalars.
pub fn sticker_upload_request(source: &[u8], offset: u32) -> Result<Vec<u8>> {
    ensure!(
        !source.is_empty() && source.len() <= wechat_media::MAX_IMAGE_BYTES,
        "sticker_source_size_limit"
    );
    let offset = usize::try_from(offset)?;
    ensure!(offset < source.len(), "sticker_upload_offset_invalid");
    let end = source.len().min(offset + STICKER_UPLOAD_CHUNK_BYTES);
    let identity = hex::encode(crate::crypto::md5(source));
    let item = p::message(&[
        p::bytes(1, identity.as_bytes()),
        p::uint(2, offset as u64),
        p::uint(3, source.len() as u64),
        p::bytes(4, &source[offset..end]),
    ])?;
    p::message(&[p::bytes(1, &item), p::uint(3, 1)])
}
/// Validate progress against the exact chunk submitted. Rejected, stale,
/// nonadvancing and mismatched replies must not trigger an automatic resend.
pub fn decode_sticker_upload(
    raw: &[u8],
    offset: u32,
    total: u32,
    expected_md5: &str,
) -> Result<StickerUploadAcknowledgement> {
    let fields = p::decode(raw)?;
    ensure!(p::base_code(&fields)? == 0, "sticker_upload_rejected");
    let items = fields
        .iter()
        .filter(|field| field.number == 2)
        .collect::<Vec<_>>();
    ensure!(items.len() == 1, "sticker_upload_reply_invalid");
    let item = p::decode(p::data(&fields, 2)?)?;
    ensure!(p::scalar(&item, 1)? == 0, "sticker_upload_item_rejected");
    if let Some(identity) = p::optional_data(&item, 4)? {
        ensure!(
            std::str::from_utf8(identity)?.eq_ignore_ascii_case(expected_md5),
            "sticker_upload_identity_mismatch"
        );
    }
    let next_offset = u32::try_from(p::scalar(&item, 2)?)?;
    let returned_total = u32::try_from(p::scalar(&item, 3)?)?;
    ensure!(
        returned_total == total
            && next_offset > offset
            && next_offset <= total
            && next_offset - offset <= STICKER_UPLOAD_CHUNK_BYTES as u32,
        "sticker_upload_progress_invalid"
    );
    Ok(StickerUploadAcknowledgement {
        next_offset,
        total_bytes: total,
        raw: raw.into(),
    })
}
impl NativeClient {
    pub(crate) fn upload_sticker_chunk_snapshot(
        &self,
        state: &mut crate::AccountState,
        source: &[u8],
        offset: u32,
    ) -> Result<StickerUploadAcknowledgement> {
        let request = sticker_upload_request(source, offset)?;
        let reply =
            self.authenticated_rpc(state, 703, "/cgi-bin/micromsg-bin/mmemojiupload", &request)?;
        decode_sticker_upload(
            &reply,
            offset,
            u32::try_from(source.len())?,
            &hex::encode(crate::crypto::md5(source)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_continuation_and_final_shapes_keep_raw_chunk_boundaries() {
        assert_eq!(hex::encode(sticker_upload_request(b"abc", 0).unwrap()), "0a2b0a2039303031353039383363643234666230643639363366376432386531376637321000180322036162631801");
        assert_eq!(hex::encode(sticker_upload_request(b"abc", 1).unwrap()), "0a2a0a20393030313530393833636432346662306436393633663764323865313766373210011803220262631801");
        let raw = hex::decode("0a0208001206080010031803").unwrap();
        let reply = decode_sticker_upload(&raw, 0, 3, "900150983cd24fb0d6963f7d28e17f72").unwrap();
        assert_eq!(reply.next_offset, 3);
        assert_eq!(reply.raw, raw);
        assert!(decode_sticker_upload(&raw, 3, 3, "900150983cd24fb0d6963f7d28e17f72").is_err());
        assert!(decode_sticker_upload(&raw, 0, 4, "900150983cd24fb0d6963f7d28e17f72").is_err());
        assert!(sticker_upload_request(b"abc", 3).is_err());
    }
    #[test]
    fn oversized_advances_duplicate_results_and_rejections_are_not_success() {
        for raw in [
            "0a0208001206080010041803",
            "0a0208001206080010001803",
            "0a0208001206080110031803",
            "0a0208011206080010031803",
            "0a02080012060800100318031206080010031803",
        ] {
            assert!(decode_sticker_upload(
                &hex::decode(raw).unwrap(),
                0,
                3,
                "900150983cd24fb0d6963f7d28e17f72"
            )
            .is_err());
        }
        let bytes = vec![0x42; STICKER_UPLOAD_CHUNK_BYTES + 1];
        let request = p::decode(&sticker_upload_request(&bytes, 0).unwrap()).unwrap();
        let item = p::decode(p::data(&request, 1).unwrap()).unwrap();
        assert_eq!(p::data(&item, 4).unwrap().len(), STICKER_UPLOAD_CHUNK_BYTES);
        let request =
            p::decode(&sticker_upload_request(&bytes, STICKER_UPLOAD_CHUNK_BYTES as u32).unwrap())
                .unwrap();
        let item = p::decode(p::data(&request, 1).unwrap()).unwrap();
        assert_eq!(p::data(&item, 4).unwrap(), &[0x42]);
    }
}
