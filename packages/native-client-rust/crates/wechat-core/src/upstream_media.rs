//! Decoded media transport keeps preparation, receipts and writeback in Core.
use anyhow::{bail, Result};
use wechat_protocol::media::{
    CdnClient, DownloadReceipt, DownloadedMedia, MediaDescriptor, PreparedFileUpload,
    PreparedImageUpload, PreparedUpload, PreparedVideoUpload, UploadedImage, UploadedMedia,
    UploadedVideo,
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
    /// Download into `sink` with the same checks as [`Self::download`]. The
    /// bytes in `sink` are unverified until this returns `Ok`. A transport that
    /// cannot stream downloads whole and then writes.
    fn download_to(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
        sink: &mut dyn std::io::Write,
    ) -> Result<DownloadReceipt> {
        let (bytes, receipt) = self.download(descriptor, progress)?.into_parts();
        sink.write_all(&bytes)?;
        Ok(receipt)
    }
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia>;
    /// [`Self::upload`] for an upload read from a file. A transport that sends
    /// one object per request reads it into memory first.
    fn upload_file(
        &self,
        prepared: &PreparedFileUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload(&prepared.to_memory()?, progress)
    }
    fn upload_image(
        &self,
        prepared: &PreparedImageUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedImage>;
    fn upload_video(
        &self,
        prepared: &PreparedVideoUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedVideo>;
    fn upload_video_original(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.native()?
            .upload_video_original_with_progress(prepared, task_id, progress)
    }
    /// [`Self::upload_video_original`] for an original read from a file.
    fn upload_video_original_file(
        &self,
        prepared: &PreparedFileUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload_video_original(&prepared.to_memory()?, task_id, progress)
    }
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
    fn download_to(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
        sink: &mut dyn std::io::Write,
    ) -> Result<DownloadReceipt> {
        self.download_to_with_progress(descriptor, progress, sink)
    }
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia> {
        self.upload_with_progress(prepared, progress)
    }
    fn upload_file(
        &self,
        prepared: &PreparedFileUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload_file_with_progress(prepared, progress)
    }
    fn upload_video_original_file(
        &self,
        prepared: &PreparedFileUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload_video_original_from_file_with_progress(prepared, task_id, progress)
    }
    fn upload_image(
        &self,
        prepared: &PreparedImageUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedImage> {
        self.upload_image_with_progress(prepared, progress)
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
