//! Final media-message metadata. CDN upload and message submission are separate
//! steps; persist source bytes, generated keys and intent before either step.
use crate::{
    media::{MediaQuality, UploadedImage, UploadedMedia, UploadedVideo},
    protobuf as p, silk,
    wire::base_request,
    AccountState,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
fn clean(s: &str, max: usize) -> Result<&[u8]> {
    ensure!(
        !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control()),
        "Invalid media text"
    );
    Ok(s.as_bytes())
}
fn hex32(value: &str) -> Result<String> {
    ensure!(
        value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid media MD5"
    );
    Ok(value.to_ascii_lowercase())
}
fn descriptor(v: &UploadedMedia, kind: MediaQuality) -> Result<()> {
    ensure!(
        v.descriptor.quality == kind
            && v.descriptor.aes_key.len() == 16
            && v.plaintext_bytes > 0
            && v.plaintext_bytes <= 25 * 1024 * 1024,
        "Invalid uploaded media descriptor"
    );
    clean(&v.descriptor.file_id, 8192)?;
    hex32(&v.md5)?;
    ensure!(
        v.descriptor.expected_bytes == Some(v.plaintext_bytes as u64)
            && v.descriptor
                .expected_md5
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(&v.md5)),
        "Uploaded media metadata differs"
    );
    Ok(())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn tag(name: &str, value: impl ToString) -> String {
    format!("<{name}>{}</{name}>", escape(&value.to_string()))
}
/// Generic URL shares use app subtype 5. Metadata is supplied by the caller;
/// composing or sending a card never fetches the target page or thumbnail.
pub fn share_xml(
    sender: &str,
    title: &str,
    url: &str,
    description: Option<&str>,
    thumbnail_url: Option<&str>,
) -> Result<String> {
    clean(sender, 256)?;
    ensure!(
        !title.trim().is_empty()
            && title.len() <= 16384
            && !title
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t')),
        "invalid_share_request"
    );
    crate::media::validate_shared_link_url(url)?;
    let description = description.unwrap_or("");
    ensure!(
        description.len() <= 16384
            && !description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t')),
        "invalid_share_request"
    );
    if let Some(url) = thumbnail_url {
        crate::media::validate_link_thumbnail_url(url)?;
    }
    let xml = format!(
        "<appmsg appid=\"\" sdkver=\"0\">{}{}{}{}{}{}{}{}{}{}</appmsg>{}",
        tag("title", title),
        tag("des", description),
        tag("action", "view"),
        tag("type", 5),
        tag("showtype", 0),
        tag("url", url),
        tag("lowurl", ""),
        tag("thumburl", thumbnail_url.unwrap_or("")),
        tag("sourceusername", ""),
        tag("sourcedisplayname", ""),
        tag("fromusername", sender)
    );
    ensure!(xml.len() <= 256 * 1024, "invalid_share_request");
    Ok(xml)
}
pub fn share_send_request(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    created_at: u32,
    content: &str,
) -> Result<Vec<u8>> {
    clean(destination, 256)?;
    clean(client_id, 1024)?;
    ensure!(
        created_at > 0 && !content.is_empty() && content.len() <= 256 * 1024,
        "invalid_share_request"
    );
    let app = p::message(&[
        p::bytes(1, state.own_username.as_bytes()),
        p::bytes(2, &[]),
        p::uint(3, 0),
        p::bytes(4, destination.as_bytes()),
        p::uint(5, 5),
        p::bytes(6, content.as_bytes()),
        p::uint(7, created_at as u64),
        p::bytes(8, client_id.as_bytes()),
        p::int32(10, 1),
        p::bytes(13, &[]),
        p::bytes(14, &[]),
        p::bytes(15, &[]),
    ])?;
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &app),
        p::uint(9, 0),
        p::uint(10, 0),
        p::uint(11, 2),
    ])
}
pub fn file_xml(sender: &str, upload: &UploadedMedia) -> Result<String> {
    clean(sender, 1024)?;
    descriptor(upload, MediaQuality::File)?;
    let name = &upload.file_name;
    clean(name, 255)?;
    ensure!(
        name != "." && name != ".." && !name.contains(['/', '\\']),
        "Expected a single filename component"
    );
    let extension = name
        .rsplit_once('.')
        .filter(|(base, _)| !base.is_empty())
        .map(|(_, extension)| extension)
        .unwrap_or("");
    let mut app = tag("title", name);
    for name in ["des", "action"] {
        app.push_str(&tag(name, ""));
    }
    app.push_str(&tag("type", 6));
    app.push_str(&tag("showtype", 0));
    app.push_str(&tag("soundtype", 0));
    for name in ["mediatagname", "messageext", "messageaction", "content"] {
        app.push_str(&tag(name, ""));
    }
    app.push_str(&tag("contentattr", 0));
    for name in ["url", "lowurl", "dataurl", "lowdataurl"] {
        app.push_str(&tag(name, ""));
    }
    app.push_str("<appattach>");
    app.push_str(&tag("totallen", upload.plaintext_bytes));
    app.push_str(&tag("attachid", ""));
    app.push_str(&tag("emoticonmd5", ""));
    app.push_str(&tag("fileext", extension));
    app.push_str(&tag("cdnattachurl", &upload.descriptor.file_id));
    app.push_str(&tag("aeskey", hex::encode(&upload.descriptor.aes_key)));
    app.push_str(&tag("encryver", 1));
    app.push_str("</appattach>");
    for name in ["extinfo", "sourceusername", "sourcedisplayname", "thumburl"] {
        app.push_str(&tag(name, ""));
    }
    app.push_str(&tag("md5", hex32(&upload.md5)?));
    app.push_str(&tag("statextstr", ""));
    Ok(format!(
        "<appmsg appid=\"\" sdkver=\"0\">{app}</appmsg>{}",
        tag("fromusername", sender)
    ))
}
/// Reconstruct local metadata from accepted uploaded tuples. These are local
/// cache records, not a claim that a server returned this exact XML spelling.
fn local_element(name: &str, attributes: Vec<(&str, String)>) -> String {
    let attributes = attributes
        .into_iter()
        .map(|(key, value)| format!(" {key}=\"{}\"", escape(&value)))
        .collect::<String>();
    format!("<msg><{name}{attributes}/></msg>")
}
pub fn local_image_xml(image: &UploadedImage) -> Result<String> {
    descriptor(&image.original, MediaQuality::Original)?;
    descriptor(&image.standard, MediaQuality::Standard)?;
    descriptor(&image.thumbnail, MediaQuality::Thumbnail)?;
    ensure!(
        image.original.descriptor.aes_key == image.standard.descriptor.aes_key,
        "Image variants require one main key"
    );
    Ok(local_element(
        "img",
        vec![
            ("aeskey", hex::encode(&image.original.descriptor.aes_key)),
            ("encryver", "1".into()),
            ("cdnbigimgurl", image.original.descriptor.file_id.clone()),
            ("cdnmidimgurl", image.standard.descriptor.file_id.clone()),
            ("cdnthumburl", image.thumbnail.descriptor.file_id.clone()),
            (
                "cdnthumbaeskey",
                hex::encode(&image.thumbnail.descriptor.aes_key),
            ),
            ("md5", hex32(&image.original.md5)?),
            ("hdlength", image.original.plaintext_bytes.to_string()),
            ("length", image.standard.plaintext_bytes.to_string()),
            (
                "cdnthumblength",
                image.thumbnail.plaintext_bytes.to_string(),
            ),
            (
                "cdnthumbwidth",
                image.thumbnail.width.unwrap_or(0).to_string(),
            ),
            (
                "cdnthumbheight",
                image.thumbnail.height.unwrap_or(0).to_string(),
            ),
        ],
    ))
}
pub fn local_video_xml(video: &UploadedVideo, seconds: u32) -> Result<String> {
    descriptor(&video.video, MediaQuality::Video)?;
    descriptor(&video.thumbnail, MediaQuality::Thumbnail)?;
    ensure!(seconds > 0, "Invalid video duration");
    Ok(local_element(
        "videomsg",
        vec![
            ("aeskey", hex::encode(&video.video.descriptor.aes_key)),
            ("cdnvideourl", video.video.descriptor.file_id.clone()),
            ("length", video.video.plaintext_bytes.to_string()),
            ("md5", hex32(&video.video.md5)?),
            ("playlength", seconds.to_string()),
            ("cdnthumburl", video.thumbnail.descriptor.file_id.clone()),
            (
                "cdnthumbaeskey",
                hex::encode(&video.thumbnail.descriptor.aes_key),
            ),
            (
                "cdnthumblength",
                video.thumbnail.plaintext_bytes.to_string(),
            ),
            (
                "cdnthumbwidth",
                video.thumbnail.width.unwrap_or(0).to_string(),
            ),
            (
                "cdnthumbheight",
                video.thumbnail.height.unwrap_or(0).to_string(),
            ),
        ],
    ))
}
pub fn local_voice_xml(encoded_bytes: usize, duration_ms: u32) -> Result<String> {
    ensure!(
        encoded_bytes > 0
            && encoded_bytes <= silk::MAX_NOTE_BYTES
            && (1..=silk::MAX_NOTE_MS).contains(&duration_ms),
        "Invalid voice note metadata"
    );
    Ok(local_element(
        "voicemsg",
        vec![
            ("voiceformat", "4".into()),
            ("length", encoded_bytes.to_string()),
            ("voicelength", duration_ms.to_string()),
            ("endflag", "1".into()),
        ],
    ))
}
pub fn local_file_xml(sender: &str, upload: &UploadedMedia) -> Result<String> {
    Ok(format!("<msg>{}</msg>", file_xml(sender, upload)?))
}
pub fn file_send_request(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    created_at: u32,
    upload: &UploadedMedia,
) -> Result<Vec<u8>> {
    clean(destination, 512)?;
    clean(client_id, 1024)?;
    ensure!(created_at > 0, "Invalid media timestamp");
    let content = file_xml(&state.own_username, upload)?;
    let app = p::message(&[
        p::bytes(1, state.own_username.as_bytes()),
        p::bytes(2, &[]),
        p::uint(3, 0),
        p::bytes(4, destination.as_bytes()),
        p::uint(5, 6),
        p::bytes(6, content.as_bytes()),
        p::uint(7, created_at as u64),
        p::bytes(8, client_id.as_bytes()),
        p::int32(10, 1),
        p::bytes(13, &[]),
        p::bytes(14, &[]),
        p::bytes(15, &[]),
    ])?;
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &app),
        p::bytes(5, hex32(&upload.md5)?.as_bytes()),
        p::uint(9, 0),
        p::uint(10, 0),
        p::uint(11, 2),
    ])
}
pub fn image_send_request(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    image: &UploadedImage,
) -> Result<Vec<u8>> {
    clean(destination, 512)?;
    clean(client_id, 4096)?;
    descriptor(&image.original, MediaQuality::Original)?;
    descriptor(&image.standard, MediaQuality::Standard)?;
    descriptor(&image.thumbnail, MediaQuality::Thumbnail)?;
    ensure!(
        image.original.descriptor.aes_key == image.standard.descriptor.aes_key,
        "Image main variants require one shared key"
    );
    let thumbnail = &image.thumbnail;
    let width = thumbnail
        .width
        .ok_or_else(|| anyhow::anyhow!("Missing thumbnail width"))?;
    let height = thumbnail
        .height
        .ok_or_else(|| anyhow::anyhow!("Missing thumbnail height"))?;
    ensure!(
        width > 0 && width <= 8192 && height > 0 && height <= 8192,
        "Invalid thumbnail dimensions"
    );
    let sk = |s: &[u8]| p::bytes(1, s);
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, &sk(client_id.as_bytes())),
        p::bytes(3, &sk(state.own_username.as_bytes())),
        p::bytes(4, &sk(destination.as_bytes())),
        p::uint(5, thumbnail.plaintext_bytes as u64),
        p::uint(6, 0),
        p::uint(7, thumbnail.plaintext_bytes as u64),
        p::bytes(8, &p::buffer(&[])),
        p::uint(9, 3),
        p::uint(11, 1),
        p::int32(12, 2),
        p::int32(13, 0),
        p::bytes(15, image.original.descriptor.file_id.as_bytes()),
        p::bytes(16, image.standard.descriptor.file_id.as_bytes()),
        p::bytes(
            17,
            hex::encode(&image.original.descriptor.aes_key).as_bytes(),
        ),
        p::int32(18, 1),
        p::int32(19, image.original.plaintext_bytes as i32),
        p::int32(20, image.standard.plaintext_bytes as i32),
        p::bytes(21, thumbnail.descriptor.file_id.as_bytes()),
        p::int32(22, thumbnail.plaintext_bytes as i32),
        p::int32(23, height as i32),
        p::int32(24, width as i32),
        p::bytes(25, hex::encode(&thumbnail.descriptor.aes_key).as_bytes()),
        p::bytes(27, hex32(&image.original.md5)?.as_bytes()),
        p::uint(28, 0),
        p::uint(30, 0),
        p::uint(36, 0),
        p::uint(41, 0),
    ])
}
pub fn video_send_request(
    state: &AccountState,
    destination: &str,
    client_id: &str,
    video: &UploadedVideo,
    play_seconds: u32,
) -> Result<Vec<u8>> {
    clean(destination, 512)?;
    clean(client_id, 1024)?;
    ensure!(play_seconds > 0, "Invalid video play duration");
    descriptor(&video.video, MediaQuality::Video)?;
    descriptor(&video.thumbnail, MediaQuality::Thumbnail)?;
    let v = &video.video;
    let t = &video.thumbnail;
    let width = t
        .width
        .ok_or_else(|| anyhow::anyhow!("Missing video thumbnail width"))?;
    let height = t
        .height
        .ok_or_else(|| anyhow::anyhow!("Missing video thumbnail height"))?;
    ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192,
        "Invalid video thumbnail dimensions"
    );
    p::message(&[
        p::bytes(1, &base_request(&state.device, state.uin, 0)?),
        p::bytes(2, client_id.as_bytes()),
        p::bytes(3, state.own_username.as_bytes()),
        p::bytes(4, destination.as_bytes()),
        p::uint(5, t.plaintext_bytes as u64),
        p::uint(6, 0),
        p::bytes(7, &p::buffer(&[])),
        p::uint(8, v.plaintext_bytes as u64),
        p::uint(9, 0),
        p::bytes(10, &p::buffer(&[])),
        p::uint(11, play_seconds as u64),
        p::uint(12, 1),
        p::uint(13, 0),
        p::uint(14, 0),
        p::bytes(15, &[]),
        p::bytes(16, v.descriptor.file_id.as_bytes()),
        p::bytes(17, hex::encode(&v.descriptor.aes_key).as_bytes()),
        p::int32(18, 1),
        p::bytes(19, t.descriptor.file_id.as_bytes()),
        p::int32(20, t.plaintext_bytes as i32),
        p::int32(21, height as i32),
        p::int32(22, width as i32),
        p::bytes(23, hex::encode(&t.descriptor.aes_key).as_bytes()),
        p::bytes(26, hex32(&v.md5)?.as_bytes()),
        p::bytes(37, &[]),
        p::uint(38, 0),
    ])
}
/// Encoded SILK bytes carried by one `uploadvoice` request. A short-link
/// request travels in one MMTLS record of at most 65,519 plaintext bytes,
/// including the route, packet header and envelope, so a 60-second note (about
/// 126-131 KB from the bundled 16 kbit/s profile) cannot use one request. 32 KiB
/// matches the `downloadvoice` range size and leaves room for that overhead.
pub const VOICE_UPLOAD_CHUNK_BYTES: usize = 32 * 1024;

/// One voice note submitted through `uploadvoice`. A note of at most
/// [`VOICE_UPLOAD_CHUNK_BYTES`] uses a single request with the end flag set.
/// A larger note is sent as consecutive byte ranges (offset/length) under the
/// same client message ID; only the last range sets the end flag, and later
/// ranges carry a server message ID returned for an earlier range. Each range
/// is submitted once: an error after any submission is an uncertain outcome.
pub struct VoiceUpload<'a> {
    destination: &'a str,
    client_id: &'a str,
    created_at: u32,
    duration_ms: u32,
    encoded: &'a [u8],
    offset: usize,
    server_msg_id: Option<u32>,
    new_msg_id: Option<u64>,
}
impl<'a> VoiceUpload<'a> {
    pub fn new(
        destination: &'a str,
        client_id: &'a str,
        created_at: u32,
        duration_ms: u32,
        encoded: &'a [u8],
    ) -> Result<Self> {
        clean(destination, 512)?;
        let client = clean(client_id, 128)?;
        ensure!(
            client.len() >= 16
                && client
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-'),
            "Invalid voice client ID"
        );
        ensure!(
            created_at > 0
                && (1..=silk::MAX_NOTE_MS).contains(&duration_ms)
                && encoded.len() >= 12
                && encoded.len() <= silk::MAX_NOTE_BYTES
                && encoded[0] == 2
                && encoded[1..10] == *b"#!SILK_V3",
            "Unsupported bounded SILK voice profile"
        );
        Ok(Self {
            destination,
            client_id,
            created_at,
            duration_ms,
            encoded,
            offset: 0,
            server_msg_id: None,
            new_msg_id: None,
        })
    }
    fn range(&self) -> std::ops::Range<usize> {
        self.offset..(self.offset + VOICE_UPLOAD_CHUNK_BYTES).min(self.encoded.len())
    }
    /// Number of requests needed for the whole note.
    pub fn request_count(&self) -> usize {
        self.encoded.len().div_ceil(VOICE_UPLOAD_CHUNK_BYTES)
    }
    /// Whether the next request carries the last range and the end flag.
    pub fn is_final(&self) -> bool {
        self.range().end == self.encoded.len()
    }
    /// Build the request for the next unsent range.
    pub fn request(&self, state: &AccountState) -> Result<Vec<u8>> {
        let range = self.range();
        let chunk = &self.encoded[range.clone()];
        let client = self.client_id.as_bytes();
        let mut parts = vec![
            p::bytes(1, state.own_username.as_bytes()),
            p::bytes(2, self.destination.as_bytes()),
            p::uint(3, range.start as u64),
            p::uint(4, chunk.len() as u64),
            p::bytes(5, client),
        ];
        if let Some(id) = self.server_msg_id {
            parts.push(p::uint(6, id.into()));
        }
        parts.extend([
            p::uint(7, self.duration_ms.into()),
            p::bytes(8, &p::buffer(chunk)),
            p::uint(9, self.is_final().into()),
            p::bytes(10, &base_request(&state.device, state.uin, 0)?),
            p::uint(11, 4),
            p::uint(13, 0),
            p::uint(15, 0),
            p::uint(16, 0),
            p::uint(17, self.created_at.into()),
            p::bytes(18, &p::buffer(client)),
            p::bytes(21, &[]),
        ]);
        p::message(&parts)
    }
    /// Apply the reply for the request built last. Returns the note's result
    /// after the final range, or `None` when the next range must be sent. A
    /// rejection after an earlier range was accepted needs review, because the
    /// server already holds part of the note under this client message ID.
    pub fn accept_reply(&mut self, input: &[u8]) -> Result<Option<MediaSendResult>> {
        let continued = self.offset > 0;
        if self.is_final() {
            let mut result = decode_voice_send(input, self.client_id)?;
            let changed = self.new_msg_id.is_some_and(|known| {
                result.server_message_id.as_deref() != Some(known.to_string().as_str())
            });
            if (continued && result.status == SendStatus::Rejected) || changed {
                result.status = SendStatus::NeedsReview;
            }
            return Ok(Some(result));
        }
        let (f, code) = voice_reply(input)?;
        if code != 0 {
            let mut result = rejected(code);
            if continued {
                result.status = SendStatus::NeedsReview;
            }
            return Ok(Some(result));
        }
        let scalar = |n| -> Result<Option<u64>> {
            Ok(match p::optional(&f, n)? {
                Some(p::Value::Varint(v)) => Some(*v),
                _ => None,
            })
        };
        for n in [3, 4, 5, 7, 8, 9] {
            if let Some(v) = scalar(n)? {
                u32::try_from(v)?;
            }
        }
        ensure!(
            scalar(9)?.unwrap_or(0) == 0,
            "Voice upload ended before its last range"
        );
        if let Some(id) = scalar(7)?.filter(|id| *id != 0) {
            let id = u32::try_from(id)?;
            ensure!(
                self.server_msg_id.is_none_or(|known| known == id),
                "Voice upload message ID changed"
            );
            self.server_msg_id = Some(id);
        }
        if let Some(id) = scalar(12)?.filter(|id| *id != 0) {
            ensure!(
                self.new_msg_id.is_none_or(|known| known == id),
                "Voice upload message ID changed"
            );
            self.new_msg_id = Some(id);
        }
        self.offset = self.range().end;
        Ok(None)
    }
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    ServerAccepted,
    Rejected,
    NeedsReview,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MediaSendResult {
    pub status: SendStatus,
    pub service_code: i32,
    pub server_message_id: Option<String>,
    pub client_id_matches: Option<bool>,
}
fn rejected(code: i32) -> MediaSendResult {
    MediaSendResult {
        status: SendStatus::Rejected,
        service_code: code,
        server_message_id: None,
        client_id_matches: None,
    }
}
pub fn decode_file_send(input: &[u8], client_id: &str) -> Result<MediaSendResult> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    if code != 0 {
        return Ok(rejected(code));
    }
    for n in [5, 7] {
        u32::try_from(p::scalar(&f, n)?)?;
    }
    let id = p::scalar(&f, 9)?;
    ensure!(id != 0, "Accepted file response has no server ID");
    let mut matches = false;
    for n in [2, 3, 4, 6, 10, 11, 13] {
        if let Some(v) = p::optional_data(&f, n)? {
            std::str::from_utf8(v)?;
            matches |= v == client_id.as_bytes();
        }
    }
    Ok(MediaSendResult {
        status: SendStatus::ServerAccepted,
        service_code: code,
        server_message_id: Some(id.to_string()),
        client_id_matches: matches.then_some(true),
    })
}
pub fn decode_image_send(
    input: &[u8],
    client_id: &str,
    sender: &str,
    destination: &str,
) -> Result<MediaSendResult> {
    let f = p::decode(input)?;
    for n in 1..=13 {
        p::optional(&f, n)?;
    }
    let code = p::base_code(&f)?;
    if code != 0 {
        return Ok(rejected(code));
    }
    for n in [2, 6, 7, 8, 9] {
        u32::try_from(p::scalar(&f, n)?)?;
    }
    let nested = |n| -> Result<Vec<u8>> {
        let item = p::decode(p::data(&f, n)?)?;
        Ok(p::data(&item, 1)?.to_vec())
    };
    let correlated = nested(3)? == client_id.as_bytes()
        && nested(4)? == sender.as_bytes()
        && nested(5)? == destination.as_bytes();
    let id = p::scalar(&f, 10)?;
    ensure!(id != 0, "Accepted image response has no server ID");
    for n in [11, 12, 13] {
        p::optional_data(&f, n)?;
    }
    Ok(MediaSendResult {
        status: if correlated {
            SendStatus::ServerAccepted
        } else {
            SendStatus::NeedsReview
        },
        service_code: code,
        server_message_id: Some(id.to_string()),
        client_id_matches: Some(correlated),
    })
}
pub fn decode_video_send(input: &[u8], client_id: &str) -> Result<MediaSendResult> {
    let f = p::decode(input)?;
    let code = p::base_code(&f)?;
    if code != 0 {
        return Ok(rejected(code));
    }
    for n in [3, 4, 5] {
        u32::try_from(p::scalar(&f, n)?)?;
    }
    let id = p::scalar(&f, 6)?;
    ensure!(id != 0, "Accepted video response has no server ID");
    let mut matches = false;
    for n in [2, 7, 8] {
        if let Some(v) = p::optional_data(&f, n)? {
            std::str::from_utf8(v)?;
            matches |= v == client_id.as_bytes();
        }
    }
    Ok(MediaSendResult {
        status: SendStatus::ServerAccepted,
        service_code: code,
        server_message_id: Some(id.to_string()),
        client_id_matches: matches.then_some(true),
    })
}
fn voice_reply(input: &[u8]) -> Result<(Vec<p::Field>, i32)> {
    let f = p::decode(input)?;
    for n in 1..=13 {
        let v = p::optional(&f, n)?;
        if let Some(v) = v {
            ensure!(
                matches!(
                    (n, v),
                    (1 | 2 | 6 | 10, p::Value::Bytes(_))
                        | (3 | 4 | 5 | 7 | 8 | 9 | 11 | 12 | 13, p::Value::Varint(_))
                ),
                "Invalid voice response field"
            );
        }
    }
    let base = p::decode(p::data(&f, 10)?)?;
    let code = p::scalar(&base, 1)? as u32 as i32;
    Ok((f, code))
}
/// Decode the reply to the request carrying a note's end flag.
pub fn decode_voice_send(input: &[u8], client_id: &str) -> Result<MediaSendResult> {
    let (f, code) = voice_reply(input)?;
    if code != 0 {
        return Ok(rejected(code));
    }
    for n in [3, 4, 5, 7, 8, 9] {
        u32::try_from(p::scalar(&f, n)?)?;
    }
    let id = p::scalar(&f, 12)?;
    ensure!(id != 0, "Accepted voice response has no server ID");
    let mut matches = 0;
    for n in [1, 2, 6] {
        if let Some(v) = p::optional_data(&f, n)? {
            std::str::from_utf8(v)?;
            if v == client_id.as_bytes() {
                matches += 1;
            }
        }
    }
    Ok(MediaSendResult {
        status: if matches == 1 {
            SendStatus::ServerAccepted
        } else {
            SendStatus::NeedsReview
        },
        service_code: code,
        server_message_id: Some(id.to_string()),
        client_id_matches: Some(matches == 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn synthetic_state() -> AccountState {
        AccountState {
            schema: 1,
            device: crate::DeviceProfile {
                device_id: vec![1; 16],
                device_type: "UnifiedPCLinux synthetic x86_64".into(),
                client_version: -227271398,
            },
            uin: 123,
            own_username: "synthetic-user".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![],
            response_key: vec![],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    #[test]
    fn metadata_matches_independent_synthetic_wire_vectors() {
        use crate::media::MediaDescriptor;
        let state = synthetic_state();
        let descriptor = |quality, file_name: &str| UploadedMedia {
            descriptor: MediaDescriptor {
                file_id: "synthetic-file-id".into(),
                aes_key: vec![1; 16],
                quality,
                expected_bytes: Some(30),
                expected_md5: Some("0".repeat(32)),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: 30,
            md5: "0".repeat(32),
            sha256: "0".repeat(64),
            file_name: file_name.into(),
            width: Some(16),
            height: Some(12),
        };
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/synthetic-media-wire.json")).unwrap();
        let file = file_send_request(
            &state,
            "filehelper",
            "synthetic_client_id_0001",
            1234567,
            &descriptor(MediaQuality::File, "example&.txt"),
        )
        .unwrap();
        assert_eq!(hex::encode(file), expected["fileHex"]);
        let share_xml = share_xml(
            "synthetic-user",
            "A <title> & preview",
            "https://example.invalid/a?x=1&y=2",
            Some("Description <as text>"),
            Some("https://example.invalid/preview.jpg"),
        )
        .unwrap();
        assert!(share_xml.starts_with("<appmsg "));
        assert!(share_xml.contains("<title>A &lt;title&gt; &amp; preview</title>"));
        assert!(share_xml.contains("<type>5</type>"));
        assert!(share_xml.contains("<url>https://example.invalid/a?x=1&amp;y=2</url>"));
        assert!(share_xml.ends_with("<fromusername>synthetic-user</fromusername>"));
        assert!(!share_xml.contains("<msg>"));
        let share =
            share_send_request(&state, "room@chatroom", "share-client", 1234567, &share_xml)
                .unwrap();
        let outer = p::decode(&share).unwrap();
        let app = p::decode(p::data(&outer, 2).unwrap()).unwrap();
        assert_eq!(p::scalar(&app, 5).unwrap(), 5);
        assert_eq!(p::data(&app, 4).unwrap(), b"room@chatroom");
        assert_eq!(p::data(&app, 6).unwrap(), share_xml.as_bytes());
        assert_eq!(p::scalar(&app, 7).unwrap(), 1234567);
        assert_eq!(p::data(&app, 8).unwrap(), b"share-client");
        assert!(p::optional_data(&outer, 5).unwrap().is_none()); // No file MD5 for a URL share.
        let image = UploadedImage {
            original: descriptor(MediaQuality::Original, "image.jpg"),
            standard: descriptor(MediaQuality::Standard, "image.jpg"),
            thumbnail: descriptor(MediaQuality::Thumbnail, "image.jpg"),
        };
        assert_eq!(
            hex::encode(
                image_send_request(&state, "filehelper", "synthetic_client_id_0001", &image)
                    .unwrap()
            ),
            expected["imageHex"]
        );
        let video = UploadedVideo {
            video: descriptor(MediaQuality::Video, "video.mp4"),
            thumbnail: descriptor(MediaQuality::Thumbnail, "thumb.jpg"),
        };
        assert_eq!(
            hex::encode(
                video_send_request(&state, "filehelper", "synthetic_client_id_0001", &video, 4)
                    .unwrap()
            ),
            expected["videoHex"]
        );
        // A note that fits one request keeps the single end-flagged request shape.
        let encoded = [vec![2], b"#!SILK_V3".to_vec(), vec![0; 6]].concat();
        let upload = VoiceUpload::new(
            "filehelper",
            "synthetic_client_id_0001",
            1234567,
            4000,
            &encoded,
        )
        .unwrap();
        assert_eq!((upload.request_count(), upload.is_final()), (1, true));
        assert_eq!(
            hex::encode(upload.request(&state).unwrap()),
            expected["voiceHex"]
        );
    }
    fn synthetic_note(len: usize) -> Vec<u8> {
        let mut note = [vec![2], b"#!SILK_V3".to_vec()].concat();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        note.extend((note.len()..len).map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        }));
        note
    }
    fn voice_reply_bytes(code: i32, extra: &[Vec<u8>]) -> Vec<u8> {
        let mut parts = vec![p::bytes(10, &p::int32(1, code))];
        parts.extend_from_slice(extra);
        p::message(&parts).unwrap()
    }
    #[test]
    fn long_voice_note_uses_consecutive_ranges_of_one_client_id() {
        let state = synthetic_state();
        let client = "synthetic_voice_id";
        let note = synthetic_note(2 * VOICE_UPLOAD_CHUNK_BYTES + 100);
        let mut upload = VoiceUpload::new("filehelper", client, 1234567, 60_000, &note).unwrap();
        assert_eq!(upload.request_count(), 3);
        let mut sent = vec![];
        for (index, (offset, length)) in [
            (0, VOICE_UPLOAD_CHUNK_BYTES),
            (VOICE_UPLOAD_CHUNK_BYTES, VOICE_UPLOAD_CHUNK_BYTES),
            (2 * VOICE_UPLOAD_CHUNK_BYTES, 100),
        ]
        .into_iter()
        .enumerate()
        {
            let f = p::decode(&upload.request(&state).unwrap()).unwrap();
            assert_eq!(p::data(&f, 5).unwrap(), client.as_bytes());
            assert_eq!(p::scalar(&f, 3).unwrap(), offset as u64);
            assert_eq!(p::scalar(&f, 4).unwrap(), length as u64);
            assert_eq!(p::scalar(&f, 7).unwrap(), 60_000);
            assert_eq!(p::scalar(&f, 9).unwrap(), u64::from(index == 2));
            assert_eq!(p::scalar(&f, 11).unwrap(), 4);
            // Later ranges continue the server message ID returned by the first.
            assert_eq!(
                p::optional(&f, 6).unwrap().is_some(),
                index > 0,
                "range {index}"
            );
            if index > 0 {
                assert_eq!(p::scalar(&f, 6).unwrap(), 77);
            }
            let data = p::unbuffer(p::data(&f, 8).unwrap()).unwrap();
            assert_eq!(data.len(), length);
            sent.extend(data);
            let reply = if index < 2 {
                voice_reply_bytes(
                    0,
                    &[
                        p::uint(3, offset as u64),
                        p::uint(4, length as u64),
                        p::bytes(6, client.as_bytes()),
                        p::uint(7, 77),
                        p::uint(9, 0),
                    ],
                )
            } else {
                let mut extra = vec![p::bytes(6, client.as_bytes()), p::uint(12, 9_000)];
                extra.extend([3, 4, 5, 7, 8, 9].map(|n| p::uint(n, 0)));
                voice_reply_bytes(0, &extra)
            };
            let result = upload.accept_reply(&reply).unwrap();
            assert_eq!(result.is_some(), index == 2);
            if let Some(result) = result {
                assert_eq!(result.status, SendStatus::ServerAccepted);
                assert_eq!(result.server_message_id.as_deref(), Some("9000"));
            }
        }
        assert_eq!(sent, note);
    }
    #[test]
    fn long_voice_note_partial_or_odd_replies_stop_without_acceptance() {
        let note = synthetic_note(2 * VOICE_UPLOAD_CHUNK_BYTES + 100);
        let start =
            || VoiceUpload::new("filehelper", "synthetic_voice_id", 1, 60_000, &note).unwrap();
        let progress = voice_reply_bytes(0, &[p::uint(7, 5), p::uint(9, 0)]);
        // Rejecting the first range leaves nothing on the server.
        let mut upload = start();
        let result = upload
            .accept_reply(&voice_reply_bytes(-2, &[]))
            .unwrap()
            .unwrap();
        assert_eq!(
            (result.status, result.service_code),
            (SendStatus::Rejected, -2)
        );
        // A later rejection follows accepted ranges and needs review.
        let mut upload = start();
        assert!(upload.accept_reply(&progress).unwrap().is_none());
        let result = upload
            .accept_reply(&voice_reply_bytes(-2, &[]))
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SendStatus::NeedsReview);
        let mut upload = start();
        assert!(upload.accept_reply(&progress).unwrap().is_none());
        assert!(upload.accept_reply(&progress).unwrap().is_none());
        assert!(upload.is_final());
        let result = upload
            .accept_reply(&voice_reply_bytes(-2, &[]))
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SendStatus::NeedsReview);
        // An early end flag or a changed server message ID is an uncertain error.
        let mut upload = start();
        assert!(upload
            .accept_reply(&voice_reply_bytes(0, &[p::uint(9, 1)]))
            .is_err());
        let mut upload = start();
        assert!(upload.accept_reply(&progress).unwrap().is_none());
        assert!(upload
            .accept_reply(&voice_reply_bytes(0, &[p::uint(7, 6)]))
            .is_err());
        // A final ID that differs from one announced earlier needs review.
        let mut upload = start();
        let early_id = voice_reply_bytes(0, &[p::uint(12, 41)]);
        assert!(upload.accept_reply(&early_id).unwrap().is_none());
        assert!(upload.accept_reply(&early_id).unwrap().is_none());
        let mut extra = vec![p::bytes(6, b"synthetic_voice_id"), p::uint(12, 42)];
        extra.extend([3, 4, 5, 7, 8, 9].map(|n| p::uint(n, 0)));
        let result = upload
            .accept_reply(&voice_reply_bytes(0, &extra))
            .unwrap()
            .unwrap();
        assert_eq!(result.status, SendStatus::NeedsReview);
    }
    #[test]
    fn voice_note_bounds_follow_the_sixty_second_profile() {
        let note = synthetic_note(silk::MAX_NOTE_BYTES);
        assert!(VoiceUpload::new("filehelper", "synthetic_voice_id", 1, 60_000, &note).is_ok());
        assert!(VoiceUpload::new("filehelper", "synthetic_voice_id", 1, 60_001, &note).is_err());
        let oversized = synthetic_note(silk::MAX_NOTE_BYTES + 1);
        assert!(
            VoiceUpload::new("filehelper", "synthetic_voice_id", 1, 60_000, &oversized).is_err()
        );
        assert!(local_voice_xml(silk::MAX_NOTE_BYTES, 60_000).is_ok());
        assert!(local_voice_xml(silk::MAX_NOTE_BYTES + 1, 60_000).is_err());
        assert!(local_voice_xml(1000, 60_001).is_err());
        assert!(local_voice_xml(1000, 0).is_err());
    }
    #[test]
    fn largest_voice_request_fits_one_short_link_record() {
        // Worst case: longest accepted identities, a full incompressible range,
        // either packet mode and the longest host name the route can carry.
        let mut state = synthetic_state();
        state.own_username = "u".repeat(256);
        let destination = "d".repeat(512);
        let client = "c".repeat(128);
        let note = synthetic_note(silk::MAX_NOTE_BYTES);
        let upload = VoiceUpload::new(&destination, &client, u32::MAX, 60_000, &note).unwrap();
        let request = upload.request(&state).unwrap();
        let path = b"/cgi-bin/micromsg-bin/uploadvoice";
        for keys in [vec![], vec![2; 24]] {
            state.request_key = keys.clone();
            state.response_key = keys;
            let packet = crate::business::AuthenticatedPacket::prepare(&state, 127, &request)
                .unwrap()
                .packet
                .clone();
            let route = crate::mmtls::section(
                &[
                    (path.len() as u16).to_be_bytes().as_slice(),
                    path,
                    &253u16.to_be_bytes(),
                    &[b'h'; 253],
                    &crate::mmtls::section(&packet),
                ]
                .concat(),
            );
            assert!(route.len() <= 65519, "{}", route.len());
            assert!(crate::mmtls::seal(0x17, &route, &[0; 16], &[0; 12], 2).is_ok());
        }
    }
    #[test]
    fn voice_response_needs_unique_correlation() {
        let mut parts = vec![p::bytes(10, &p::uint(1, 0)), p::uint(12, u64::MAX)];
        for n in [3, 4, 5, 7, 8, 9] {
            parts.push(p::uint(n, 0));
        }
        let no_echo = p::message(&parts).unwrap();
        assert_eq!(
            decode_voice_send(&no_echo, "synthetic_voice_id")
                .unwrap()
                .status,
            SendStatus::NeedsReview
        );
        parts.push(p::bytes(6, b"synthetic_voice_id"));
        let result = decode_voice_send(&p::message(&parts).unwrap(), "synthetic_voice_id").unwrap();
        assert_eq!(result.status, SendStatus::ServerAccepted);
        assert_eq!(result.server_message_id, Some(u64::MAX.to_string()));
        parts.push(p::bytes(2, b"synthetic_voice_id"));
        assert_eq!(
            decode_voice_send(&p::message(&parts).unwrap(), "synthetic_voice_id")
                .unwrap()
                .status,
            SendStatus::NeedsReview
        );
    }
    #[test]
    fn xml_escapes_filename_and_rejects_paths() {
        use crate::media::MediaDescriptor;
        let mut upload = UploadedMedia {
            descriptor: MediaDescriptor {
                file_id: "synthetic-file-id".into(),
                aes_key: vec![1; 16],
                quality: MediaQuality::File,
                expected_bytes: Some(3),
                expected_md5: Some("0".repeat(32)),
                strict_size: true,
                expires_at: None,
            },
            plaintext_bytes: 3,
            md5: "0".repeat(32),
            sha256: "0".repeat(64),
            file_name: "test&<.txt".into(),
            width: None,
            height: None,
        };
        let xml = file_xml("synthetic-user", &upload).unwrap();
        assert!(xml.contains("test&amp;&lt;.txt"));
        upload.file_name = "../secret".into();
        assert!(file_xml("synthetic-user", &upload).is_err());
    }
    #[test]
    fn rejected_response_does_not_require_success_fields() {
        let reply = p::bytes(1, &p::int32(1, -2));
        assert_eq!(
            decode_file_send(&reply, "client").unwrap().status,
            SendStatus::Rejected
        );
    }
}
