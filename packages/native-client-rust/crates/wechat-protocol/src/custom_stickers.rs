//! Custom collection reads and mutations, separate from sticker chat sends.
use crate::{protobuf as p, AccountState, NativeClient};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn string(fields: &[p::Field], number: u32) -> Result<Option<String>> {
    p::optional_data(fields, number)?
        .map(|bytes| {
            ensure!(bytes.len() <= 16384, "sticker_metadata_limit");
            Ok(std::str::from_utf8(bytes)?.to_owned())
        })
        .transpose()
}
/// Private transport metadata. These URLs and keys must not be exposed by an API.
#[derive(Clone, Serialize, Deserialize)]
pub struct CustomStickerMetadata {
    /// Content MD5.
    pub id: String,
    /// Original source URL, if supplied.
    pub original_url: Option<String>,
    /// Thumbnail source URL, if supplied.
    pub thumbnail_url: Option<String>,
    /// Full original record, retaining unassigned transport and extension fields.
    pub raw: Vec<u8>,
}
/// One custom-collection response, retained before profile writeback.
#[derive(Clone, Serialize, Deserialize)]
pub struct CustomStickerPage {
    /// Content identities in server order.
    pub ids: Vec<String>,
    /// Available per-item metadata; match by identity, not array position.
    pub metadata: Vec<CustomStickerMetadata>,
    /// Native continuation index; absence remains absence.
    pub next_index: Option<u32>,
    /// Native opaque continuation bytes.
    pub next_data: Vec<u8>,
    /// True only when the completion predicate is satisfied.
    pub complete: bool,
    /// Server version extension, used only after a complete list.
    pub version: Option<String>,
    /// Original response, including fields unknown to this decoder.
    pub raw: Vec<u8>,
}
/// Encode a custom-list page. Initial cursor is index zero with empty data.
pub fn custom_sticker_list_request(index: u32, data: &[u8]) -> Result<Vec<u8>> {
    ensure!(data.len() <= 65536, "sticker_cursor_limit");
    p::message(&[
        p::uint(1, index.into()),
        p::uint(2, 2),
        p::uint(4, 0),
        p::bytes(5, data),
    ])
}
/// Encode a bounded metadata request for known collection identities.
pub fn custom_sticker_metadata_request(ids: &[String]) -> Result<Vec<u8>> {
    ensure!(
        !ids.is_empty() && ids.len() <= 100 && ids.iter().all(|id| valid_id(id)),
        "invalid_sticker_ids"
    );
    let mut fields = vec![p::uint(1, 0), p::uint(2, 1)];
    fields.extend(ids.iter().map(|id| p::bytes(3, id.as_bytes())));
    p::message(&fields)
}
/// Decode the shared list/metadata response without requiring generated fields
/// that the service omits. Callers enforce advancing cursors and page limits.
pub fn decode_custom_stickers(raw: &[u8]) -> Result<CustomStickerPage> {
    let fields = p::decode(raw)?;
    ensure!(p::base_code(&fields)? == 0, "custom_stickers_rejected");
    let mut ids = Vec::new();
    let mut metadata = Vec::new();
    for field in &fields {
        if field.number == 3 {
            let p::Value::Bytes(bytes) = &field.value else {
                anyhow::bail!("invalid_sticker_id");
            };
            let id = std::str::from_utf8(bytes)?.to_ascii_lowercase();
            ensure!(
                valid_id(&id) && ids.len() < 10000 && !ids.contains(&id),
                "invalid_sticker_id"
            );
            ids.push(id);
        }
        if field.number == 5 {
            let p::Value::Bytes(bytes) = &field.value else {
                anyhow::bail!("invalid_sticker_metadata");
            };
            ensure!(metadata.len() < 10000, "sticker_metadata_limit");
            let item = p::decode(bytes)?;
            let id = string(&item, 1)?.unwrap_or_default().to_ascii_lowercase();
            ensure!(
                valid_id(&id)
                    && !metadata
                        .iter()
                        .any(|item: &CustomStickerMetadata| item.id == id),
                "invalid_sticker_id"
            );
            metadata.push(CustomStickerMetadata {
                id,
                original_url: string(&item, 2)?,
                thumbnail_url: string(&item, 3)?,
                raw: bytes.clone(),
            });
        }
    }
    let next_index = p::optional(&fields, 2)?
        .map(|value| match value {
            p::Value::Varint(value) => Ok(u32::try_from(*value)?),
            _ => anyhow::bail!("invalid_sticker_cursor"),
        })
        .transpose()?;
    let complete = ids.is_empty()
        || match p::optional(&fields, 4)? {
            Some(p::Value::Varint(value)) => *value != 0,
            None => false,
            _ => anyhow::bail!("invalid_sticker_completion"),
        };
    let next_data = p::optional_data(&fields, 6)?.unwrap_or_default().to_vec();
    ensure!(next_data.len() <= 65536, "sticker_cursor_limit");
    Ok(CustomStickerPage {
        ids,
        metadata,
        next_index,
        next_data,
        complete,
        version: string(&fields, 8)?,
        raw: raw.into(),
    })
}
/// One membership or placement intent. It is not an automatic retry policy.
#[derive(Clone, Serialize, Deserialize)]
pub enum CustomStickerMutation {
    /// Save an uploaded/existing sticker identity.
    Add,
    /// Remove Saved membership for the identity.
    Remove,
    /// Move the identity to the front.
    First,
    /// Move after an anchor using a fresh complete-list server version.
    After {
        /// Identity immediately before the requested placement.
        anchor: String,
        /// Current version from a completed fresh custom-list response.
        version: String,
        /// Fresh correlation value for this operation, retained for its lifetime.
        update_id: String,
    },
}
/// Encode one custom membership/placement operation, without a BaseRequest.
pub fn custom_sticker_mutation_request(
    id: &str,
    action: &CustomStickerMutation,
) -> Result<Vec<u8>> {
    ensure!(valid_id(id), "invalid_sticker_id");
    let operation = match action {
        CustomStickerMutation::Add => 4,
        CustomStickerMutation::Remove => 2,
        CustomStickerMutation::First => 3,
        CustomStickerMutation::After { .. } => 5,
    };
    let mut fields = vec![
        p::bytes(1, id.as_bytes()),
        p::uint(2, operation),
        p::uint(3, 0),
    ];
    if let CustomStickerMutation::After {
        anchor,
        version,
        update_id,
    } = action
    {
        ensure!(
            valid_id(anchor)
                && !anchor.eq_ignore_ascii_case(id)
                && !version.is_empty()
                && version.len() <= 16384
                && !update_id.is_empty()
                && update_id.len() <= 256,
            "invalid_sticker_placement"
        );
        fields.extend([
            p::bytes(4, anchor.as_bytes()),
            p::bytes(5, version.as_bytes()),
            p::bytes(6, update_id.as_bytes()),
        ]);
    }
    p::message(&fields)
}
impl NativeClient {
    pub(crate) fn custom_sticker_read_snapshot(
        &self,
        state: &mut AccountState,
        request: &[u8],
    ) -> Result<CustomStickerPage> {
        let raw = self.authenticated_rpc(
            state,
            697,
            "/cgi-bin/micromsg-bin/mmbatchemojidownload",
            request,
        )?;
        decode_custom_stickers(&raw)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_list_and_metadata_use_distinct_presence() {
        assert_eq!(
            hex::encode(custom_sticker_list_request(0, &[]).unwrap()),
            "0800100220002a00"
        );
        let id = "00000000000000000000000000000000".to_string();
        let request = p::decode(&custom_sticker_metadata_request(&[id]).unwrap()).unwrap();
        assert_eq!(p::scalar(&request, 2).unwrap(), 1);
        assert!(p::optional(&request, 4).unwrap().is_none());
        assert!(p::optional(&request, 5).unwrap().is_none());
    }
    #[test]
    fn omitted_generated_fields_do_not_hide_an_incomplete_nonempty_list() {
        let base = p::bytes(1, &p::uint(1, 0));
        let id = p::bytes(3, b"00000000000000000000000000000000");
        let raw = p::message(&[base.clone(), id.clone(), p::bytes(8, b"fresh-version")]).unwrap();
        let page = decode_custom_stickers(&raw).unwrap();
        assert!(!page.complete);
        assert_eq!(page.next_index, None);
        let raw = p::message(&[base, id, p::uint(4, 1)]).unwrap();
        let page = decode_custom_stickers(&raw).unwrap();
        assert!(page.complete);
        assert!(page.version.is_none());
    }
    #[test]
    fn after_anchor_requires_distinct_identity_and_fresh_version() {
        let id = "00000000000000000000000000000000";
        let action = CustomStickerMutation::After {
            anchor: "11111111111111111111111111111111".into(),
            version: "fresh".into(),
            update_id: "synthetic_uuid_1234".into(),
        };
        let raw = custom_sticker_mutation_request(id, &action).unwrap();
        let fields = p::decode(&raw).unwrap();
        assert_eq!(p::scalar(&fields, 2).unwrap(), 5);
        assert_eq!(
            p::data(&fields, 4).unwrap(),
            b"11111111111111111111111111111111"
        );
        assert_eq!(p::data(&fields, 5).unwrap(), b"fresh");
        assert!(
            custom_sticker_mutation_request("11111111111111111111111111111111", &action).is_err()
        );
    }
}
