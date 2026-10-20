use crate::{
    CachedBytes, CachedImage, ImageQuality, ProfileError, ProfileReader, Result, SourceMessageId,
    VerifiedImageQuality,
};
use image::{ImageFormat, ImageReader, Limits};
use md5::{Digest, Md5};
use quick_xml::{events::Event, Reader};
use rusqlite::OptionalExtension;
use sha2::Sha256;
use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Read},
    path::{Component, Path},
};

pub(crate) fn validate_image(bytes: &[u8]) -> Result<(String, u32, u32)> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let format = reader.format().ok_or(ProfileError::InvalidMetadata)?;
    let mime = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        _ => return Err(ProfileError::InvalidMetadata),
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(20000);
    limits.max_image_height = Some(20000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| ProfileError::InvalidMetadata)?;
    Ok((mime.into(), decoded.width(), decoded.height()))
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && Path::new(s).components().count() == 1
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !s.contains(['\\', '\0'])
}
fn stem(s: &str) -> Option<String> {
    let s = s.strip_suffix(".dat").unwrap_or(s);
    let s = s
        .strip_suffix("_h")
        .or_else(|| s.strip_suffix("_t"))
        .unwrap_or(s);
    if s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()) {
        Some(s.to_ascii_lowercase())
    } else {
        None
    }
}
fn image_md5(xml: &str) -> Result<Option<String>> {
    if xml.len() > 16 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut reader = Reader::from_str(xml);
    let mut value = None;
    let mut count = 0;
    loop {
        match reader.read_event() {
            Ok(Event::DocType(_)) => return Err(ProfileError::InvalidMetadata),
            Ok(Event::Start(e) | Event::Empty(e)) => {
                count += 1;
                if count > 100000 {
                    return Err(ProfileError::ResourceLimit);
                }
                if e.name().as_ref() == b"img" {
                    for a in e.attributes() {
                        let a = a.map_err(|_| ProfileError::InvalidMetadata)?;
                        if a.key.as_ref() == b"md5" {
                            let s = std::str::from_utf8(a.value.as_ref())
                                .map_err(|_| ProfileError::InvalidMetadata)?
                                .to_ascii_lowercase();
                            if s.len() != 32
                                || !s.bytes().all(|c| c.is_ascii_hexdigit())
                                || value.replace(s).is_some()
                            {
                                return Err(ProfileError::InvalidMetadata);
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(ProfileError::InvalidMetadata),
        }
    }
    Ok(value)
}
impl ProfileReader {
    /// Resolve one exact local message through its advertised original hash and
    /// hardlink namespace. Ambiguous groups are rejected. `_h` is never assumed to
    /// mean Original. Standard/Thumbnail requests remain unsupported until their
    /// exact variant-role binding is verified for this adapter.
    pub fn cached_image(&self, id: &SourceMessageId, quality: ImageQuality) -> Result<CachedImage> {
        if matches!(quality, ImageQuality::Standard | ImageQuality::Thumbnail) {
            return Err(ProfileError::ImageQualityUnavailable);
        }
        if !crate::schema::message_table(&id.table) {
            return Err(ProfileError::InvalidQuery);
        }
        let local = id
            .local_id
            .parse::<i64>()
            .map_err(|_| ProfileError::InvalidQuery)?;
        let c = self.snapshot.open(&id.database)?;
        let q = crate::schema::quote_identifier(&id.table);
        let row = c
            .query_row(
                &format!("SELECT * FROM {q} WHERE local_id=?1"),
                [local],
                super::reader::raw,
            )
            .optional()?
            .ok_or(ProfileError::NotCached)?;
        if super::reader::num(&row, "local_type").map(|n| n & 0x7fffffff) != Some(3) {
            return Err(ProfileError::InvalidQuery);
        }
        let decoded = super::reader::decode(
            &row,
            "message_content",
            "WCDB_CT_message_content",
            self.snapshot.options.max_blob_bytes,
        )?;
        let hash = image_md5(
            decoded
                .text
                .as_deref()
                .ok_or(ProfileError::InvalidMetadata)?,
        )?
        .ok_or(ProfileError::NotCached)?;
        let hard = self
            .snapshot
            .inventory
            .iter()
            .filter(|d| d.relative_path.rsplit('/').next() == Some("hardlink.db"))
            .collect::<Vec<_>>();
        if hard.len() != 1 {
            return Err(ProfileError::NotCached);
        }
        let c = self.snapshot.open(&hard[0].relative_path)?;
        let mut s=c.prepare("SELECT a.username,b.username,h.file_name FROM image_hardlink_info_v4 h JOIN dir2id a ON a.rowid=h.dir1 JOIN dir2id b ON b.rowid=h.dir2 WHERE lower(h.md5)=?1 LIMIT 201")?;
        let rows = s
            .query_map([&hash], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > 200 {
            return Err(ProfileError::ResourceLimit);
        }
        let mut groups = BTreeSet::new();
        for (d1, d2, file) in rows {
            if !component(&d1) || !component(&d2) || !component(&file) {
                return Err(ProfileError::InvalidMetadata);
            }
            if let Some(base) = stem(&file) {
                groups.insert((d1, d2, base));
            }
        }
        if groups.len() != 1 {
            return Err(if groups.is_empty() {
                ProfileError::NotCached
            } else {
                ProfileError::InvalidMetadata
            });
        }
        let (d1, d2, base) = groups.into_iter().next().unwrap();
        let directory = self
            .snapshot
            .location
            .root
            .join("msg/attach")
            .join(d1)
            .join(d2)
            .join("Img");
        let mut current = self.snapshot.location.root.clone();
        let parts = directory
            .strip_prefix(&current)
            .map_err(|_| ProfileError::InvalidMetadata)?
            .components()
            .map(|p| p.as_os_str().to_owned())
            .collect::<Vec<_>>();
        for part in parts {
            current.push(part);
            let m = fs::symlink_metadata(&current).map_err(|_| ProfileError::NotCached)?;
            if !m.is_dir() || m.file_type().is_symlink() {
                return Err(ProfileError::UnsupportedFile);
            }
        }
        let mut variants = Vec::new();
        for suffix in ["", "_h", "_t"] {
            let p = directory.join(format!("{base}{suffix}.dat"));
            let m = match fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if !m.is_file() || m.file_type().is_symlink() {
                return Err(ProfileError::UnsupportedFile);
            }
            if m.len() > crate::MAX_DAT_PLAINTEXT_BYTES as u64 + 31 {
                return Err(ProfileError::ResourceLimit);
            }
            let before = m.modified()?;
            let mut data = Vec::with_capacity(m.len() as usize);
            fs::File::open(&p)?
                .take(crate::MAX_DAT_PLAINTEXT_BYTES as u64 + 32)
                .read_to_end(&mut data)?;
            if data.len() > crate::MAX_DAT_PLAINTEXT_BYTES + 31 {
                return Err(ProfileError::ResourceLimit);
            }
            let after = fs::metadata(&p)?;
            if before != after.modified()? || m.len() != after.len() {
                return Err(ProfileError::SourceChanged);
            }
            let (plain, _) = self.decode_dat(&data)?;
            let (mime, width, height) = validate_image(&plain)?;
            let original = format!("{:x}", Md5::digest(&plain)) == hash;
            if quality == ImageQuality::Original && !original {
                continue;
            }
            variants.push(CachedImage {
                media: CachedBytes {
                    media_type: mime,
                    sha256: hex::encode(Sha256::digest(&plain)),
                    bytes: plain,
                    source_database: id.database.clone(),
                    source_table: id.table.clone(),
                },
                quality: if original {
                    VerifiedImageQuality::Original
                } else {
                    VerifiedImageQuality::Unknown
                },
                width,
                height,
                cache_variant: if suffix.is_empty() {
                    "unsuffixed".into()
                } else {
                    suffix.into()
                },
            });
        }
        variants.sort_by_key(|v| {
            (
                v.quality == VerifiedImageQuality::Original,
                u64::from(v.width) * u64::from(v.height),
                v.media.bytes.len(),
            )
        });
        variants.pop().ok_or(ProfileError::NotCached)
    }
}
