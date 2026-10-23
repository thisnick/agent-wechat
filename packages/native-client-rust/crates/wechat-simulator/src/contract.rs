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
    SendQuote {
        session: Session,
        destination: String,
        content: String,
        created_at: u32,
        client_id: u32,
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
    GroupDetail {
        session: Session,
        room: String,
    },
    Room {
        session: Session,
        room: String,
    },
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

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupFault {
    None,
    Reject,
    PartialMembers,
    LostAcknowledgment,
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
    },
    GroupFault {
        account: String,
        fault: GroupFault,
    },
    SendFault {
        account: String,
        fault: SendFault,
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
    },
}
fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}
