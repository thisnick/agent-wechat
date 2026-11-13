//! Single-intent sticker sends with a probe and bounded byte continuations.
use crate::{protobuf as p, AccountState};
use anyhow::{ensure, Context, Result};

/// Maximum native byte continuation; the initial probe has a present empty buffer.
pub const STICKER_SEND_CHUNK: usize = 8192;
/// Journaled request identity and bytes, shared by the probe and all continuations.
pub struct StickerSendChunk<'a> {
    /// Lowercase original GIF MD5.
    pub id: &'a str,
    /// Intended chat identity.
    pub recipient: &'a str,
    /// Unique durable per-send identity, never a product ID.
    pub client_id: &'a str,
    /// Complete owned source; its length is the request total.
    pub source: &'a [u8],
    /// Acknowledged source offset.
    pub offset: u32,
    /// Initial empty probe; only valid at offset zero.
    pub probe: bool,
}
/// Encode a type-2 send after the caller validates the complete GIF and MD5.
/// Field4 is a nested buffer, not raw bytes; no source rehash occurs per chunk.
pub fn sticker_send_request(state: &AccountState, chunk: &StickerSendChunk<'_>) -> Result<Vec<u8>> {
    sticker_send_body(
        &crate::wire::base_request(&state.device, state.uin, 0)?,
        chunk,
    )
}
fn sticker_send_body(base: &[u8], chunk: &StickerSendChunk<'_>) -> Result<Vec<u8>> {
    ensure!(
        chunk.id.len() == 32
            && chunk.id.bytes().all(|b| b.is_ascii_hexdigit())
            && !chunk.source.is_empty()
            && chunk.source.len() <= 16 * 1024 * 1024
            && (chunk.offset as usize) < chunk.source.len()
            && (!chunk.probe || chunk.offset == 0),
        "invalid_sticker_send_source"
    );
    for value in [chunk.recipient, chunk.client_id] {
        ensure!(
            !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control),
            "invalid_sticker_send_identity"
        );
    }
    let offset = chunk.offset as usize;
    let bytes = if chunk.probe {
        &[][..]
    } else {
        &chunk.source[offset..(offset + STICKER_SEND_CHUNK).min(chunk.source.len())]
    };
    let item = p::message(&[
        p::bytes(1, chunk.id.as_bytes()),
        p::int32(2, chunk.offset as i32),
        p::int32(3, chunk.source.len() as i32),
        p::bytes(4, &p::buffer(bytes)),
        p::int32(5, 2),
        p::bytes(6, chunk.recipient.as_bytes()),
        p::bytes(7, b"<gameext type=\"0\" content=\"0\" ></gameext>"),
        p::bytes(9, chunk.client_id.as_bytes()),
    ])?;
    p::message(&[p::bytes(1, base), p::int32(2, 1), p::bytes(3, &item)])
}
/// Validated progress. A server ID alone does not establish final acceptance.
pub struct StickerSendAcknowledgement {
    /// Exact acknowledged next offset within this owned source.
    pub next_offset: u32,
    /// Nonzero 64-bit server identity, required on the complete acknowledgement.
    pub server_id: Option<String>,
}
/// Validate cardinality, returns, identity, total and source range correlation.
/// Retain raw bytes before calling. A decoding failure leaves submission uncertain.
pub fn decode_sticker_send_acknowledgement(
    raw: &[u8],
    chunk: &StickerSendChunk<'_>,
) -> Result<StickerSendAcknowledgement> {
    let fields = p::decode(raw)?;
    ensure!(p::base_code(&fields)? == 0, "sticker_send_rejected");
    ensure!(
        p::scalar(&fields, 2)? == 1 && fields.iter().filter(|f| f.number == 3).count() == 1,
        "sticker_send_reply_mismatch"
    );
    let item = p::decode(p::data(&fields, 3)?)?;
    ensure!(p::scalar(&item, 1)? == 0, "sticker_send_rejected");
    let next = u32::try_from(p::scalar(&item, 2)?)?;
    let total = u32::try_from(p::scalar(&item, 3)?)?;
    let identity = std::str::from_utf8(p::data(&item, 4)?)?;
    let _legacy = u32::try_from(p::scalar(&item, 5)?)?;
    ensure!(
        identity.eq_ignore_ascii_case(chunk.id)
            && total as usize == chunk.source.len()
            && next <= total
            && next >= chunk.offset,
        "sticker_send_reply_mismatch"
    );
    if !chunk.probe {
        ensure!(
            next > chunk.offset
                && next as usize
                    <= (chunk.offset as usize + STICKER_SEND_CHUNK).min(chunk.source.len()),
            "sticker_send_reply_mismatch"
        );
    }
    let server_id = match p::optional(&item, 6)? {
        Some(p::Value::Varint(id)) if *id > 0 => Some(id.to_string()),
        None | Some(p::Value::Varint(0)) => None,
        _ => anyhow::bail!("sticker_send_reply_mismatch"),
    };
    if next == total {
        server_id
            .as_ref()
            .context("sticker_send_final_id_missing")?;
    }
    Ok(StickerSendAcknowledgement {
        next_offset: next,
        server_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independently_encoded_probe_and_final_match_field_boundaries() {
        let base = hex::decode("0a0010011a0664657669636520012a056c696e75783000").unwrap();
        let mut chunk = StickerSendChunk {
            id: "900150983cd24fb0d6963f7d28e17f72",
            recipient: "peer",
            client_id: "intent-1",
            source: b"abc",
            offset: 0,
            probe: true,
        };
        assert_eq!(hex::encode(sticker_send_body(&base,&chunk).unwrap()),"0a170a0010011a0664657669636520012a056c696e7578300010011a690a2039303031353039383363643234666230643639363366376432386531376637321000180322040800120028023204706565723a293c67616d6565787420747970653d22302220636f6e74656e743d223022203e3c2f67616d656578743e4a08696e74656e742d31");
        chunk.probe = false;
        assert_eq!(hex::encode(sticker_send_body(&base,&chunk).unwrap()),"0a170a0010011a0664657669636520012a056c696e7578300010011a6c0a2039303031353039383363643234666230643639363366376432386531376637321000180322070803120361626328023204706565723a293c67616d6565787420747970653d22302220636f6e74656e743d223022203e3c2f67616d656578743e4a08696e74656e742d31");
    }
    #[test]
    fn incomplete_progress_is_not_acceptance_and_ranges_cannot_fast_forward() {
        let source = vec![b'a'; 8193];
        let mut chunk = StickerSendChunk {
            id: "b7652d9bca37038c342cc0c492dd70f9",
            recipient: "peer",
            client_id: "intent-1",
            source: &source,
            offset: 0,
            probe: true,
        };
        let reply = |next, server| {
            p::message(&[
                p::bytes(1, &p::int32(1, 0)),
                p::int32(2, 1),
                p::bytes(
                    3,
                    &p::message(&[
                        p::int32(1, 0),
                        p::uint(2, next),
                        p::uint(3, 8193),
                        p::bytes(4, chunk.id.as_bytes()),
                        p::uint(5, 1),
                        p::uint(6, server),
                    ])
                    .unwrap(),
                ),
            ])
            .unwrap()
        };
        assert_eq!(
            decode_sticker_send_acknowledgement(&reply(0, 42), &chunk)
                .unwrap()
                .next_offset,
            0
        );
        chunk.probe = false;
        assert!(decode_sticker_send_acknowledgement(&reply(8193, 42), &chunk).is_err());
        assert_eq!(
            decode_sticker_send_acknowledgement(&reply(8192, 42), &chunk)
                .unwrap()
                .next_offset,
            8192
        );
        chunk.offset = 8192;
        assert!(decode_sticker_send_acknowledgement(&reply(8193, 0), &chunk).is_err());
        assert_eq!(
            decode_sticker_send_acknowledgement(&reply(8193, u64::MAX), &chunk)
                .unwrap()
                .server_id
                .as_deref(),
            Some("18446744073709551615")
        );
    }
}
