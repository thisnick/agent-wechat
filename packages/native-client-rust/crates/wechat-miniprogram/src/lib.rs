//! Verified Mini Program archives and their supported compiled page entries.

use anyhow::{ensure, Context, Result};
use openssl::{
    hash::MessageDigest,
    pkcs5::pbkdf2_hmac,
    symm::{Cipher, Crypter, Mode},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ops::Range, sync::OnceLock};

mod compiled;
mod styles;

pub mod runner;

/// Maximum supported size of one archive.
pub const MAX_PACKAGE_BYTES: usize = 32 * 1024 * 1024;

/// Metadata for one package already staged in the account's cache.
#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CachedPackage {
    /// Display name for the known-app list.
    pub title: String,
    /// Numeric version of the package selected for this account.
    pub version: String,
    /// Identifier of the main package or subpackage.
    pub package_id: String,
    /// Digest of the original archive bytes.
    pub sha256: String,
    /// Default route within this package.
    pub entry_page: String,
    /// Relative path to the package's compiled service module.
    pub service_path: String,
    /// Allow this app to request a WeChat login code.
    #[serde(default)]
    pub allow_login: bool,
}

impl CachedPackage {
    /// Check metadata before using it to identify a package or launch a page.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty()
                && self.title.len() <= 256
                && !self.title.chars().any(char::is_control),
            "Invalid Mini Program title"
        );
        ensure!(
            (1..=20).contains(&self.version.len())
                && self.version.bytes().all(|b| b.is_ascii_digit()),
            "Invalid Mini Program package version"
        );
        ensure!(
            (1..=32).contains(&self.package_id.len())
                && self
                    .package_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "Invalid Mini Program package ID"
        );
        ensure!(
            self.sha256.len() == 64 && self.sha256.bytes().all(lower_hex),
            "Invalid Mini Program package digest"
        );
        ensure!(
            valid_path(&self.entry_page),
            "Invalid Mini Program page path"
        );
        ensure!(
            valid_path(&self.service_path)
                && self.service_path.rsplit('/').next() == Some("app-service.js"),
            "Invalid Mini Program service path"
        );
        Ok(())
    }
}

/// Check the public identifier used to select a Mini Program.
pub fn valid_app_id(value: &str) -> bool {
    value.len() == 18
        && value.starts_with("wx")
        && value.as_bytes()[2..].iter().copied().all(lower_hex)
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

/// Validate a card route and separate the query from the archive path.
pub fn split_page_path(value: &str) -> Option<(&str, &str)> {
    let (path, query) = value.split_once('?').unwrap_or((value, ""));
    let path = path.strip_prefix('/').unwrap_or(path);
    let path = path.strip_suffix(".html").unwrap_or(path);
    (valid_path(path) && query.len() <= 2048 && !query.chars().any(|c| c.is_control() || c == '#'))
        .then_some((path, query))
}

fn valid_path(path: &str) -> bool {
    (1..=240).contains(&path.len())
        && path.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
        })
}

/// Check a downloaded or cached archive against the selected package's digest.
pub fn verify_digest(bytes: &[u8], expected: &str) -> Result<()> {
    ensure!(
        (18..=MAX_PACKAGE_BYTES).contains(&bytes.len()),
        "Invalid Mini Program archive size"
    );
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == expected,
        "Mini Program archive digest mismatch"
    );
    Ok(())
}

/// A verified archive. File contents remain in memory and are never extracted to paths.
pub struct Archive {
    bytes: Vec<u8>,
    entries: BTreeMap<String, Option<Member>>,
}

struct Member {
    range: Range<usize>,
    service: OnceLock<Result<Box<compiled::ServiceIndex>, String>>,
}

/// The two factory expressions for a selected Glass-easel page.
pub struct CompiledPage<'a> {
    /// Factory for the registered page templates.
    pub templates: &'a str,
    /// Factory that registers the selected page with the runtime.
    pub definition: &'a str,
}

/// Registration pieces for a page compiled for the legacy WebView renderer.
pub struct LegacyPage<'a> {
    generator: &'a str,
    definition: &'a str,
}

#[derive(Clone, Copy)]
pub enum Framework {
    GlassEasel,
    Exparser,
}

impl Framework {
    /// Framework name in the launch contract.
    pub fn name(self) -> &'static str {
        match self {
            Self::GlassEasel => "glass-easel",
            Self::Exparser => "exparser",
        }
    }
}

impl Archive {
    /// Verify the original bytes and parse the selected cache format.
    pub fn open(app_id: &str, bytes: Vec<u8>, digest: &str) -> Result<Self> {
        ensure!(valid_app_id(app_id), "Invalid Mini Program AppID");
        verify_digest(&bytes, digest)?;
        let bytes = if bytes.starts_with(b"V1MMWX") {
            unwrap(app_id, &bytes)?
        } else {
            bytes
        };
        ensure!(
            bytes[0] == 0xbe && bytes[13] == 0xed,
            "Invalid Mini Program archive header"
        );
        let end = 14usize
            .checked_add(word(&bytes, 5)?)
            .context("Invalid Mini Program archive bounds")?;
        let count = word(&bytes, 14)?;
        ensure!(
            end >= 18 && end.checked_add(word(&bytes, 9)?) == Some(bytes.len()) && count <= 20000,
            "Invalid Mini Program archive bounds"
        );
        let mut entries = BTreeMap::new();
        let mut at = 18;
        for _ in 0..count {
            ensure!(at + 12 <= end, "Invalid Mini Program archive index");
            let length = word(&bytes, at)?;
            ensure!(
                (1..=4096).contains(&length),
                "Invalid Mini Program archive entry"
            );
            let name_end = at + 4 + length;
            ensure!(name_end + 8 <= end, "Invalid Mini Program archive entry");
            let name = std::str::from_utf8(&bytes[at + 4..name_end])?;
            let name = name.strip_prefix('/').unwrap_or(name);
            ensure!(
                !name.chars().any(|c| c == '\\' || c.is_control())
                    && name.split('/').all(|part| !matches!(part, "" | "." | "..")),
                "Unsafe Mini Program archive path"
            );
            let start = word(&bytes, name_end)?;
            let size = word(&bytes, name_end + 4)?;
            let stop = start
                .checked_add(size)
                .filter(|stop| *stop <= bytes.len())
                .context("Invalid Mini Program archive entry bounds")?;
            ensure!(
                size == 0 || start >= end,
                "Invalid Mini Program archive entry offset"
            );
            entries
                .entry(name.to_owned())
                .and_modify(|member| *member = None)
                .or_insert_with(|| {
                    Some(Member {
                        range: start..stop,
                        service: OnceLock::new(),
                    })
                });
            at = name_end + 8;
        }
        ensure!(at == end, "Invalid Mini Program archive table size");
        Ok(Self { bytes, entries })
    }

    /// Select a unique file by its archive path.
    pub fn file(&self, path: &str) -> Result<&[u8]> {
        Ok(&self.bytes[self.member(path)?.range.clone()])
    }

    fn member(&self, path: &str) -> Result<&Member> {
        self.entries
            .get(path)
            .context("Mini Program archive file is missing")?
            .as_ref()
            .context("Ambiguous Mini Program archive file")
    }

    fn service(&self, path: &str) -> Result<(&str, &compiled::ServiceIndex)> {
        let member = self.member(path)?;
        let source = std::str::from_utf8(&self.bytes[member.range.clone()])?;
        let index = member.service.get_or_init(|| {
            compiled::ServiceIndex::parse(source)
                .map(Box::new)
                .map_err(|error| error.to_string())
        });
        Ok((
            source,
            index
                .as_deref()
                .map_err(|error| anyhow::anyhow!("{error}"))?,
        ))
    }

    fn view_service(&self, service: &str) -> Result<(&str, &compiled::ServiceIndex)> {
        let view = service
            .strip_suffix("app-service.js")
            .context("Invalid service path")?
            .to_owned()
            + "app-wxss.js";
        if self.entries.contains_key(&view) {
            let (source, index) = self.service(&view)?;
            if !index.generators.is_empty() {
                return Ok((source, index));
            }
        }
        self.service(service)
    }

    /// Find the renderer that can load the compiled registration for this route.
    pub fn framework(&self, entry: &str, service: &str) -> Result<Framework> {
        if self.compiled_page(entry, service).is_ok() {
            Ok(Framework::GlassEasel)
        } else {
            self.legacy_page(entry, service)?;
            Ok(Framework::Exparser)
        }
    }

    fn legacy_page(&self, entry: &str, service: &str) -> Result<LegacyPage<'_>> {
        ensure!(valid_path(entry), "Invalid Mini Program page path");
        let (source, index) = self.service(service)?;
        let definition = page_factory(source, index, entry)?;
        let (_, index) = self.view_service(service)?;
        ensure!(index.prelude.is_some(), "Missing legacy compiler prelude");
        let path = format!("./{entry}.wxml");
        let fallback;
        let generator = if let Some(generator) = index.legacy.get(&path) {
            generator
        } else {
            let html = std::str::from_utf8(self.file(&format!("{entry}.html"))?)?;
            fallback = compiled::html_generator(html, &path)?;
            &fallback
        };
        let (generator, _) = index
            .generators
            .get_key_value(generator)
            .context("Missing compiled legacy template")?;
        Ok(LegacyPage {
            generator,
            definition,
        })
    }

    /// Resolve one page using the supported compiled Glass-easel registration.
    pub fn compiled_page(&self, entry: &str, service: &str) -> Result<CompiledPage<'_>> {
        ensure!(valid_path(entry), "Invalid Mini Program page path");
        let (source, index) = self.service(service)?;
        let templates = index
            .glass
            .get(entry)
            .context("Missing compiled page templates")?;
        Ok(CompiledPage {
            templates: &source[templates.clone()],
            definition: page_factory(source, index, entry)?,
        })
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<usize> {
    let bytes = bytes
        .get(offset..offset + 4)
        .context("Truncated Mini Program archive")?;
    Ok(u32::from_be_bytes(bytes.try_into()?) as usize)
}

fn page_factory<'a>(
    source: &'a str,
    index: &compiled::ServiceIndex,
    entry: &str,
) -> Result<&'a str> {
    let definition = index
        .modules
        .get(&format!("{entry}.js"))
        .filter(|module| module.is_page)
        .context("Missing compiled page definition")?;
    Ok(&source[definition.factory.clone()])
}

fn unwrap(app_id: &str, wrapped: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        wrapped.len() >= 1030,
        "Truncated wrapped Mini Program package"
    );
    let mut key = [0; 32];
    pbkdf2_hmac(
        app_id.as_bytes(),
        b"saltiest",
        1000,
        MessageDigest::sha1(),
        &mut key,
    )?;
    let mut cipher = Crypter::new(
        Cipher::aes_256_cbc(),
        Mode::Decrypt,
        &key,
        Some(b"the iv: 16 bytes"),
    )?;
    cipher.pad(false);
    let mut head = [0; 1040];
    let count = cipher.update(&wrapped[6..1030], &mut head)?;
    ensure!(
        count + cipher.finalize(&mut head[count..])? == 1024,
        "Invalid wrapped Mini Program header"
    );
    let mut bytes = head[..1023].to_vec();
    let mask = app_id.as_bytes()[16];
    bytes.extend(wrapped[1030..].iter().map(|byte| byte ^ mask));
    Ok(bytes)
}
