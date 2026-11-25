//! Incoming voice metadata parsing, format detection, multi-codec WAV decoding,
//! and structured non-secret failure diagnostics.
use super::*;
use std::fmt;

/// Parsed attributes from a type-34 `<msg><voicemsg .../></msg>` payload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VoiceXmlMeta {
    pub length: Option<u32>,
    pub voice_format: Option<u32>,
    pub end_flag: Option<u32>,
    pub cancel_flag: Option<u32>,
}

/// Detected container/codec format of raw voice bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VoiceFormatKind {
    Silk,
    OpusRawV1,
    Amr,
    Wav,
    Mp3,
    Speex,
    Unknown,
}

impl VoiceFormatKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Silk => "silk",
            Self::OpusRawV1 => "opus_raw_v1",
            Self::Amr => "amr",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Speex => "speex",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Silk => "audio/silk",
            Self::OpusRawV1 => "audio/opus",
            Self::Amr => "audio/amr",
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::Speex => "audio/x-speex",
            Self::Unknown => "application/octet-stream",
        }
    }
}

/// Non-secret diagnostic facts recorded on a failed voice download job.
/// Carries no credentials, keys, URLs, or audio payload bytes.
#[derive(Clone, Debug)]
pub(crate) struct VoiceDownloadFailure {
    pub stage: &'static str,
    pub transport: &'static str,
    pub cdn_fallback: bool,
    pub advertised_bytes: u32,
    pub received_bytes: usize,
    pub chunk_count: usize,
    pub offset_mismatch: bool,
    pub voice_format: Option<u32>,
    pub detected_format: &'static str,
    pub reason: String,
}

impl fmt::Display for VoiceDownloadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("voice_download_failed")
    }
}

impl std::error::Error for VoiceDownloadFailure {}

/// Strip an optional `<sender>:\n` chatroom prefix and parse `<voicemsg>` attributes.
pub(crate) fn parse_voice_xml(content: &str, peer: Option<&str>) -> Option<VoiceXmlMeta> {
    if content.len() > 1024 * 1024 || content.contains('\0') {
        return None;
    }
    let mut xml = content.trim_start();
    if !xml.starts_with('<')
        && peer.is_some_and(|p| p.ends_with("@chatroom") || p.ends_with("@im.chatroom"))
    {
        if let Some((prefix, rest)) = xml.split_once(":\n") {
            if !prefix.is_empty()
                && prefix.len() <= 128
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(&b))
            {
                xml = rest.trim_start();
            }
        }
    }
    if xml.to_ascii_uppercase().contains("<!DOCTYPE")
        || xml.to_ascii_uppercase().contains("<!ENTITY")
    {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    let node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "voicemsg")?;
    let attr_u32 = |name: &str| -> Option<u32> {
        node.attribute(name)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .and_then(|v| v.parse::<u32>().ok())
    };
    Some(VoiceXmlMeta {
        length: attr_u32("length"),
        voice_format: attr_u32("voiceformat"),
        end_flag: attr_u32("endflag"),
        cancel_flag: attr_u32("cancelflag"),
    })
}

/// Detect the container format of raw voice bytes using magic bytes first, then XML hints.
pub(crate) fn detect_voice_format(bytes: &[u8], voice_format_hint: Option<u32>) -> VoiceFormatKind {
    if wechat_protocol::silk::is_silk(bytes) {
        VoiceFormatKind::Silk
    } else if bytes.starts_with(wechat_calls::opus::RAW_OPUS_V1_MAGIC) {
        VoiceFormatKind::OpusRawV1
    } else if bytes.starts_with(b"#!AMR\n") || bytes.starts_with(b"#!AMR-WB\n") {
        VoiceFormatKind::Amr
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        VoiceFormatKind::Wav
    } else if bytes.starts_with(b"ID3") || voice_format_hint == Some(2) {
        VoiceFormatKind::Mp3
    } else if voice_format_hint == Some(0) {
        VoiceFormatKind::Amr
    } else if voice_format_hint == Some(1) {
        VoiceFormatKind::Speex
    } else {
        VoiceFormatKind::Unknown
    }
}

/// Prepend `#!AMR\n` to raw AMR frames when `voiceformat == 0` and the header is missing,
/// matching the official client's voice storage behavior.
pub(crate) fn normalize_encoded_voice(bytes: Vec<u8>, voice_format_hint: Option<u32>) -> Vec<u8> {
    if voice_format_hint == Some(0)
        && !bytes.is_empty()
        && !bytes.starts_with(b"#!AMR")
        && !wechat_protocol::silk::is_silk(&bytes)
        && !bytes.starts_with(wechat_calls::opus::RAW_OPUS_V1_MAGIC)
    {
        let mut out = Vec::with_capacity(6 + bytes.len());
        out.extend_from_slice(b"#!AMR\n");
        out.extend_from_slice(&bytes);
        return out;
    }
    bytes
}

/// Validate `#!OPUS_RAW_V1` header and complete packet framing to exact EOF.
pub(crate) fn validate_raw_opus_v1(bytes: &[u8]) -> bool {
    if bytes.len() < 18
        || !bytes.starts_with(wechat_calls::opus::RAW_OPUS_V1_MAGIC)
        || bytes[13] > 4
        || !matches!(bytes[14], 1 | 2)
        || bytes[15] != 0
    {
        return false;
    }
    let mut pos = 16usize;
    let mut packets = 0usize;
    while pos < bytes.len() {
        if bytes.len() - pos < 2 {
            return false;
        }
        let len = usize::from(u16::from_le_bytes([bytes[pos], bytes[pos + 1]]));
        pos += 2;
        if len > wechat_calls::opus::MAX_OPUS_PACKET_BYTES || len > bytes.len() - pos {
            return false;
        }
        pos += len;
        packets += 1;
        if packets > 15_000 {
            return false;
        }
    }
    packets > 0
}

/// Validate RFC 4867 AMR-NB or AMR-WB frame boundaries to exact EOF.
pub(crate) fn validate_amr_framing(bytes: &[u8], raw_nb_allowed: bool) -> bool {
    const NB_SIZES: [Option<usize>; 16] = [
        Some(12),
        Some(13),
        Some(15),
        Some(17),
        Some(19),
        Some(20),
        Some(26),
        Some(31),
        Some(5),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(0),
    ];
    const WB_SIZES: [Option<usize>; 16] = [
        Some(17),
        Some(23),
        Some(32),
        Some(36),
        Some(40),
        Some(46),
        Some(50),
        Some(58),
        Some(60),
        Some(5),
        None,
        None,
        None,
        None,
        Some(0),
        Some(0),
    ];
    let (mut pos, sizes) = if bytes.starts_with(b"#!AMR\n") {
        (6usize, &NB_SIZES)
    } else if bytes.starts_with(b"#!AMR-WB\n") {
        (9usize, &WB_SIZES)
    } else if raw_nb_allowed && !bytes.is_empty() {
        (0usize, &NB_SIZES)
    } else {
        return false;
    };
    let mut frames = 0usize;
    while pos < bytes.len() {
        let toc = bytes[pos];
        if (toc & 0x83) != 0 {
            return false;
        }
        let ft = usize::from((toc >> 3) & 0x0f);
        let Some(payload_len) = sizes[ft] else {
            return false;
        };
        pos += 1;
        if payload_len > bytes.len() - pos {
            return false;
        }
        pos += payload_len;
        frames += 1;
        if frames > 15_000 {
            return false;
        }
    }
    frames > 0
}

/// Whether inline voice bytes from an `AddMsg` payload are usable without a network fetch.
/// Matches the advertised length or carries a structurally complete voice stream.
pub(crate) fn is_usable_inline_voice(
    bytes: &[u8],
    advertised_length: u32,
    voice_format_hint: Option<u32>,
) -> bool {
    if bytes.len() <= 1 || bytes.len() > 16 * 1024 * 1024 {
        return false;
    }
    if bytes.len() == advertised_length as usize {
        return true;
    }
    if wechat_protocol::silk::validate_framing(bytes).is_ok() {
        return true;
    }
    if validate_raw_opus_v1(bytes) {
        return true;
    }
    if validate_amr_framing(bytes, voice_format_hint == Some(0)) {
        return true;
    }
    false
}

/// Validate raw encoded voice bytes before persisting to write-once `VoiceInfo` on
/// `--quality encoded`. Known containers must pass full framing validation; unknown
/// containers are rejected when the CGI transfer ended early before advertised length.
pub(crate) fn validate_encoded_voice(
    bytes: &[u8],
    voice_format_hint: Option<u32>,
    early_eof: bool,
    from_profile: bool,
) -> Result<()> {
    ensure!(!bytes.is_empty(), "empty_voice_payload");
    match detect_voice_format(bytes, voice_format_hint) {
        VoiceFormatKind::Silk => {
            wechat_protocol::silk::validate_framing(bytes)
                .map_err(|_| anyhow::anyhow!("media_integrity_failed"))?;
        }
        VoiceFormatKind::OpusRawV1 => {
            ensure!(validate_raw_opus_v1(bytes), "media_integrity_failed");
        }
        VoiceFormatKind::Amr => {
            ensure!(
                validate_amr_framing(bytes, from_profile),
                "media_integrity_failed"
            );
        }
        VoiceFormatKind::Wav => {
            ensure!(
                bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
                "media_integrity_failed"
            );
            let riff_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
            ensure!(
                riff_len >= 36 && riff_len.saturating_add(8) <= bytes.len(),
                "media_integrity_failed"
            );
        }
        VoiceFormatKind::Mp3 | VoiceFormatKind::Speex | VoiceFormatKind::Unknown => {
            ensure!(!early_eof, "media_integrity_failed");
        }
    }
    Ok(())
}

/// Encode signed 16-bit interleaved PCM samples into a standard RIFF/WAVE container.
pub(crate) fn pcm_to_wav(samples: &[i16], sample_rate: u32, channels: u16) -> Result<Vec<u8>> {
    ensure!(
        !samples.is_empty()
            && sample_rate > 0
            && matches!(channels, 1 | 2)
            && samples.len() <= (sample_rate as usize) * usize::from(channels) * 300,
        "voice_duration_limit"
    );
    let data_bytes = u32::try_from(samples.len().saturating_mul(2))?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(channels))
        .and_then(|v| v.checked_mul(2))
        .context("invalid_wav_format")?;
    let block_align = channels * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend(b"RIFF");
    out.extend((36 + data_bytes).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(channels.to_le_bytes());
    out.extend(sample_rate.to_le_bytes());
    out.extend(byte_rate.to_le_bytes());
    out.extend(block_align.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_bytes.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    Ok(out)
}

fn decode_via_ffmpeg_to_wav(
    bytes: &[u8],
    input_formats: &str,
    cancelled: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    let raw = super::run_ffmpeg(
        bytes,
        "voice.pcm",
        input_formats,
        &[
            "-vn", "-ac", "1", "-ar", "16000", "-f", "s16le", "-t", "301",
        ],
        cancelled,
    )?;
    ensure!(
        !raw.is_empty() && raw.len() % 2 == 0 && raw.len() <= 300 * 16000 * 2,
        "voice_duration_limit"
    );
    let pcm: Vec<i16> = raw
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]))
        .collect();
    pcm_to_wav(&pcm, 16000, 1)
}

/// Decode downloaded raw voice bytes (SILK, `#!OPUS_RAW_V1`, AMR, WAV, MP3, or
/// other FFmpeg-readable containers) into a playable PCM16 WAV payload.
pub(crate) fn decode_voice_to_wav(
    bytes: &[u8],
    voice_format_hint: Option<u32>,
    cancelled: impl FnMut() -> Result<()>,
) -> Result<Vec<u8>> {
    ensure!(!bytes.is_empty(), "empty_voice_payload");
    match detect_voice_format(bytes, voice_format_hint) {
        VoiceFormatKind::Silk => {
            let pcm = wechat_protocol::silk::decode(bytes)?;
            wechat_protocol::silk::wav(&pcm)
        }
        VoiceFormatKind::OpusRawV1 => {
            let decoded = wechat_calls::opus::decode_raw_opus_v1(bytes)
                .map_err(|_| anyhow::anyhow!("opus_decode_failed"))?;
            pcm_to_wav(&decoded.samples, decoded.sample_rate, decoded.channels)
        }
        VoiceFormatKind::Wav => {
            if super::pcm16k_wav(bytes)?.is_some() {
                Ok(bytes.to_vec())
            } else {
                decode_via_ffmpeg_to_wav(bytes, "wav", cancelled)
            }
        }
        VoiceFormatKind::Amr => {
            let normalized = normalize_encoded_voice(bytes.to_vec(), Some(0));
            decode_via_ffmpeg_to_wav(&normalized, "amr", cancelled)
        }
        VoiceFormatKind::Mp3 => decode_via_ffmpeg_to_wav(bytes, "mp3", cancelled),
        VoiceFormatKind::Speex => bail!("unsupported_voice_codec"),
        VoiceFormatKind::Unknown => decode_via_ffmpeg_to_wav(
            bytes,
            "wav,mp3,flac,ogg,aac,mov,matroska,webm,caf,amr",
            cancelled,
        )
        .map_err(|error| {
            let classified = super::classify_error(&error);
            if matches!(
                classified.as_str(),
                "media_cancelled"
                    | "media_disconnected"
                    | "auth_required"
                    | "media_worker_stopping"
                    | "media_encoder_unavailable"
                    | "voice_duration_limit"
            ) {
                error
            } else {
                anyhow::anyhow!("voice_decode_failed")
            }
        }),
    }
}

/// Serialize structured voice failure facts into `media_jobs.data.result.voiceDownloadFailure`.
pub(super) fn voice_download_failure(error: &anyhow::Error) -> Option<serde_json::Value> {
    let failure = error.downcast_ref::<VoiceDownloadFailure>().or_else(|| {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<VoiceDownloadFailure>())
    })?;
    Some(json!({
        "stage": failure.stage,
        "transport": failure.transport,
        "cdnFallback": failure.cdn_fallback,
        "advertisedBytes": failure.advertised_bytes,
        "receivedBytes": failure.received_bytes,
        "chunkCount": failure.chunk_count,
        "offsetMismatch": failure.offset_mismatch,
        "voiceFormat": failure.voice_format,
        "detectedFormat": failure.detected_format,
        "reason": failure.reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn sample_silk_with_dtx_and_eof() -> Vec<u8> {
        let pcm: Vec<i16> = (0..3200)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 8000.0) as i16)
            .collect();
        let encoded = wechat_protocol::silk::encode(&pcm).unwrap();
        let mut out = encoded[..10].to_vec();
        out.extend([0u8, 0]);
        out.extend(&encoded[10..]);
        out.extend([0u8, 0]);
        out.extend([0xffu8, 0xff]);
        out
    }

    fn sample_opus_raw_v1() -> Vec<u8> {
        use wechat_calls::audio::{AudioEncoder, AudioFormat};
        use wechat_calls::opus::{OpusEncoder, RAW_OPUS_V1_MAGIC};
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let mut encoder = OpusEncoder::new(format, 32000).unwrap();
        let frame: Vec<i16> = (0..320)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 8000.0) as i16)
            .collect();
        let mut out = Vec::from(*RAW_OPUS_V1_MAGIC);
        out.extend([2u8, 1, 0]); // 16 kHz, mono, reserved=0
        for _ in 0..5 {
            let pkt = encoder.encode(&frame).unwrap();
            out.extend((pkt.len() as u16).to_le_bytes());
            out.extend(pkt);
        }
        out
    }

    struct VoiceTestUpstream {
        chunks: Mutex<Vec<Result<wechat_protocol::wire::VoiceChunk>>>,
        cdn_bytes: Mutex<Option<Result<Vec<u8>>>>,
        cgi_calls: AtomicUsize,
        cdn_calls: AtomicUsize,
    }

    impl VoiceTestUpstream {
        fn new(chunks: Vec<Result<wechat_protocol::wire::VoiceChunk>>) -> Self {
            Self {
                chunks: Mutex::new(chunks),
                cdn_bytes: Mutex::new(None),
                cgi_calls: AtomicUsize::new(0),
                cdn_calls: AtomicUsize::new(0),
            }
        }
        fn with_cdn(mut self, cdn: Result<Vec<u8>>) -> Self {
            self.cdn_bytes = Mutex::new(Some(cdn));
            self
        }
    }

    struct StubMediaTransport {
        result: Result<Vec<u8>>,
    }

    impl crate::upstream::media::MediaTransport for StubMediaTransport {
        fn download(
            &self,
            _descriptor: &MediaDescriptor,
            _progress: crate::upstream::media::Progress<'_>,
        ) -> Result<wechat_protocol::media::DownloadedMedia> {
            bail!("unused")
        }
        fn download_voice_cdn(
            &self,
            _descriptor: &wechat_protocol::media::VoiceCdnDescriptor,
            _progress: crate::upstream::media::Progress<'_>,
        ) -> Result<Vec<u8>> {
            match &self.result {
                Ok(bytes) => Ok(bytes.clone()),
                Err(error) => {
                    if let Some(rejected) = wechat_protocol::ServiceRejected::find(error) {
                        return Err(anyhow::Error::new(*rejected));
                    }
                    bail!("{error}")
                }
            }
        }
        fn upload(
            &self,
            _prepared: &wechat_protocol::media::PreparedUpload,
            _progress: crate::upstream::media::Progress<'_>,
        ) -> Result<UploadedMedia> {
            bail!("unused")
        }
        fn upload_image(
            &self,
            _prepared: &PreparedImageUpload,
            _progress: crate::upstream::media::Progress<'_>,
        ) -> Result<UploadedImage> {
            bail!("unused")
        }
        fn upload_video(
            &self,
            _prepared: &PreparedVideoUpload,
            _progress: crate::upstream::media::Progress<'_>,
        ) -> Result<UploadedVideo> {
            bail!("unused")
        }
    }

    impl crate::upstream::Upstream for VoiceTestUpstream {
        fn kind(&self) -> crate::upstream::UpstreamKind {
            crate::upstream::UpstreamKind::Native
        }
        fn media_transport_route(
            &self,
            _state: &mut wechat_protocol::AccountState,
            _route: Option<LargeCdnRoute>,
        ) -> Result<Box<dyn crate::upstream::media::MediaTransport>> {
            self.cdn_calls.fetch_add(1, Ordering::SeqCst);
            let Some(res) = self.cdn_bytes.lock().unwrap().take() else {
                bail!("cdn_unavailable");
            };
            Ok(Box::new(StubMediaTransport { result: res }))
        }
        fn download_voice(
            &self,
            _state: &mut wechat_protocol::AccountState,
            _server_id: u64,
            _length: u32,
            _offset: u32,
        ) -> Result<wechat_protocol::wire::VoiceChunk> {
            self.cgi_calls.fetch_add(1, Ordering::SeqCst);
            let mut chunks = self.chunks.lock().unwrap();
            if chunks.is_empty() {
                bail!("unexpected_cgi_call");
            }
            chunks.remove(0)
        }
    }

    fn voice_fixture(
        upstream: Arc<VoiceTestUpstream>,
        xml: &str,
        server_id: &str,
    ) -> (tempfile::TempDir, Arc<Core>, String, Vec<MediaAsset>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let core = Core::with_upstream(store, None, upstream).unwrap();
        let mut state = super::super::tests::accepted_media_state();
        state.device.device_id = vec![b'V'; 16];
        state.uin = 7;
        state.cookie = vec![1; 15];
        state.auto_auth_key = wechat_protocol::protobuf::message(&[
            wechat_protocol::protobuf::bytes(1, &wechat_protocol::protobuf::buffer(b"k1")),
            wechat_protocol::protobuf::bytes(2, &wechat_protocol::protobuf::buffer(b"k2")),
        ])
        .unwrap();
        state.key_info = vec![4; 180];
        state.database_password = vec![42; 32];
        core.test_managed_profile("one", &state).unwrap();
        let source = core
            .project_accepted_media(
                "one",
                "wxid_peer",
                34,
                xml,
                "[Voice]",
                1_700_000_001,
                server_id,
                &format!("voice-{server_id}"),
            )
            .unwrap();
        let message_id = crate::message_id(&source).unwrap();
        let assets = core.media_assets("one", &message_id).unwrap();
        (dir, core, message_id, assets)
    }

    fn run_download(core: &Core, asset_id: &str, job_id: &str) -> Result<()> {
        core.store
            .create_media_job(
                "one",
                job_id,
                &format!("download:{job_id}"),
                &digest(job_id.as_bytes()),
                "download",
                &json!({"assetId": asset_id}),
                b"",
            )
            .unwrap();
        let outcome = core.perform_download("one", job_id);
        if let Err(ref error) = outcome {
            core.record_media_failure_diagnostics("one", job_id, error)
                .unwrap();
            core.fail_media_job("one", job_id, &super::classify_error(error))
                .unwrap();
        }
        outcome
    }

    #[test]
    fn voice_download_handles_dtx_silk_offset_mismatch_and_short_transcoded_opus() {
        let opus = sample_opus_raw_v1();
        let advertised = (opus.len() + 200) as u32;
        let xml = format!(
            "<msg><voicemsg voiceformat='7' length='{advertised}' voicelength='100' endflag='1'/></msg>"
        );
        // Simulate server returning shorter transcoded Opus with mismatched f2 offset
        // followed by 3 consecutive empty chunks (Android VoiceDownloadTask EOF rule).
        let upstream = Arc::new(VoiceTestUpstream::new(vec![
            Ok(wechat_protocol::wire::VoiceChunk {
                offset: 0,
                next_offset: opus.len() as u32,
                total_length: advertised,
                complete: false,
                bytes: opus.clone(),
                echoed_server_id: None,
                offset_matches: false,
            }),
            Ok(wechat_protocol::wire::VoiceChunk {
                offset: opus.len() as u32,
                next_offset: opus.len() as u32,
                total_length: advertised,
                complete: false,
                bytes: vec![],
                echoed_server_id: None,
                offset_matches: false,
            }),
            Ok(wechat_protocol::wire::VoiceChunk {
                offset: opus.len() as u32,
                next_offset: opus.len() as u32,
                total_length: advertised,
                complete: false,
                bytes: vec![],
                echoed_server_id: None,
                offset_matches: false,
            }),
            Ok(wechat_protocol::wire::VoiceChunk {
                offset: opus.len() as u32,
                next_offset: opus.len() as u32,
                total_length: advertised,
                complete: false,
                bytes: vec![],
                echoed_server_id: None,
                offset_matches: false,
            }),
        ]));
        let (_dir, core, _msg_id, assets) = voice_fixture(upstream.clone(), &xml, "9001001");
        let normal = assets
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        let encoded = assets
            .iter()
            .find(|a| a.variant == MediaVariant::Encoded)
            .unwrap();

        run_download(&core, &normal.id, "job-opus-wav").unwrap();
        let wav_asset = core.media_asset("one", &normal.id).unwrap();
        assert_eq!(wav_asset.state, "available");
        assert_eq!(wav_asset.content_type.as_deref(), Some("audio/wav"));
        assert_eq!(wav_asset.official_writeback, "complete");
        let wav = core.media_content("one", &normal.id).unwrap();
        assert_eq!(&wav.bytes[..4], b"RIFF");
        assert_eq!(upstream.cgi_calls.load(Ordering::SeqCst), 4);

        // Encoded variant now reuses the profile's stored Opus bytes without another CGI call.
        run_download(&core, &encoded.id, "job-opus-raw").unwrap();
        let raw_asset = core.media_asset("one", &encoded.id).unwrap();
        assert_eq!(raw_asset.state, "available");
        assert_eq!(raw_asset.content_type.as_deref(), Some("audio/opus"));
        assert_eq!(core.media_content("one", &encoded.id).unwrap().bytes, opus);
        assert_eq!(upstream.cgi_calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn voice_download_tries_cdn_first_and_falls_back_to_cgi_and_preserves_raw_encoded() {
        let silk = sample_silk_with_dtx_and_eof();
        let xml = format!(
            "<msg><voicemsg voiceformat='4' length='{}' voicelength='240' endflag='1' voiceurl='cdn-voice-file-id' aeskey='0102030405060708090a0b0c0d0e0f10'/></msg>",
            silk.len()
        );
        // CDN succeeds on first attempt: CGI 128 is never called.
        let upstream_cdn = Arc::new(VoiceTestUpstream::new(vec![]).with_cdn(Ok(silk.clone())));
        let (_dir1, core1, _m1, assets1) = voice_fixture(upstream_cdn.clone(), &xml, "9001002");
        let normal1 = assets1
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        run_download(&core1, &normal1.id, "job-cdn-ok").unwrap();
        assert_eq!(upstream_cdn.cdn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(upstream_cdn.cgi_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            &core1.media_content("one", &normal1.id).unwrap().bytes[..4],
            b"RIFF"
        );

        // CDN fails -> falls back unconditionally to CGI 128; encoded Speex stores raw without SILK decode.
        let speex_raw = vec![0xaa; 120];
        let xml_speex = format!(
            "<msg><voicemsg voiceformat='1' length='{}' voicelength='200' endflag='1' voiceurl='cdn-speex' aeskey='0102030405060708090a0b0c0d0e0f10'/></msg>",
            speex_raw.len()
        );
        let upstream_fb = Arc::new(
            VoiceTestUpstream::new(vec![Ok(wechat_protocol::wire::VoiceChunk {
                offset: 0,
                next_offset: speex_raw.len() as u32,
                total_length: speex_raw.len() as u32,
                complete: true,
                bytes: speex_raw.clone(),
                echoed_server_id: Some("9001003".into()),
                offset_matches: true,
            })])
            .with_cdn(Err(anyhow::anyhow!("cdn_http_502"))),
        );
        let (_dir2, core2, _m2, assets2) =
            voice_fixture(upstream_fb.clone(), &xml_speex, "9001003");
        let encoded2 = assets2
            .iter()
            .find(|a| a.variant == MediaVariant::Encoded)
            .unwrap();
        run_download(&core2, &encoded2.id, "job-speex-encoded").unwrap();
        assert_eq!(upstream_fb.cdn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(upstream_fb.cgi_calls.load(Ordering::SeqCst), 1);
        let asset2 = core2.media_asset("one", &encoded2.id).unwrap();
        assert_eq!(asset2.state, "available");
        assert_eq!(asset2.content_type.as_deref(), Some("audio/x-speex"));
        assert_eq!(
            core2.media_content("one", &encoded2.id).unwrap().bytes,
            speex_raw
        );

        // Requesting WAV on Speex records structured voiceDownloadFailure diagnostics.
        let normal2 = assets2
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        assert!(run_download(&core2, &normal2.id, "job-speex-wav").is_err());
        let failed = core2.store.media_job("one", "job-speex-wav").unwrap();
        assert_eq!(failed.state, "unsupported");
        let detail = &failed.data["result"]["voiceDownloadFailure"];
        assert_eq!(detail["stage"], "decode");
        assert_eq!(detail["detectedFormat"], "speex");
        assert_eq!(detail["voiceFormat"], 1);
        assert_eq!(detail["receivedBytes"], 120);
    }

    #[tokio::test]
    async fn failed_voice_download_can_be_restarted_by_explicit_transfer_request() {
        let silk = sample_silk_with_dtx_and_eof();
        let xml = format!(
            "<msg><voicemsg voiceformat='4' length='{}' voicelength='240' endflag='1'/></msg>",
            silk.len()
        );
        let upstream = Arc::new(VoiceTestUpstream::new(vec![]));
        let (_dir, core, msg_id, assets) = voice_fixture(upstream, &xml, "9001004");
        let normal = assets
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        let key = format!("download:{}", normal.id);
        let (mut row, _) = core
            .store
            .create_media_job(
                "one",
                "prior-failed-voice",
                &key,
                &digest(normal.id.as_bytes()),
                "download",
                &json!({"assetId": normal.id}),
                b"",
            )
            .unwrap();
        row.state = "failed".into();
        row.error_code = Some("media_processing_failed".into());
        row.retryable = false;
        core.store.save_media_job("one", &row).unwrap();

        let fresh = core
            .start_media_transfer(
                "one",
                MediaTransferRequest {
                    message_id: msg_id,
                    asset_id: Some(normal.id.clone()),
                    quality: MediaVariant::Normal,
                },
            )
            .unwrap();
        assert_ne!(fresh.id, "prior-failed-voice");
        assert_eq!(fresh.state, "queued");
        let refreshed = core.media_asset("one", &normal.id).unwrap();
        assert_eq!(refreshed.state, "pending");
        assert_eq!(refreshed.reason, None);
        assert_eq!(refreshed.job_id.as_deref(), Some(fresh.id.as_str()));
    }

    #[test]
    fn voice_validation_rejects_truncated_inline_and_incomplete_encoded_and_maps_cancel() {
        let opus = sample_opus_raw_v1();
        assert!(is_usable_inline_voice(
            &opus,
            (opus.len() + 10) as u32,
            Some(7)
        ));
        // Truncated `#!OPUS_RAW_V1` header/packet is rejected for inline bypass and encoded writeback.
        let truncated_opus = &opus[..opus.len() - 3];
        assert!(!is_usable_inline_voice(
            truncated_opus,
            (opus.len() + 10) as u32,
            Some(7)
        ));
        assert!(validate_encoded_voice(truncated_opus, Some(7), true, false).is_err());

        // Valid AMR-NB (FT=0 -> 12 payload bytes) accepted; truncated AMR rejected.
        let mut amr = Vec::from(&b"#!AMR\n"[..]);
        amr.push(0x04); // P=0, FT=0, Q=1
        amr.extend([0u8; 12]);
        assert!(is_usable_inline_voice(&amr, 999, Some(0)));
        assert!(is_usable_inline_voice(&amr[6..], 999, Some(0)));
        assert!(!is_usable_inline_voice(&amr[..12], 999, Some(0)));

        // Cancelled and streaming XML yield empty downloadable assets (not 500 errors).
        let cancelled_xml =
            "<msg><voicemsg voiceformat='4' length='100' cancelflag='1' endflag='1'/></msg>";
        let (_dir_c, _core_c, _msg_c, assets_c) = voice_fixture(
            Arc::new(VoiceTestUpstream::new(vec![])),
            cancelled_xml,
            "9001005",
        );
        assert!(assets_c.is_empty());

        // CGI 128 CancelFlag=1 maps to terminal non-retryable `voice_cancelled`.
        let xml = "<msg><voicemsg voiceformat='4' length='200' endflag='1'/></msg>";
        let upstream_cancel = Arc::new(VoiceTestUpstream::new(vec![Err(anyhow::anyhow!(
            "voice_cancelled"
        ))]));
        let (_dir_k, core_k, _msg_k, assets_k) = voice_fixture(upstream_cancel, xml, "9001006");
        let normal_k = assets_k
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        assert!(run_download(&core_k, &normal_k.id, "job-voice-cancel").is_err());
        let job_k = core_k.store.media_job("one", "job-voice-cancel").unwrap();
        assert_eq!(job_k.state, "cancelled");
        assert_eq!(job_k.error_code.as_deref(), Some("voice_cancelled"));
        assert!(!job_k.retryable);
        assert_eq!(
            job_k.data["result"]["voiceDownloadFailure"]["reason"],
            "voice_cancelled"
        );

        // CDN bootstrap auth rejection (-13) fails immediately without falling back to CGI 128.
        let xml_cdn = "<msg><voicemsg voiceformat='4' length='200' endflag='1' voiceurl='cdn-auth' aeskey='0102030405060708090a0b0c0d0e0f10'/></msg>";
        let upstream_auth = Arc::new(VoiceTestUpstream::new(vec![]).with_cdn(Err(
            anyhow::Error::new(wechat_protocol::ServiceRejected::new(
                wechat_protocol::Service::CdnBootstrap,
                -13,
            )),
        )));
        let (_dir_a, core_a, _msg_a, assets_a) =
            voice_fixture(upstream_auth.clone(), xml_cdn, "9001007");
        let normal_a = assets_a
            .iter()
            .find(|a| a.variant == MediaVariant::Normal)
            .unwrap();
        assert!(run_download(&core_a, &normal_a.id, "job-cdn-auth").is_err());
        assert_eq!(upstream_auth.cdn_calls.load(Ordering::SeqCst), 1);
        assert_eq!(upstream_auth.cgi_calls.load(Ordering::SeqCst), 0);
        let job_a = core_a.store.media_job("one", "job-cdn-auth").unwrap();
        assert_eq!(job_a.error_code.as_deref(), Some("auth_required"));
    }
}
