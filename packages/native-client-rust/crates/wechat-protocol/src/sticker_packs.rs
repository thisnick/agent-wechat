//! Official pack metadata reads and membership requests. Media acquisition and
//! profile installation are separate operations; these records contain private URLs.
use crate::{protobuf as p, AccountState, NativeClient};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

/// Feed selected by the supported Linux pack-list producer.
#[derive(Clone, Copy, Debug)]
pub enum StickerPackFeed {
    /// Available public feed; reaching its end does not imply a global inventory.
    Catalog,
    /// Complete account membership, including packs absent from the public feed.
    Added,
}
/// Transport-level summary. Protect URLs and the retained original record.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackSummary {
    /// Official product ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display description, independent of the Linux SQL introduction column.
    pub description: String,
    /// Private pack icon source.
    pub icon_url: String,
    /// Private cover source.
    pub cover_url: String,
    /// Numbered type flags; only the corroborated free class is eligible.
    pub pack_type: u32,
    /// Native flags; do not infer entitlement from a price string.
    pub flags: u32,
    /// Native expiry, zero when unspecified.
    pub expiry: u32,
    /// Full original summary for exact profile mapping and unknown extensions.
    pub raw: Vec<u8>,
}
/// One list page. Callers enforce cursor progress, aggregate limits and deduplication.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackPage {
    /// Returned summaries in the service's order.
    pub items: Vec<StickerPackSummary>,
    /// Opaque continuation bytes from the native buffer.
    pub next_cursor: Vec<u8>,
    /// Only BaseResponse.Ret zero establishes a complete list.
    pub complete: bool,
    /// Ret3 invalidates this pagination sequence; restart as a new read.
    pub restart: bool,
    /// Complete reply, to retain before projection.
    pub raw: Vec<u8>,
}
/// One remote pack item. Dimensions and byte lengths require verified media.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackItem {
    /// Original media identity.
    pub id: String,
    /// Private original source URL.
    pub original_url: String,
    /// Private thumbnail source URL.
    pub thumbnail_url: String,
    /// Full item record including unsupported alternate representations.
    pub raw: Vec<u8>,
}
/// Pack detail, independent of account membership and downloaded content.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackDetail {
    /// Product metadata, decoded with the detail field layout.
    pub summary: StickerPackSummary,
    /// Complete supplied content list; an empty result is not invented content.
    pub items: Vec<StickerPackItem>,
    /// Original full response.
    pub raw: Vec<u8>,
}
/// Localized captions supplied for one pack item.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackCaption {
    /// Content MD5 used to join the row to an item.
    pub id: String,
    /// Official language/text pairs; empty values are omitted.
    pub captions: Vec<(String, String)>,
}
/// Caption response retained separately from pack content readiness.
#[derive(Clone, Serialize, Deserialize)]
pub struct StickerPackCaptions {
    /// Caption rows in service order.
    pub items: Vec<StickerPackCaption>,
    /// Original response, retaining unassigned flags.
    pub raw: Vec<u8>,
}
fn product(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control),
        "invalid_sticker_pack_id"
    );
    Ok(())
}
fn md5(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid_sticker_id"
    );
    Ok(())
}
fn string(fields: &[p::Field], n: u32) -> Result<String> {
    let bytes = p::optional_data(fields, n)?.unwrap_or_default();
    ensure!(bytes.len() <= 16384, "sticker_metadata_limit");
    Ok(std::str::from_utf8(bytes)?.into())
}
fn scalar(fields: &[p::Field], n: u32) -> Result<u32> {
    match p::optional(fields, n)? {
        Some(p::Value::Varint(value)) => Ok(u32::try_from(*value)?),
        None => Ok(0),
        _ => anyhow::bail!("invalid_sticker_pack_scalar"),
    }
}
fn repeated(fields: &[p::Field], n: u32) -> Result<Vec<&[u8]>> {
    let mut items = vec![];
    for field in fields.iter().filter(|field| field.number == n) {
        ensure!(items.len() < 1000, "sticker_pack_item_limit");
        let p::Value::Bytes(bytes) = &field.value else {
            anyhow::bail!("invalid_sticker_pack_record")
        };
        items.push(bytes.as_slice());
    }
    Ok(items)
}
fn summary(raw: &[u8], detail: bool) -> Result<StickerPackSummary> {
    let fields = p::decode(raw)?;
    let id = string(&fields, 1)?;
    product(&id)?;
    Ok(StickerPackSummary {
        id,
        name: string(&fields, 3)?,
        description: string(&fields, 4)?,
        icon_url: string(&fields, 2)?,
        cover_url: string(&fields, if detail { 11 } else { 9 })?,
        pack_type: scalar(&fields, 7)?,
        flags: scalar(&fields, 8)?,
        expiry: scalar(&fields, if detail { 12 } else { 10 })?,
        raw: raw.into(),
    })
}
/// Encode an initial or continuation feed read with native mode/context pairs.
pub fn sticker_pack_list_request(
    state: &AccountState,
    feed: StickerPackFeed,
    cursor: &[u8],
) -> Result<Vec<u8>> {
    ensure!(cursor.len() <= 65536, "sticker_cursor_limit");
    let (mode, context) = match feed {
        StickerPackFeed::Catalog => (5, 5),
        StickerPackFeed::Added => (8, 13),
    };
    p::message(&[
        p::bytes(1, &crate::wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &p::buffer(cursor)),
        p::uint(3, mode),
        p::uint(4, context),
    ])
}
/// Decode list completion from BaseResponse.Ret, preserving a restart outcome.
pub fn decode_sticker_pack_page(raw: &[u8]) -> Result<StickerPackPage> {
    let fields = p::decode(raw)?;
    let code = p::base_code(&fields)?;
    ensure!(matches!(code, 0 | 2 | 3), "sticker_pack_list_rejected");
    let items = repeated(&fields, 4)?
        .into_iter()
        .map(|raw| summary(raw, false))
        .collect::<Result<Vec<_>>>()?;
    let cursor = p::optional_data(&fields, 2)?
        .map(p::unbuffer)
        .transpose()?
        .unwrap_or_default();
    ensure!(cursor.len() <= 65536, "sticker_cursor_limit");
    Ok(StickerPackPage {
        items,
        next_cursor: cursor,
        complete: code == 0,
        restart: code == 3,
        raw: raw.into(),
    })
}
/// Encode detail read; field4 is signed int32 minus one, not uint32 max.
pub fn sticker_pack_detail_request(state: &AccountState, id: &str) -> Result<Vec<u8>> {
    product(id)?;
    p::message(&[
        p::bytes(1, &crate::wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, id.as_bytes()),
        p::uint(3, 3),
        p::int32(4, -1),
    ])
}
/// Decode a detail and its per-item extension, checking requested product identity.
pub fn decode_sticker_pack_detail(raw: &[u8], expected: &str) -> Result<StickerPackDetail> {
    let fields = p::decode(raw)?;
    ensure!(p::base_code(&fields)? == 0, "sticker_pack_detail_rejected");
    let summary = summary(p::data(&fields, 2)?, true)?;
    ensure!(summary.id == expected, "sticker_pack_identity_mismatch");
    let mut items = vec![];
    for raw in repeated(&fields, 3)? {
        let fields = p::decode(raw)?;
        let id = string(&fields, 1)?.to_ascii_lowercase();
        md5(&id)?;
        ensure!(
            !items.iter().any(|item: &StickerPackItem| item.id == id),
            "sticker_pack_duplicate_item"
        );
        items.push(StickerPackItem {
            id,
            original_url: string(&fields, 2)?,
            thumbnail_url: string(&fields, 3)?,
            raw: raw.into(),
        });
    }
    Ok(StickerPackDetail {
        summary,
        items,
        raw: raw.into(),
    })
}
/// Encode a localized-caption read without starting any media acquisition.
pub fn sticker_pack_captions_request(state: &AccountState, id: &str) -> Result<Vec<u8>> {
    product(id)?;
    p::message(&[
        p::bytes(1, &crate::wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, id.as_bytes()),
    ])
}
/// Decode captions by MD5/language rather than assuming positional correspondence.
pub fn decode_sticker_pack_captions(raw: &[u8]) -> Result<StickerPackCaptions> {
    let fields = p::decode(raw)?;
    ensure!(
        p::base_code(&fields)? == 0,
        "sticker_pack_captions_rejected"
    );
    let mut items = vec![];
    for raw in repeated(&fields, 3)? {
        let fields = p::decode(raw)?;
        let id = string(&fields, 1)?.to_ascii_lowercase();
        md5(&id)?;
        let mut captions = vec![];
        for raw in repeated(&fields, 3)? {
            let pair = p::decode(raw)?;
            let lang = string(&pair, 1)?;
            let text = string(&pair, 2)?;
            if !lang.is_empty() && !text.is_empty() {
                ensure!(
                    !captions.iter().any(|(l, _)| l == &lang),
                    "sticker_caption_duplicate_language"
                );
                captions.push((lang, text));
            }
        }
        items.push(StickerPackCaption { id, captions });
    }
    Ok(StickerPackCaptions {
        items,
        raw: raw.into(),
    })
}
/// Encode free-pack membership. This is neither a purchase nor content download.
pub fn sticker_pack_membership_request(
    state: &AccountState,
    id: &str,
    added: bool,
) -> Result<Vec<u8>> {
    product(id)?;
    p::message(&[
        p::bytes(1, &crate::wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, id.as_bytes()),
        p::uint(3, if added { 1 } else { 2 }),
    ])
}
/// Request an archive locator for one eligible free pack. This does not add
/// membership. Retain the raw acknowledgement before interpreting its locator.
pub fn sticker_pack_acquisition_request(state: &AccountState, id: &str) -> Result<Vec<u8>> {
    product(id)?;
    p::message(&[
        p::bytes(1, &crate::wire::base_request(&state.device, state.uin, 0)?),
        p::bytes(2, id.as_bytes()),
        p::uint(4, 0),
        p::uint(5, 0),
    ])
}
/// Decode the response-bound file-ID, AES key and exact plaintext archive size.
/// The result contains credentials and must not appear in logs or public results.
pub fn decode_sticker_pack_acquisition(raw: &[u8]) -> Result<crate::media::MediaDescriptor> {
    let fields = p::decode(raw)?;
    ensure!(
        p::base_code(&fields)? == 0,
        "sticker_pack_acquisition_rejected"
    );
    let locator = p::decode(p::data(&fields, 2)?)?;
    let file_id = string(&locator, 1)?;
    let key = string(&locator, 2)?;
    let size = u32::try_from(p::scalar(&locator, 3)?)?;
    ensure!(
        !file_id.is_empty()
            && file_id.len() <= 8192
            && !file_id.chars().any(char::is_control)
            && key.len() == 32
            && key.bytes().all(|b| b.is_ascii_hexdigit())
            && size > 0
            && size <= 16 * 1024 * 1024,
        "invalid_sticker_pack_locator"
    );
    Ok(crate::media::MediaDescriptor {
        file_id,
        aes_key: hex::decode(key)?,
        quality: crate::media::MediaQuality::File,
        expected_bytes: Some(u64::from(size)),
        expected_md5: None,
        strict_size: true,
        expires_at: None,
    })
}
impl NativeClient {
    pub(crate) fn sticker_pack_rpc_snapshot(
        &self,
        state: &mut AccountState,
        cgi: u16,
        request: &[u8],
    ) -> Result<Vec<u8>> {
        let path = match cgi {
            411 => "/cgi-bin/micromsg-bin/getemotionlist",
            412 => "/cgi-bin/micromsg-bin/getemotiondetail",
            521 => "/cgi-bin/micromsg-bin/getemotiondesc",
            413 => "/cgi-bin/micromsg-bin/modemotionpack",
            423 => "/cgi-bin/micromsg-bin/exchangeemotionpack",
            _ => anyhow::bail!("unsupported_sticker_pack_request"),
        };
        self.authenticated_rpc(state, cgi, path, request)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_ret_controls_continuation_and_unknown_fields_survive() {
        let item = p::message(&[
            p::bytes(1, b"pack.fixture"),
            p::bytes(2, b"https://example.invalid/icon"),
            p::bytes(3, b"Fixture"),
            p::uint(7, 4),
            p::uint(8, 0),
            p::bytes(13, b"gray"),
        ])
        .unwrap();
        let raw = p::message(&[
            p::bytes(1, &p::int32(1, 2)),
            p::bytes(2, &p::buffer(b"next")),
            p::uint(3, 0),
            p::bytes(4, &item),
            p::uint(99, 7),
        ])
        .unwrap();
        let page = decode_sticker_pack_page(&raw).unwrap();
        assert!(!page.complete);
        assert_eq!(page.next_cursor, b"next");
        assert_eq!(page.items[0].name, "Fixture");
        assert_eq!(page.raw, raw);
        let done = p::message(&[p::bytes(1, &p::int32(1, 0)), p::uint(3, 999)]).unwrap();
        assert!(decode_sticker_pack_page(&done).unwrap().complete);
    }
    #[test]
    fn detail_tail_and_item_identity_use_their_own_layout() {
        let id = "00000000000000000000000000000000";
        let detail = p::message(&[
            p::bytes(1, b"pack.fixture"),
            p::bytes(11, b"cover"),
            p::uint(12, 99),
        ])
        .unwrap();
        let item = p::message(&[
            p::bytes(1, id.as_bytes()),
            p::bytes(2, b"original"),
            p::bytes(3, b"thumb"),
        ])
        .unwrap();
        let raw = p::message(&[
            p::bytes(1, &p::int32(1, 0)),
            p::bytes(2, &detail),
            p::bytes(3, &item),
        ])
        .unwrap();
        let parsed = decode_sticker_pack_detail(&raw, "pack.fixture").unwrap();
        assert_eq!(parsed.summary.cover_url, "cover");
        assert_eq!(parsed.summary.expiry, 99);
        assert_eq!(parsed.items[0].id, id);
        assert!(decode_sticker_pack_detail(&raw, "other").is_err());
        assert_eq!(hex::encode(p::int32(4, -1)), "20ffffffffffffffffff01");
    }
}
