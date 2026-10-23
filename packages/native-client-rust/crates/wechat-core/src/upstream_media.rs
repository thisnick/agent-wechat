//! Decoded media transport keeps preparation, receipts and writeback in Core.
use anyhow::{bail, Result};
use wechat_protocol::media::{
    CdnClient, DownloadedMedia, MediaDescriptor, PreparedImageUpload, PreparedUpload,
    PreparedVideoUpload, UploadedImage, UploadedMedia, UploadedVideo,
};

pub type Progress<'a> = &'a dyn Fn(u64, Option<u64>) -> Result<()>;

pub trait MediaTransport {
    fn native(&self) -> Result<&CdnClient> {
        bail!("unsupported_simulator_operation")
    }
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia>;
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia>;
    fn upload_image(&self, prepared: &PreparedImageUpload) -> Result<UploadedImage>;
    fn upload_video(
        &self,
        prepared: &PreparedVideoUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedVideo>;
}
impl MediaTransport for CdnClient {
    fn native(&self) -> Result<&CdnClient> {
        Ok(self)
    }
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia> {
        self.download_with_progress(descriptor, progress)
    }
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia> {
        self.upload_with_progress(prepared, progress)
    }
    fn upload_image(&self, prepared: &PreparedImageUpload) -> Result<UploadedImage> {
        self.upload_image(prepared)
    }
    fn upload_video(
        &self,
        prepared: &PreparedVideoUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedVideo> {
        self.upload_video_with_progress(prepared, progress)
    }
}

pub enum MediaBody<'a> {
    File {
        upload: &'a UploadedMedia,
        reservation: Option<&'a wechat_protocol::file_upload::FileUploadReservation>,
    },
    Image(&'a UploadedImage),
    Video {
        upload: &'a UploadedVideo,
        seconds: u32,
    },
    Voice {
        encoded: &'a [u8],
        duration_ms: u32,
    },
}
pub struct MediaSubmission<'a> {
    pub destination: &'a str,
    pub client_id: &'a str,
    pub created_at: u32,
    pub body: MediaBody<'a>,
}
