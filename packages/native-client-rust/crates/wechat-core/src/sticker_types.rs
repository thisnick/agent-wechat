//! Account-scoped sticker preparation contracts.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Maximum source bytes for local image/GIF/video sticker preparation.
pub const MAX_STICKER_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// Explicit preparation of owned image/GIF/video bytes; never saves or sends a sticker.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerPreparationRequest {
    /// ID returned by the existing upload endpoint.
    pub source_id: String,
    /// Permit proportional resizing to the current sticker edge limit.
    #[serde(default)]
    pub fit: bool,
    /// Optional explicit segment, in milliseconds. Omission preserves the source duration.
    pub trim: Option<StickerPreparationTrim>,
}
/// Explicit segment retained when preparing an animation or video.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerPreparationTrim {
    /// Start of the retained segment in milliseconds.
    pub start_ms: u32,
    /// Duration of the retained segment in milliseconds, greater than zero.
    pub duration_ms: u32,
}

/// Output of a successful local sticker preparation job.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSticker {
    /// Preparation job ID; use it instead of resubmitting preparation.
    pub id: String,
    /// Uploaded source identity, retained while the operation needs it.
    pub source_id: String,
    /// Verified original GIF output, available through the normal media endpoint.
    pub original: crate::MediaAsset,
    /// Representative PNG suitable for model vision input.
    pub preview: crate::MediaAsset,
    /// PNG of sampled displayed frames; does not imply video playback by the model.
    pub contact_sheet: crate::MediaAsset,
    /// Prepared output width in pixels.
    pub width: u32,
    /// Prepared output height in pixels.
    pub height: u32,
    /// Sum of retained GIF delays in milliseconds.
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// Applied transformations, empty when original GIF bytes were retained.
    pub transformations: Vec<String>,
    /// Advisories about the prepared output; empty when none apply.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Local preparation expiry as Unix seconds; this is not a WeChat CDN expiry.
    #[ts(type = "number")]
    pub expires_at: i64,
}

/// One saved custom sticker read from the official profile.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomSticker {
    /// Official content identity.
    pub id: String,
    /// Optional official caption, not an editable local label.
    pub caption: Option<String>,
    /// Verified original and visual preview references when the individual item is read.
    pub original: Option<crate::MediaAsset>,
}
/// Saved membership and order read from one profile snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomStickerList {
    /// Items in picker order.
    pub items: Vec<CustomSticker>,
    /// Ordered-identity digest, returned as the HTTP entity tag.
    pub revision: String,
    /// Origin of the observation, currently `profile`.
    pub freshness: String,
}

/// Source to save into the account's one custom Saved collection.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum CustomStickerSource {
    /// Validated temporary output created by sticker preparation.
    Prepared {
        /// ID of a completed, unexpired preparation.
        #[serde(rename = "preparationId")]
        preparation_id: String,
    },
    /// A verified original attachment belonging to a received sticker message.
    Message {
        /// Existing message identity in this account.
        #[serde(rename = "messageId")]
        message_id: String,
        /// Original sticker asset belonging to that message.
        #[serde(rename = "assetId")]
        asset_id: String,
    },
}
/// Save a prepared or received sticker; this does not send a chat message.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddCustomStickerRequest {
    /// Owned source, validated before the asynchronous operation is admitted.
    pub source: CustomStickerSource,
}
/// Relative placement within Saved, avoiding stale numeric indexes.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomStickerPosition {
    /// Place immediately after this identity; null places the item first.
    pub after_id: Option<String>,
}
/// Move one Saved sticker using a collection revision precondition.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveCustomStickerRequest {
    /// Requested relative placement.
    pub position: CustomStickerPosition,
}

/// Explicit target for acquiring a Saved original, one pack item or one pack.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerDownloadRequest {
    /// Advertised pack icon, cover or thumbnail asset. Exclusive of pack/item IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asset_id: Option<String>,
    /// Stable item identity. Required for Saved; optional for an entire pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sticker_id: Option<String>,
    /// Product identity for one official pack. Omit for a Saved original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pack_id: Option<String>,
}
/// Request reconciliation of the account's custom Saved collection.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerSyncRequest {
    /// Collections to reconcile: `saved`, `added` or `all`.
    #[serde(default)]
    pub scope: StickerSyncScope,
}
/// Collection selected for a reconciliation operation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StickerSyncScope {
    /// The account's custom Saved collection.
    #[default]
    Saved,
    /// Every account-added official pack, independently of the public feed.
    Added,
    /// Saved and all account-added packs in one reconciliation job.
    All,
}

/// A derived composer collection; only Saved supports individual mutations.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerCollection {
    /// Stable picker identity, `saved` or a pack-prefixed identity.
    pub id: String,
    /// `saved` or `pack`.
    pub kind: String,
    /// Official display name; clients localize the Saved label themselves.
    pub title: String,
    /// Product identity for an added official pack.
    pub pack_id: Option<String>,
}
/// All current account-added picker collections, independent of catalog coverage.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerCollections {
    /// Saved followed by canonical added packs.
    pub items: Vec<StickerCollection>,
}
/// Public pack metadata with membership separate from verified media readiness.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerPack {
    /// Official product identity.
    pub id: String,
    /// Official name, falling back to identity when absent locally.
    pub name: String,
    /// Optional official display description.
    pub description: Option<String>,
    /// `added` or `not_added`, derived from the canonical account profile.
    pub membership: String,
    /// Media readiness; `unverified` does not mean that cached bytes are missing.
    pub content_readiness: String,
    /// Whether this remote observation is in the validated eligible-free class.
    /// Null means eligibility has not been established by this view.
    pub eligible: Option<bool>,
    /// Item count when a complete detail read supplied it.
    pub sticker_count: Option<u32>,
    /// Authenticated icon media reference, when indexed.
    pub icon: Option<crate::MediaAsset>,
    /// Authenticated cover media reference, when indexed.
    pub cover: Option<crate::MediaAsset>,
}
/// A bounded public-feed page or the complete canonical account-added list.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerPackList {
    /// Pack metadata in the view's order.
    pub items: Vec<StickerPack>,
    /// Opaque continuation for the available feed; null when complete.
    pub next_cursor: Option<String>,
    /// `available_feed` or `account_added`; neither claims a global inventory.
    pub coverage: String,
}
/// One item of an official pack. Captions are optional official metadata.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerPackSticker {
    /// Original content identity.
    pub id: String,
    /// Product identity.
    pub pack_id: String,
    /// One-based pack ordinal.
    pub ordinal: u32,
    /// Caption selected by exact requested locale then literal default fallback.
    pub caption: Option<String>,
    /// All supplied official locale/text pairs.
    pub captions: Vec<(String, String)>,
    /// Verified original metadata and authenticated media reference, when indexed.
    pub original: Option<crate::MediaAsset>,
    /// Thumbnail metadata and authenticated media reference, when indexed.
    pub thumbnail: Option<crate::MediaAsset>,
}
/// Complete supplied pack items, with no acquisition started by the read.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StickerPackStickers {
    /// Product identity.
    pub pack_id: String,
    /// Ordered pack contents.
    pub items: Vec<StickerPackSticker>,
}

/// Stable selection to send; it does not require downloading and re-uploading in a client.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "kind", rename_all = "snake_case")]
pub enum StickerSendSource {
    /// Existing member of the account's Saved collection.
    Saved {
        /// Stable sticker content identity.
        #[serde(rename = "stickerId")]
        sticker_id: String,
    },
    /// Existing member of an account-added official pack.
    Pack {
        /// Official product identity.
        #[serde(rename = "packId")]
        pack_id: String,
        /// Stable sticker content identity.
        #[serde(rename = "stickerId")]
        sticker_id: String,
    },
    /// Validated temporary output; sending does not save it to Saved.
    Prepared {
        /// Completed preparation identity.
        #[serde(rename = "preparationId")]
        preparation_id: String,
    },
}
/// Discriminator for a sticker request on the existing sends resource.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StickerSendKind {
    /// Send one sticker using a stable Saved, pack or preparation reference.
    Sticker,
}
/// Send one prepared or existing sticker through an asynchronous idempotent job.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StickerSendRequest {
    /// Must be `sticker`.
    pub kind: StickerSendKind,
    /// Intended chat identity.
    pub thread_id: String,
    /// Stable source selection, revalidated before submission.
    pub source: StickerSendSource,
}
