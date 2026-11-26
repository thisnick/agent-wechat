//! Decoded media transport keeps preparation, receipts and writeback in Core.
use crate::async_bridge::block_on;
use anyhow::{bail, Result};
use std::sync::Arc;
use wechat_protocol::{
    media::{
        AsyncDownloadSink, CdnClient, CdnConfig, DownloadReceipt, DownloadedMedia, MediaDescriptor,
        PreparedAsyncUpload, PreparedImageUpload, PreparedUpload, PreparedVideoUpload,
        UploadedImage, UploadedMedia, UploadedVideo,
    },
    transport::{Clock, HttpTransport, ProtocolTransport, TransportFuture},
};

pub type Progress<'a> = &'a dyn Fn(u64, Option<u64>) -> Result<()>;

pub trait MediaTransport {
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia>;
    /// Download an incoming voice note over CDN (`filetype = 15`), bounded to
    /// [`wechat_protocol::media::MAX_VOICE_CDN_BYTES`].
    fn download_voice_cdn(
        &self,
        _descriptor: &wechat_protocol::media::VoiceCdnDescriptor,
        _progress: Progress<'_>,
    ) -> Result<Vec<u8>> {
        bail!("unsupported_simulator_operation")
    }
    /// Download into `sink` with the same checks as [`Self::download`]. The
    /// bytes in `sink` are unverified until this returns `Ok`. A transport that
    /// cannot stream downloads whole and then writes.
    fn download_to(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
        sink: &mut (dyn std::io::Write + Send),
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
        prepared: &PreparedAsyncUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload(&block_on(prepared.to_memory())?, progress)
    }
    /// Upload an ordinary file through the multi-phase large-file route.
    fn upload_large_file(
        &self,
        _prepared: &PreparedAsyncUpload,
        _task_id: u64,
        _progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        bail!("unsupported_simulator_operation")
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
        _prepared: &PreparedUpload,
        _task_id: u64,
        _progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        bail!("unsupported_simulator_operation")
    }
    /// [`Self::upload_video_original`] for an original read from a file.
    fn upload_video_original_file(
        &self,
        prepared: &PreparedAsyncUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        self.upload_video_original(&block_on(prepared.to_memory())?, task_id, progress)
    }
}

/// The account's CDN on the native async transport: the shared pooled HTTP
/// client (system resolver, as for the server-signed CDN addresses) and clock.
pub struct NativeMediaTransport {
    cdn: CdnClient,
    http: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

/// Per-request CDN deadline for daemon media transfers.
const CDN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

impl NativeMediaTransport {
    /// The account's CDN from its `config`, on `transport`'s HTTP client and
    /// clock.
    pub fn new(config: CdnConfig, transport: &ProtocolTransport) -> Result<Self> {
        Ok(Self {
            cdn: CdnClient::new(config, CDN_TIMEOUT)?,
            http: transport.http.clone(),
            clock: transport.clock.clone(),
        })
    }

    /// Download a storage object (sticker pack archives) as the official
    /// client does.
    pub fn download_storage(
        &self,
        descriptor: &MediaDescriptor,
        file_key: &str,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia> {
        block_on(self.cdn.download_storage_with_progress_async(
            descriptor,
            file_key,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
}

/// Writes decrypted CDN pieces to a synchronous writer. Core waits for the
/// transfer on a blocking worker, so the writes may block.
pub(crate) struct WriteSink<'a>(pub(crate) &'a mut (dyn std::io::Write + Send));

impl AsyncDownloadSink for WriteSink<'_> {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> TransportFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0.write_all(buf)?;
            Ok(())
        })
    }

    fn flush<'a>(&'a mut self) -> TransportFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0.flush()?;
            Ok(())
        })
    }
}

impl MediaTransport for NativeMediaTransport {
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia> {
        block_on(self.cdn.download_with_progress_async(
            descriptor,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn download_voice_cdn(
        &self,
        descriptor: &wechat_protocol::media::VoiceCdnDescriptor,
        progress: Progress<'_>,
    ) -> Result<Vec<u8>> {
        block_on(self.cdn.download_voice_with_progress_async(
            descriptor,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn download_to(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
        sink: &mut (dyn std::io::Write + Send),
    ) -> Result<DownloadReceipt> {
        block_on(self.cdn.download_to_with_progress_async(
            descriptor,
            &*self.http,
            &*self.clock,
            progress,
            &mut WriteSink(sink),
        ))
    }
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia> {
        block_on(
            self.cdn
                .upload_with_progress_async(prepared, &*self.http, &*self.clock, progress),
        )
    }
    fn upload_file(
        &self,
        prepared: &PreparedAsyncUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        block_on(self.cdn.upload_source_with_progress_async(
            prepared,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn upload_large_file(
        &self,
        prepared: &PreparedAsyncUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        block_on(self.cdn.upload_large_file_from_source_with_progress_async(
            prepared,
            task_id,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn upload_image(
        &self,
        prepared: &PreparedImageUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedImage> {
        block_on(self.cdn.upload_image_with_progress_async(
            prepared,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn upload_video(
        &self,
        prepared: &PreparedVideoUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedVideo> {
        block_on(self.cdn.upload_video_with_progress_async(
            prepared,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn upload_video_original(
        &self,
        prepared: &PreparedUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        block_on(self.cdn.upload_video_original_with_progress_async(
            prepared,
            task_id,
            &*self.http,
            &*self.clock,
            progress,
        ))
    }
    fn upload_video_original_file(
        &self,
        prepared: &PreparedAsyncUpload,
        task_id: u64,
        progress: Progress<'_>,
    ) -> Result<UploadedMedia> {
        block_on(
            self.cdn
                .upload_video_original_from_source_with_progress_async(
                    prepared,
                    task_id,
                    &*self.http,
                    &*self.clock,
                    progress,
                ),
        )
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
