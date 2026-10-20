use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Local IDs are scoped to a database *and* a table. Decimal strings avoid a
/// lossy JSON floating-point conversion and are not native server checkpoints.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceMessageId {
    pub database: String,
    pub table: String,
    pub local_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "storageType", content = "value", rename_all = "camelCase")]
pub enum SqlValue {
    Null,
    Integer(String),
    // Strings preserve non-finite SQLite doubles, which JSON numbers cannot
    // represent, as well as finite round-trip values and negative zero.
    Real(String),
    Text(String),
    TextBytes(String),
    Blob(String),
}
pub type RawFields = BTreeMap<String, SqlValue>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedField {
    pub text: Option<String>,
    pub state: DecodeState,
    pub compression_flag: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecodeState {
    Null,
    Utf8,
    Zstd,
    InvalidUtf8,
    UnsupportedCompression,
    InvalidCompressedData,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: SourceMessageId,
    pub server_message_id: Option<String>,
    pub server_id_signed: Option<String>,
    pub peer: Option<String>,
    pub sender: Option<String>,
    pub outgoing: Option<bool>,
    pub message_type: i64,
    pub local_type: String,
    pub create_time: i64,
    pub sort_seq: Option<String>,
    pub content: DecodedField,
    pub source: DecodedField,
    pub raw_fields: RawFields,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub username: String,
    pub local_id: String,
    pub label: String,
    pub nickname: Option<String>,
    pub remark: Option<String>,
    pub alias: Option<String>,
    pub local_type: Option<i64>,
    pub is_group: bool,
    pub raw_fields: RawFields,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub username: String,
    pub label: String,
    pub unread_count: Option<i64>,
    pub hidden: Option<bool>,
    pub sort_timestamp: Option<i64>,
    pub last_timestamp: Option<i64>,
    pub raw_fields: RawFields,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub username: String,
    pub local_id: String,
    pub label: String,
    pub owner: Option<String>,
    pub announcement: Option<String>,
    pub raw_fields: RawFields,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMember {
    pub username: String,
    pub local_id: String,
    pub label: String,
    pub room_display_name: Option<String>,
    pub contact: Contact,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedBytes {
    pub media_type: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
    pub source_database: String,
    pub source_table: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageQuality {
    Best,
    Original,
    Standard,
    Thumbnail,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedImage {
    pub media: CachedBytes,
    /// Original requires advertised content MD5. Standard/Thumbnail require an
    /// exact native resource join; a filename suffix alone remains Unknown.
    pub quality: VerifiedImageQuality,
    pub width: u32,
    pub height: u32,
    pub cache_variant: String,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerifiedImageQuality {
    Original,
    Standard,
    Thumbnail,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInventory {
    pub relative_path: String,
    pub bytes: u64,
    pub plaintext: bool,
    pub has_wal: bool,
    pub generation: String,
    pub state: DatabaseState,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseState {
    Readable,
    KeyUnavailable,
    KeyMismatch,
    UnsupportedSchema,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub partial: bool,
    pub databases: Vec<DatabaseInventory>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub source_generation: String,
    pub coverage: Coverage,
}

#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub search: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MessageQuery {
    pub chat: Option<String>,
    pub own_username: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub message_type: Option<i64>,
}
