//! Bounded media inspection shared by native transport and official profiles.
//! Inspection retains source bytes, including application trailers. Explicit
//! sticker preparation returns a separately validated output and previews.
//! WXGF inspection decodes a representative still only. Its animation timing,
//! frame count and alpha semantics remain unknown; the original stays WXGF.

mod image;
mod sticker_archive;
mod sticker_export;
mod sticker_video;
mod stickers;
mod video;
mod wxgf;

pub use image::ImageInspection;
pub use sticker_archive::*;
pub use sticker_export::*;
pub use sticker_video::prepare_sticker;
pub use stickers::*;
pub use video::inspect_video_container;
pub use wxgf::wxgf_preview;

pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
/// Native ordinary-file plaintext limit, shared by transport and local storage.
pub const MAX_FILE_BYTES: usize = 512 * 1024 * 1024;
/// Outgoing video source limit, shared by normal preparation and the unchanged
/// original upload. It matches the ordinary-file limit; the prepared normal
/// playback copy keeps its own smaller output limit.
pub const MAX_VIDEO_SOURCE_BYTES: usize = MAX_FILE_BYTES;

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
