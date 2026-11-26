use serde::{Deserialize, Serialize};
use wechat_protocol::{AccountState, DeviceProfile, LoginStatus};

pub const HOST: &str = "simulation.invalid.wechat.com";
pub const MAX_BODY: usize = 1024 * 1024;
pub const MAX_MEDIA_BODY: usize = 36 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub account: String,
    pub generation: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    StartLogin {
        account: String,
        device: DeviceProfile,
        computer_name: String,
        saved: bool,
    },
    PollLogin {
        challenge: String,
    },
    CompleteLogin {
        challenge: String,
    },
    RefreshLogin {
        challenge: String,
    },
    Logout {
        session: Session,
    },
    Heartbeat {
        session: Session,
        cursor: Vec<u8>,
    },
    Sync {
        session: Session,
        cursor: Vec<u8>,
    },
    SendText {
        session: Session,
        destination: String,
        content: String,
        created_at: u32,
        client_id: u32,
    },
    SendTextWithSource {
        session: Session,
        destination: String,
        content: String,
        created_at: u32,
        client_id: u32,
        source: String,
    },
    SendQuote {
        session: Session,
        destination: String,
        content: String,
        created_at: u32,
        client_id: u32,
        #[serde(default)]
        source: Option<String>,
    },
    Poll {
        session: Session,
        after: u64,
    },
    Media {
        session: Session,
        request: MediaRequest,
    },
    Read {
        session: Session,
        notification: wechat_protocol::SessionReadNotification,
    },
    Contacts {
        session: Session,
        ids: Vec<String>,
    },
    Avatar {
        session: Session,
        username: String,
    },
    GroupMutation {
        session: Session,
        request: wechat_protocol::GroupRpc,
    },
    ContactMutation {
        session: Session,
        request: wechat_protocol::ContactRpc,
    },
    GroupDetail {
        session: Session,
        room: String,
    },
    Room {
        session: Session,
        room: String,
    },
}
impl Request {
    /// The request's `op` tag, for counting what the hub asked for.
    pub fn op(&self) -> &'static str {
        match self {
            Self::StartLogin { .. } => "start_login",
            Self::PollLogin { .. } => "poll_login",
            Self::CompleteLogin { .. } => "complete_login",
            Self::RefreshLogin { .. } => "refresh_login",
            Self::Logout { .. } => "logout",
            Self::Heartbeat { .. } => "heartbeat",
            Self::Sync { .. } => "sync",
            Self::SendText { .. } => "send_text",
            Self::SendTextWithSource { .. } => "send_text_with_source",
            Self::SendQuote { .. } => "send_quote",
            Self::Poll { .. } => "poll",
            Self::Media { .. } => "media",
            Self::Read { .. } => "read",
            Self::Contacts { .. } => "contacts",
            Self::Avatar { .. } => "avatar",
            Self::GroupMutation { .. } => "group_mutation",
            Self::ContactMutation { .. } => "contact_mutation",
            Self::GroupDetail { .. } => "group_detail",
            Self::Room { .. } => "room",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaRequest {
    Upload {
        metadata: wechat_protocol::media::UploadedMedia,
        encrypted: String,
    },
    Download {
        descriptor: wechat_protocol::media::MediaDescriptor,
    },
    Sticker {
        descriptor: wechat_protocol::media::StickerDescriptor,
    },
    Submit {
        destination: String,
        client_id: String,
        created_at: u32,
        message_type: u32,
        content: String,
        voice: Option<String>,
    },
    Voice {
        server_id: u64,
        length: u32,
        offset: u32,
    },
}

#[derive(Serialize, Deserialize)]
pub struct Challenge {
    pub id: String,
    pub qr_png: Vec<u8>,
    pub expires_at: u32,
    pub status: LoginStatus,
}

#[derive(Serialize, Deserialize)]
pub struct Authorization {
    pub state: AccountState,
}

#[derive(Serialize, Deserialize)]
pub struct SendReply {
    pub accepted: bool,
    pub server_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PollReply {
    pub sequence: u64,
    pub notified: bool,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendFault {
    #[default]
    None,
    Reject,
    LostAcknowledgment,
}

/// Error code the scenario service returns for a refused saved-device sign-in.
/// Its response also carries the refusal's `serviceCode`.
pub const SAVED_LOGIN_REFUSED: &str = "saved_device_login_refused";

/// The scenario refused a saved-device sign-in with this service code, as
/// set by [`Control::SavedLoginFault`]. The adapter turns it back into the
/// typed saved-device refusal the native client reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedLoginRefused(pub i32);

impl std::fmt::Display for SavedLoginRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SAVED_LOGIN_REFUSED)
    }
}

impl std::error::Error for SavedLoginRefused {}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupFault {
    None,
    Reject,
    PartialMembers,
    LostAcknowledgment,
}

/// Metadata lookups that [`Control::MetadataFault`] can fail.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataKind {
    Contacts,
    Room,
    GroupDetail,
}
impl MetadataKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Contacts => "contacts",
            Self::Room => "room",
            Self::GroupDetail => "group_detail",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Login {
        account: String,
        status: LoginStatus,
    },
    Advance {
        seconds: u32,
    },
    Inbound {
        account: String,
        peer: String,
        body: String,
        #[serde(default)]
        self_sent: bool,
        #[serde(default = "yes")]
        notify: bool,
        /// The contact speaking in a group conversation. Required when `peer` is a
        /// joined group and refused otherwise; groups carry no sender of their own.
        #[serde(default)]
        sender: Option<String>,
    },
    GroupFault {
        account: String,
        fault: GroupFault,
    },
    /// Another member changed a joined group. Its name or announcement is
    /// updated, its member and detail versions advance, and the group's contact
    /// record is delivered through sync, so the hub refreshes it.
    GroupChange {
        account: String,
        room: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        announcement: Option<String>,
    },
    /// The account's next `count` valid lookups of one metadata kind, for any
    /// group or contact, fail as if the network dropped them. `count` 0 clears
    /// the fault. The state reports what remains.
    MetadataFault {
        account: String,
        kind: MetadataKind,
        count: u32,
    },
    SendFault {
        account: String,
        fault: SendFault,
    },
    /// Refuses the account's saved-device sign-ins with `code` until cleared
    /// with `null`. QR sign-ins are unaffected.
    SavedLoginFault {
        account: String,
        code: Option<i32>,
    },
    Revoke {
        account: String,
    },
    Notify {
        account: String,
        #[serde(default = "one")]
        count: u32,
    },
    UnknownSync {
        account: String,
    },
    EchoSend {
        account: String,
        client_id: u32,
    },
    StatusNotification {
        account: String,
        operation_id: u32,
    },
    AdvertiseImage {
        account: String,
        peer: String,
    },
    ReceiveMedia {
        account: String,
        peer: String,
        kind: String,
        /// The contact sending into a group conversation, as for `Inbound`.
        #[serde(default)]
        sender: Option<String>,
    },
}
fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}
