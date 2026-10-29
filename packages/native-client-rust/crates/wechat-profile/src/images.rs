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
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::{Component, Path},
};

pub(crate) fn validate_image(bytes: &[u8]) -> Result<(String, u32, u32)> {
    if bytes.starts_with(b"wxgf")
        || bytes.starts_with(b"GIF8")
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
    {
        let inspection =
            wechat_media::inspect_image(bytes).map_err(|_| ProfileError::InvalidMetadata)?;
        return Ok((inspection.content_type, inspection.width, inspection.height));
    }
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
pub(crate) fn image_md5(xml: &str) -> Result<Option<String>> {
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
struct ImageResources {
    base: String,
    peer_directory: String,
    details: BTreeMap<String, u64>,
}
impl ProfileReader {
    fn image_resources(
        &self,
        id: &SourceMessageId,
        local: i64,
        row: &crate::RawFields,
    ) -> Result<Option<ImageResources>> {
        let databases = self
            .snapshot
            .inventory
            .iter()
            .filter(|d| d.relative_path.rsplit('/').next() == Some("message_resource.db"))
            .collect::<Vec<_>>();
        if databases.is_empty() {
            return Ok(None);
        }
        if databases.len() != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        let message = self.message(id, None)?;
        let peer = message.peer.ok_or(ProfileError::InvalidMetadata)?;
        let time = crate::reader::num(row, "create_time").ok_or(ProfileError::InvalidMetadata)?;
        let server = crate::reader::num(row, "server_id").ok_or(ProfileError::InvalidMetadata)?;
        let c = self.snapshot.open(&databases[0].relative_path)?;
        let rows=c.prepare("SELECT r.message_id,r.packed_info FROM MessageResourceInfo r JOIN ChatName2Id n ON n.rowid=r.chat_id WHERE n.user_name=?1 AND r.message_local_id=?2 AND r.message_create_time=?3 AND r.message_svr_id=?4 LIMIT 2")?.query_map(rusqlite::params![peer,local,time,server],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        let (resource, packed) = &rows[0];
        let mut bases = BTreeSet::new();
        for (field, wire, inner) in crate::reader::protobuf_fields(packed)? {
            if field == 2 && wire == 2 {
                for (f, w, b) in crate::reader::protobuf_fields(inner)? {
                    if f == 1 && w == 2 {
                        let base =
                            std::str::from_utf8(b).map_err(|_| ProfileError::InvalidMetadata)?;
                        if !crate::media::digest_hex(base, 32) {
                            return Err(ProfileError::InvalidMetadata);
                        }
                        bases.insert(base.to_ascii_lowercase());
                    }
                }
            }
        }
        if bases.len() != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        let mut details = BTreeMap::new();
        for row in c
            .prepare("SELECT type,size,status FROM MessageResourceDetail WHERE message_id=?1")?
            .query_map([resource], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
        {
            let (kind, size, status) = row?;
            let suffix = match kind {
                65537 => "_h",
                131073 => "",
                262145 => "_t",
                _ => continue,
            };
            if status == 1 && size > 0 && details.insert(suffix.to_owned(), size as u64).is_some() {
                return Err(ProfileError::InvalidMetadata);
            }
        }
        Ok(Some(ImageResources {
            base: bases.pop_first().ok_or(ProfileError::InvalidMetadata)?,
            peer_directory: format!("{:x}", Md5::digest(peer.as_bytes())),
            details,
        }))
    }
    /// Resolve one exact local message through its advertised original hash and
    /// hardlink namespace. Ambiguous groups are rejected. Standard/Thumbnail
    /// require the exact resource basename/type/status/encoded-size join. A suffix
    /// alone is not a quality proof; Original also requires the XML content MD5.
    pub fn cached_image(&self, id: &SourceMessageId, quality: ImageQuality) -> Result<CachedImage> {
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
        let resource = self.image_resources(id, local, &row)?;
        if matches!(quality, ImageQuality::Standard | ImageQuality::Thumbnail) && resource.is_none()
        {
            return Err(ProfileError::ImageQualityUnavailable);
        }
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
        let query = if resource.is_some() {
            "SELECT a.username,b.username,h.file_name FROM image_hardlink_info_v4 h JOIN dir2id a ON a.rowid=h.dir1 JOIN dir2id b ON b.rowid=h.dir2 WHERE a.username=?1 AND h.file_name IN(?2,?3,?4) LIMIT 201"
        } else {
            "SELECT a.username,b.username,h.file_name FROM image_hardlink_info_v4 h JOIN dir2id a ON a.rowid=h.dir1 JOIN dir2id b ON b.rowid=h.dir2 WHERE lower(h.md5)=?1 LIMIT 201"
        };
        let mut s = c.prepare(query)?;
        let args = if let Some(r) = &resource {
            vec![
                r.peer_directory.clone(),
                format!("{}.dat", r.base),
                format!("{}_h.dat", r.base),
                format!("{}_t.dat", r.base),
            ]
        } else {
            vec![hash.clone()]
        };
        let rows = s
            .query_map(rusqlite::params_from_iter(args), |r| {
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
            if !m.is_dir() || wechat_platform::is_link(&m) {
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
            if !m.is_file() || wechat_platform::is_link(&m) {
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
            let role = if resource.as_ref().is_some_and(|r| {
                r.base == base && r.details.get(suffix).copied() == Some(data.len() as u64)
            }) {
                match suffix {
                    "" => VerifiedImageQuality::Standard,
                    "_t" => VerifiedImageQuality::Thumbnail,
                    _ => VerifiedImageQuality::Unknown,
                }
            } else {
                VerifiedImageQuality::Unknown
            };
            // Equal bytes can legitimately occupy multiple advertised roles. Honor
            // an explicit resource-bound role before comparing original content.
            let verified = match quality {
                ImageQuality::Standard if role == VerifiedImageQuality::Standard => role,
                ImageQuality::Thumbnail if role == VerifiedImageQuality::Thumbnail => role,
                _ if original => VerifiedImageQuality::Original,
                _ => role,
            };
            if match quality {
                ImageQuality::Original => !original,
                ImageQuality::Standard => verified != VerifiedImageQuality::Standard,
                ImageQuality::Thumbnail => verified != VerifiedImageQuality::Thumbnail,
                ImageQuality::Best => false,
            } {
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
                quality: verified,
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

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn legacy_jpeg_png_profile_dimension_limits_are_unchanged() {
        for format in [ImageFormat::Jpeg, ImageFormat::Png] {
            let mut encoded = Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(5000, 1)
                .write_to(&mut encoded, format)
                .unwrap();
            let (_, width, height) = validate_image(encoded.get_ref()).unwrap();
            assert_eq!((width, height), (5000, 1));
        }
    }

    #[test]
    fn additional_image_formats_use_shared_bounded_inspection() {
        let webp = include_bytes!("../tests/fixtures/two-frame.webp");
        assert_eq!(validate_image(webp).unwrap(), ("image/webp".into(), 12, 8));
        let wxgf = include_bytes!("../tests/fixtures/static.wxgf");
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            assert_eq!(
                validate_image(wxgf).unwrap(),
                ("image/x-wxgf".into(), 32, 24)
            );
        } else {
            assert!(validate_image(wxgf).is_err());
        }
    }
}
