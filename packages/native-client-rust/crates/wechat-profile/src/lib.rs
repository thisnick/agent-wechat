//! Readers and narrow opt-in writers for selected official Linux profiles.
//!
//! Offline readers use a private temporary copy, including its WAL, without
//! changing source SHM. An owned daemon can instead reuse read-only connections
//! while serializing profile operations. This crate neither launches WeChat nor connects
//! to the network or acquires keys from a process. Checked write functions require
//! explicit ownership and a durable caller intent.
mod dat;
mod images;
mod initialize;
mod keys;
mod login_storage;
mod media;
mod mmkv;
mod model;
mod owned_reader;
mod projection;
mod reader;
mod recall_search;
mod recalls;
mod schema;
mod snapshot;
mod thread_read;
mod writer;

pub use dat::*;
pub use initialize::*;
pub use keys::*;
pub use login_storage::*;
pub use media::*;
pub use mmkv::*;
pub use model::*;
pub use owned_reader::OwnedReaderCache;
pub use projection::*;
pub use reader::*;
pub use recall_search::*;
pub use recalls::*;
pub use schema::*;
pub use snapshot::{keys_from_agent_cache, keys_from_password, ProfileLocation, ReadOptions};
pub use thread_read::*;
pub use writer::*;

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profile source is unavailable or not an account root")]
    InvalidProfile,
    #[error("a stopped profile or consistent snapshot must be explicitly confirmed")]
    SnapshotRequired,
    #[error("profile changed while the read snapshot was being copied")]
    SourceChanged,
    #[error("source contains an unsupported link or special file")]
    UnsupportedFile,
    #[error("profile resource exceeds a configured bound")]
    ResourceLimit,
    #[error("protected keys file is missing")]
    KeysMissing,
    #[error("keys file must be a private regular file")]
    InsecureKeysFile,
    #[error("invalid or unsupported keys file")]
    InvalidKeysFile,
    #[error("keys are bound to a different account directory")]
    AccountMismatch,
    #[error("no authorized key is available for database {0}")]
    KeyUnavailable(String),
    #[error("supplied key or database generation did not validate for {0}")]
    KeyMismatch(String),
    #[error("validated database could not be opened for {0}")]
    DatabaseOpen(String),
    #[error("the database engine does not provide SQLCipher support")]
    SqlcipherUnavailable,
    #[error("unsupported database schema for {0}")]
    UnsupportedSchema(String),
    #[error("official-profile writes are not supported by this adapter")]
    WritesUnsupported,
    #[error("invalid or query-mismatched continuation cursor")]
    InvalidCursor,
    #[error("continuation source generation changed; start a new query")]
    CursorExpired,
    #[error("invalid query or identifier")]
    InvalidQuery,
    #[error("requested bytes are not cached")]
    NotCached,
    #[error("requested local image quality is not established by this adapter")]
    ImageQualityUnavailable,
    #[error("local image key is unavailable")]
    ImageKeyUnavailable,
    #[error("invalid or unsupported local V2 DAT payload")]
    InvalidDat,
    #[error("corrupt or unsupported encoded metadata")]
    InvalidMetadata,
    #[error("profile filesystem operation failed")]
    Io(#[from] std::io::Error),
    // Do not expose SQLCipher error SQL/parameters or row content to callers.
    #[error("profile database operation failed")]
    Database,
}

impl From<rusqlite::Error> for ProfileError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}

pub type Result<T> = std::result::Result<T, ProfileError>;
