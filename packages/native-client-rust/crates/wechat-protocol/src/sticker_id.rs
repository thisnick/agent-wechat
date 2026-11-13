//! Validated identities shared by sticker callers and account operations.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
/// Canonical lowercase MD5 identity of one sticker original.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StickerId(String);
impl StickerId {
    /// Validate and normalize a 32-character hexadecimal content identity.
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref();
        ensure!(
            value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid_sticker_id"
        );
        Ok(Self(value.to_ascii_lowercase()))
    }
    /// Borrow the canonical content identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
/// Opaque official product identity, independent of account membership.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StickerPackId(String);
impl StickerPackId {
    /// Validate a nonempty bounded product identity without interpreting its spelling.
    pub fn parse(value: impl AsRef<str>) -> Result<Self> {
        let value = value.as_ref();
        ensure!(
            !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
            "invalid_sticker_pack_id"
        );
        Ok(Self(value.into()))
    }
    /// Borrow the original product identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
macro_rules! identity_traits {
    ($kind:ty) => {
        impl FromStr for $kind {
            type Err = anyhow::Error;
            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }
        impl TryFrom<String> for $kind {
            type Error = anyhow::Error;
            fn try_from(value: String) -> Result<Self> {
                Self::parse(value)
            }
        }
        impl From<$kind> for String {
            fn from(value: $kind) -> Self {
                value.0
            }
        }
        impl AsRef<str> for $kind {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl std::ops::Deref for $kind {
            type Target = str;
            fn deref(&self) -> &str {
                self.as_str()
            }
        }
        impl fmt::Display for $kind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
identity_traits!(StickerId);
identity_traits!(StickerPackId);
