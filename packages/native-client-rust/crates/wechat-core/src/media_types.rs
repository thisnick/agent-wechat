//! Authoritative serde contract for native daemon media APIs and generated clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Largest source accepted by staging; send limits depend on the selected kind.
pub const MAX_SOURCE_BYTES: usize = wechat_media::MAX_FILE_BYTES;
pub const MAX_ORDINARY_SOURCE_BYTES: usize = 25 * 1024 * 1024 - 17;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    File,
    Video,
    Voice,
    Sticker,
}
impl MediaKind {
    pub fn max_source_bytes(self) -> usize {
        match self {
            Self::File => MAX_SOURCE_BYTES,
            Self::Image | Self::Video | Self::Voice => MAX_ORDINARY_SOURCE_BYTES,
            Self::Sticker => 0,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaVariant {
    #[default]
    Best,
    Thumbnail,
    Standard,
    Original,
    Normal,
    Encoded,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaTransferRequest {
    pub message_id: String,
    pub asset_id: Option<String>,
    #[serde(default)]
    pub quality: MediaVariant,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaSendRequest {
    pub thread_id: String,
    pub kind: MediaKind,
    pub source_id: String,
    pub standard_source_id: Option<String>,
    pub thumbnail_source_id: Option<String>,
    pub play_seconds: Option<u32>,
    pub duration_ms: Option<u32>,
}
/// A separately hashed still image. Its bytes never replace the source asset.
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaImagePreview {
    pub source_sha256: String,
    pub content_type: String,
    #[ts(type = "number")]
    pub byte_length: u64,
    pub sha256: String,
    pub filename: String,
    pub content_url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaAsset {
    pub id: String,
    pub account_id: String,
    pub message_id: Option<String>,
    pub kind: MediaKind,
    pub variant: MediaVariant,
    pub actual_variant: Option<MediaVariant>,
    pub state: String,
    pub content_type: Option<String>,
    #[ts(type = "number | null")]
    pub byte_length: Option<u64>,
    pub sha256: Option<String>,
    pub filename: Option<String>,
    #[ts(type = "number | null")]
    pub expires_at: Option<u64>,
    pub reason: Option<String>,
    pub retryable: bool,
    pub job_id: Option<String>,
    pub content_url: Option<String>,
    pub animation_frames: Option<u32>,
    pub official_writeback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<MediaImagePreview>,
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaJob {
    pub id: String,
    pub account_id: String,
    pub kind: String,
    pub state: String,
    pub phase: String,
    pub attempts: u32,
    pub cancel_requested: bool,
    pub retryable: bool,
    pub reason: Option<String>,
    pub asset_id: Option<String>,
    pub source_id: Option<String>,
    pub thread_id: Option<String>,
    pub server_message_id: Option<String>,
    #[ts(type = "number")]
    pub completed_bytes: u64,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    #[ts(type = "unknown")]
    pub result: Value,
    pub created_at: String,
    pub updated_at: String,
}
pub struct MediaContent {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub sha256: String,
    pub filename: Option<String>,
    pub source_sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_limit_does_not_expand_other_send_kinds() {
        assert_eq!(MAX_SOURCE_BYTES, 536_870_912);
        assert_eq!(MediaKind::File.max_source_bytes(), MAX_SOURCE_BYTES);
        for kind in [MediaKind::Image, MediaKind::Video, MediaKind::Voice] {
            assert_eq!(kind.max_source_bytes(), 26_214_383);
            assert!(kind.max_source_bytes() < MAX_SOURCE_BYTES);
        }
        assert_eq!(MediaKind::Sticker.max_source_bytes(), 0);
    }
}
