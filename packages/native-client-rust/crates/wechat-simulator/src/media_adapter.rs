use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use wechat_core::upstream::media::{MediaBody, MediaSubmission, MediaTransport, Progress};
use wechat_protocol::{
    crypto,
    media::{
        DownloadedMedia, MediaDescriptor, MediaQuality, PreparedImageUpload, PreparedUpload,
        PreparedVariant, PreparedVideoUpload, UploadedImage, UploadedMedia, UploadedVideo,
    },
    media_send::{MediaSendResult, SendStatus},
};

pub(super) struct SimulatorMedia {
    upstream: SimulatorUpstream,
    session: Session,
}
impl SimulatorMedia {
    pub fn new(upstream: SimulatorUpstream, session: Session) -> Self {
        Self { upstream, session }
    }
    fn request<T: DeserializeOwned>(&self, request: MediaRequest) -> Result<T> {
        self.upstream.rpc(&Request::Media {
            session: self.session.clone(),
            request,
        })
    }
    fn upload_object(
        &self,
        prepared: &PreparedUpload,
        quality: MediaQuality,
        variant: Option<&PreparedVariant>,
        dimensions: Option<(u32, u32)>,
    ) -> Result<UploadedMedia> {
        let (encrypted, size, md5, sha256) = match variant {
            Some(v) => (&v.encrypted, v.plaintext_bytes, &v.md5, &v.sha256),
            None => (
                &prepared.encrypted,
                prepared.plaintext_bytes,
                &prepared.md5,
                &prepared.sha256,
            ),
        };
        let metadata = UploadedMedia {
            descriptor: MediaDescriptor {
                file_id: format!("simm_{}", prepared.file_key),
                aes_key: prepared.aes_key.clone(),
                quality,
                expected_bytes: Some(size.into()),
                expected_md5: Some(md5.clone()),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: size,
            md5: md5.clone(),
            sha256: sha256.clone(),
            file_name: prepared.file_name.clone(),
            width: dimensions.map(|v| v.0),
            height: dimensions.map(|v| v.1),
        };
        self.request(MediaRequest::Upload {
            metadata,
            encrypted: STANDARD.encode(encrypted),
        })
    }
}
impl MediaTransport for SimulatorMedia {
    fn download(
        &self,
        descriptor: &MediaDescriptor,
        progress: Progress<'_>,
    ) -> Result<DownloadedMedia> {
        progress(0, descriptor.expected_bytes)?;
        let encoded: String = self.request(MediaRequest::Download {
            descriptor: descriptor.clone(),
        })?;
        let bytes = STANDARD.decode(encoded)?;
        let md5 = hex(&crypto::md5(&bytes));
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= wechat_core::MAX_SOURCE_BYTES
                && descriptor
                    .expected_bytes
                    .is_none_or(|size| size == bytes.len() as u64)
                && descriptor
                    .expected_md5
                    .as_ref()
                    .is_none_or(|hash| hash.eq_ignore_ascii_case(&md5)),
            "media_integrity_failed"
        );
        progress(bytes.len() as u64, Some(bytes.len() as u64))?;
        Ok(DownloadedMedia {
            sha256: hex(&crypto::sha256(&bytes)),
            bytes,
            md5,
            requested_quality: descriptor.quality,
            substituted_filetype: None,
            picture_format: None,
            advertised_size_matches: Some(true),
        })
    }
    fn upload(&self, prepared: &PreparedUpload, progress: Progress<'_>) -> Result<UploadedMedia> {
        progress(0, Some(prepared.encrypted.len() as u64))?;
        let result = self.upload_object(prepared, prepared.kind, None, None)?;
        progress(
            prepared.encrypted.len() as u64,
            Some(prepared.encrypted.len() as u64),
        )?;
        Ok(result)
    }
    fn upload_image(&self, p: &PreparedImageUpload) -> Result<UploadedImage> {
        Ok(UploadedImage {
            original: self.upload_object(
                &p.original,
                MediaQuality::Original,
                None,
                Some((p.width, p.height)),
            )?,
            standard: self.upload_object(
                &p.original,
                MediaQuality::Standard,
                Some(&p.standard),
                Some((p.standard.width, p.standard.height)),
            )?,
            thumbnail: self.upload_object(
                &p.original,
                MediaQuality::Thumbnail,
                Some(&p.thumbnail),
                Some((p.thumbnail.width, p.thumbnail.height)),
            )?,
        })
    }
    fn upload_video(
        &self,
        p: &PreparedVideoUpload,
        progress: Progress<'_>,
    ) -> Result<UploadedVideo> {
        let video = self.upload(&p.video, progress)?;
        let thumbnail = self.upload_object(
            &p.video,
            MediaQuality::Thumbnail,
            Some(&p.thumbnail),
            Some((p.thumbnail.width, p.thumbnail.height)),
        )?;
        Ok(UploadedVideo { video, thumbnail })
    }
}
pub(super) fn submit(
    upstream: &SimulatorUpstream,
    session: Session,
    input: MediaSubmission<'_>,
) -> Result<MediaSendResult> {
    let own = format!("sim_{}", session.account);
    let (message_type, content, voice) = match input.body {
        MediaBody::File(upload) => (
            49,
            wechat_protocol::media_send::local_file_xml(&own, upload)?,
            None,
        ),
        MediaBody::Image(upload) => (
            3,
            wechat_protocol::media_send::local_image_xml(upload)?,
            None,
        ),
        MediaBody::Video { upload, seconds } => (
            43,
            wechat_protocol::media_send::local_video_xml(upload, seconds)?,
            None,
        ),
        MediaBody::Voice {
            encoded,
            duration_ms,
        } => {
            wechat_protocol::media_send::VoiceUpload::new(
                input.destination,
                input.client_id,
                input.created_at,
                duration_ms,
                encoded,
            )?;
            (
                34,
                wechat_protocol::media_send::local_voice_xml(encoded.len(), duration_ms)?,
                Some(STANDARD.encode(encoded)),
            )
        }
    };
    let reply: SendReply = upstream.rpc(&Request::Media {
        session,
        request: MediaRequest::Submit {
            destination: input.destination.into(),
            client_id: input.client_id.into(),
            created_at: input.created_at,
            message_type,
            content,
            voice,
        },
    })?;
    Ok(MediaSendResult {
        status: if reply.accepted {
            SendStatus::ServerAccepted
        } else {
            SendStatus::Rejected
        },
        service_code: if reply.accepted { 0 } else { -1 },
        server_message_id: reply.server_id,
        client_id_matches: Some(true),
    })
}
pub(super) fn voice(
    upstream: &SimulatorUpstream,
    session: Session,
    server_id: u64,
    length: u32,
    offset: u32,
) -> Result<wechat_protocol::wire::VoiceChunk> {
    let encoded: String = upstream.rpc(&Request::Media {
        session,
        request: MediaRequest::Voice {
            server_id,
            length,
            offset,
        },
    })?;
    let bytes = STANDARD.decode(encoded)?;
    let next_offset = offset
        .checked_add(u32::try_from(bytes.len())?)
        .context("invalid_voice_offset")?;
    ensure!(next_offset <= length, "media_integrity_failed");
    Ok(wechat_protocol::wire::VoiceChunk {
        offset,
        next_offset,
        total_length: length,
        complete: next_offset == length,
        bytes,
        echoed_server_id: Some(server_id.to_string()),
        offset_matches: true,
    })
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn sticker(
    upstream: &SimulatorUpstream,
    session: Session,
    descriptor: &wechat_protocol::media::StickerDescriptor,
    progress: Progress<'_>,
) -> Result<(Vec<u8>, wechat_protocol::media::ImageInspection)> {
    wechat_protocol::media::validate_sticker_url(&descriptor.url)?;
    ensure!(
        descriptor.expected_bytes <= 16 * 1024 * 1024,
        "sticker_metadata_invalid"
    );
    progress(0, Some(descriptor.expected_bytes))?;
    let encoded: String = upstream.rpc(&Request::Media {
        session,
        request: MediaRequest::Sticker {
            descriptor: descriptor.clone(),
        },
    })?;
    let bytes = STANDARD.decode(encoded)?;
    ensure!(
        bytes.len() as u64 == descriptor.expected_bytes
            && hex(&crypto::md5(&bytes)).eq_ignore_ascii_case(&descriptor.expected_md5),
        "media_integrity_failed"
    );
    let inspection = wechat_protocol::media::inspect_image(&bytes)?;
    progress(bytes.len() as u64, Some(bytes.len() as u64))?;
    Ok((bytes, inspection))
}
