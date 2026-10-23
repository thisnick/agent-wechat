//! Bounded image inspection shared by native transport and official profiles.
//! Source bytes, including application trailers, are never rewritten here.
//! WXGF inspection decodes a representative still only. Its animation timing,
//! frame count and alpha semantics remain unknown; the original stays WXGF.

mod image;
mod wxgf;

pub use image::ImageInspection;
pub use wxgf::wxgf_preview;

pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// Native ordinary-file plaintext limit, shared by transport and local storage.
pub const MAX_FILE_BYTES: usize = 512 * 1024 * 1024;
/// Initial source/original-video limit; normal playback is prepared separately.
pub const MAX_VIDEO_SOURCE_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug)]
pub struct DecodedImage {
    pub bytes: Vec<u8>,
    pub inspection: ImageInspection,
}

/// Inspect the retained source. For WXGF, dimensions describe its still preview;
/// `frames` does not establish the source's animation frame count.
pub fn inspect_image(bytes: &[u8]) -> anyhow::Result<ImageInspection> {
    if bytes.starts_with(b"wxgf") {
        let mut inspection = wxgf_preview(bytes)?.inspection;
        inspection.content_type = "image/x-wxgf".into();
        inspection.animation_preserved = false;
        return Ok(inspection);
    }
    image::inspect_standard_image(bytes)
}
