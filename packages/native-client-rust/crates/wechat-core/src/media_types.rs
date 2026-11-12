//! Authoritative serde contract for native daemon media APIs and generated clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Largest source accepted by staging; send limits depend on the selected kind.
pub const MAX_SOURCE_BYTES: usize = wechat_media::MAX_FILE_BYTES;
pub const MAX_ORDINARY_SOURCE_BYTES: usize = 25 * 1024 * 1024 - 17;
pub const MAX_VIDEO_SOURCE_BYTES: usize = wechat_media::MAX_VIDEO_SOURCE_BYTES;
/// The kind of an attachment.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, TS, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A photo or other still image.
    Image,
    /// Any file. A file is sent with its bytes unchanged.
    File,
    /// A video.
    Video,
    /// A voice note.
    Voice,
    /// A sticker. Stickers can be received and downloaded but not sent.
    Sticker,
}
impl MediaKind {
    pub fn max_source_bytes(self) -> usize {
        match self {
            Self::File => MAX_SOURCE_BYTES,
            Self::Video => MAX_VIDEO_SOURCE_BYTES,
            Self::Image | Self::Voice => MAX_ORDINARY_SOURCE_BYTES,
            Self::Sticker => 0,
        }
    }
}
/// Which copy of an attachment, or which quality to request. For a request, `best` picks the best
/// copy available.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, TS, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum MediaVariant {
    /// The best copy available. It downloads one when none is cached. Only valid in a request.
    #[default]
    Best,
    /// A small preview copy.
    Thumbnail,
    /// The standard-size copy of an image.
    Standard,
    /// The original copy as offered by the sender, which can be smaller than the sender's own
    /// source.
    Original,
    /// The ordinary playback copy of a video, or the playable audio of a voice note.
    Normal,
    /// A voice note in its original encoded form.
    Encoded,
}
/// Request to download an attachment of a message.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaTransferRequest {
    /// ID of the message to download from. It must match the message ID in the URL.
    pub message_id: String,
    /// ID of the specific attachment to download. When null, the daemon picks the attachment that
    /// matches `quality`.
    pub asset_id: Option<String>,
    /// Which copy to download. The default is `best`.
    #[serde(default)]
    pub quality: MediaVariant,
}
/// How a video is sent: `normal` sends a prepared playback copy and a thumbnail, and `original`
/// also includes the unchanged source video.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, TS, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum VideoSendQuality {
    /// Send a prepared playback copy and a thumbnail.
    #[default]
    Normal,
    /// Also include the unchanged source video.
    Original,
}
/// How the original copy of an image is prepared. The standard and thumbnail copies are always
/// compact previews.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, TS, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageSendQuality {
    /// Reduce the original copy to at most 1,280 pixels and 65,520 bytes.
    #[default]
    Standard,
    /// Send a JPEG source unchanged as the original copy, or convert another format to JPEG at
    /// its full resolution.
    Original,
}
/// Request to send an uploaded file, image, video or voice note. Upload the bytes first, then send
/// with the returned source ID.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaSendRequest {
    /// ID of the chat to send to.
    pub thread_id: String,
    /// What to send. Stickers cannot be sent.
    pub kind: MediaKind,
    /// ID of the uploaded source to send, as returned by the upload route.
    pub source_id: String,
    /// ID of an uploaded JPEG to use as the standard-size copy of an image instead of generating
    /// one. Only valid for image sends.
    pub standard_source_id: Option<String>,
    /// ID of an uploaded JPEG to use as the thumbnail instead of generating one. Only valid for
    /// image and video sends.
    pub thumbnail_source_id: Option<String>,
    /// Length of a video in seconds, from 1 to 3600. Optional, because the daemon measures the
    /// video itself.
    pub play_seconds: Option<u32>,
    /// Length of a voice note in milliseconds. Optional, because the daemon measures the audio
    /// itself.
    pub duration_ms: Option<u32>,
    /// How to send a video, `normal` by default or `original`. Only valid for video sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub video_quality: Option<VideoSendQuality>,
    /// `original` sends the image at full resolution as its original copy; `standard`, the
    /// default, reduces it. Only valid for image sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_quality: Option<ImageSendQuality>,
}
/// A separately hashed still image. Its bytes never replace the source asset.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaImagePreview {
    /// SHA-256 of the original image bytes this preview was made from.
    pub source_sha256: String,
    /// MIME type of the preview.
    pub content_type: String,
    /// Size of the preview in bytes.
    #[ts(type = "number")]
    pub byte_length: u64,
    /// SHA-256 of the preview bytes, as lowercase hex.
    pub sha256: String,
    /// File name of the preview.
    pub filename: String,
    /// Path on the daemon to fetch the preview from.
    pub content_url: String,
}
/// One copy of a message's attachment, and whether its bytes have been downloaded and checked.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaAsset {
    /// Attachment ID, used to fetch or request this copy.
    pub id: String,
    /// ID of the account the attachment belongs to.
    pub account_id: String,
    /// ID of the message the attachment belongs to. Null when it is not tied to a message.
    pub message_id: Option<String>,
    /// What kind of attachment this is.
    pub kind: MediaKind,
    /// Which copy this is, as advertised by the message.
    pub variant: MediaVariant,
    /// Which copy the cached bytes really are, once checked. Null until bytes are cached.
    pub actual_variant: Option<MediaVariant>,
    /// `pending` (not downloaded yet), `available` (downloaded and checked), `expired` (WeChat no
    /// longer has it) or `unsupported`.
    pub state: String,
    /// MIME type of the cached bytes. Null until they are cached.
    pub content_type: Option<String>,
    /// Size in bytes of the cached bytes that were checked. Null until they are cached.
    #[ts(type = "number | null")]
    pub byte_length: Option<u64>,
    /// Size in bytes the sender advertised, which can differ from the checked size. Omitted when
    /// the sender gave none, which does not mean the file is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub advertised_byte_length: Option<u64>,
    /// True when the account's current download setting allows this copy to be downloaded
    /// automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_download_allowed: Option<bool>,
    /// SHA-256 of the cached bytes, as lowercase hex. Null until they are cached.
    pub sha256: Option<String>,
    /// File name of the attachment, when it has one. Null otherwise.
    pub filename: Option<String>,
    /// When WeChat's copy expires, as Unix time in seconds. Null when WeChat gave no expiry.
    #[ts(type = "number | null")]
    pub expires_at: Option<u64>,
    /// Machine-readable code explaining the state, such as `media_expired`. Null when there is
    /// nothing to explain.
    pub reason: Option<String>,
    /// True when asking for the download again can still succeed.
    pub retryable: bool,
    /// ID of the job downloading or sending this attachment. Null when there is none.
    pub job_id: Option<String>,
    /// Path on the daemon to fetch the cached bytes from. Null until the attachment is available.
    pub content_url: Option<String>,
    /// Number of frames when the attachment is an animated image. Null otherwise.
    pub animation_frames: Option<u32>,
    /// Whether the downloaded copy was also saved into the official profile's own media folders:
    /// `complete`, `pending`, `blocked`, or `unsupported` for media that is never saved there.
    pub official_writeback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<MediaImagePreview>,
}
/// A background upload or download of media, with its progress. Poll it by ID until it finishes.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaJob {
    /// Job ID.
    pub id: String,
    /// ID of the account the job belongs to.
    pub account_id: String,
    /// `send` for an upload to a chat, or `download` for fetching a received attachment.
    pub kind: String,
    /// Where the job stands: `queued`, `running`, `available` (download finished), `accepted`
    /// (WeChat took the send), `needs_review` (the outcome is unknown, so check before sending
    /// again), `cancelled`, `expired`, `unsupported` or `failed`.
    pub state: String,
    /// Current step: `queued`, `preparing`, `downloading`, `reserving`, `uploading`, `submitting`,
    /// `readback` or `completed`.
    pub phase: String,
    /// How many times the job has been started. A failed job can be retried only while this is
    /// below 3.
    pub attempts: u32,
    /// True when a cancel has been requested for the job.
    pub cancel_requested: bool,
    /// True when the job can be retried.
    pub retryable: bool,
    /// Error code of the last failure. Null when there has been none.
    pub reason: Option<String>,
    /// ID of the attachment being downloaded. Null for sends.
    pub asset_id: Option<String>,
    /// ID of the uploaded source being sent. Null for downloads.
    pub source_id: Option<String>,
    /// ID of the chat a send goes to. Null for downloads.
    pub thread_id: Option<String>,
    /// WeChat's ID of the sent message once WeChat accepted it. Null until then.
    pub server_message_id: Option<String>,
    /// Bytes transferred so far.
    #[ts(type = "number")]
    pub completed_bytes: u64,
    /// Total bytes to transfer, when known. Null otherwise.
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
    /// Details of the outcome. Its fields depend on the job, and it is null until there is
    /// something to report.
    #[ts(type = "unknown")]
    pub result: Value,
    /// When the job was created, as an RFC 3339 time.
    pub created_at: String,
    /// When the job last changed, as an RFC 3339 time.
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
        assert_eq!(MediaKind::Video.max_source_bytes(), MAX_SOURCE_BYTES);
        for kind in [MediaKind::Image, MediaKind::Voice] {
            assert_eq!(kind.max_source_bytes(), 26_214_383);
            assert!(kind.max_source_bytes() < MAX_SOURCE_BYTES);
        }
        assert_eq!(MediaKind::Sticker.max_source_bytes(), 0);
    }
}
