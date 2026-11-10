//! Canonical wire contracts, consumed by response validation and SDK generation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// A Mini Program known to the selected account.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgram {
    /// Public Mini Program AppID.
    pub app_id: String,
    /// Name shown in the known-app list.
    pub title: String,
    /// Whether the hub has a staged package for this program.
    pub available: bool,
}

/// Mini Programs known from recent saved cards and the selected account's cache.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
pub struct NativeMiniPrograms {
    /// Known apps and their package availability.
    pub items: Vec<NativeMiniProgram>,
}

/// Compiled entry page selected for a Mini Program launch.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgramEntry {
    /// Page route within the package.
    pub path: String,
    /// Archive path of the compiled page's service module.
    pub service_path: String,
    /// Query string of the shared entry, without its leading question mark.
    pub query: String,
    /// Compiled component framework selected by the runner.
    pub framework: String,
    /// Renderer used by the local runner.
    pub renderer: String,
}

/// One verified archive selected for a Mini Program launch.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgramPackage {
    /// Package ID to use when downloading this archive.
    pub id: String,
    /// Exact cached version to request.
    pub version: String,
    /// Byte length of the original archive.
    pub size_bytes: u32,
    /// SHA-256 of the original archive, in lowercase hex.
    pub sha256: String,
    /// Declared root for a cached subpackage. Omitted for the main archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root: Option<String>,
}

/// Account and app permissions available to the local runner for this launch.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgramCapabilities {
    /// Whether the runner may request app login codes.
    pub login: bool,
    /// Whether this launch can share its current page to a chat.
    #[serde(default)]
    pub share_to_chat: bool,
}

/// Select a default entry or resolve a saved card on the selected account.
#[derive(Default, Deserialize, Serialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMiniProgramLaunchRequest {
    /// Saved card message ID. Omit to use the program's default page.
    #[serde(default)]
    #[ts(optional)]
    pub source_message_id: Option<String>,
}

/// Server-provided request policy for the selected Mini Program.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgramNetworkPolicy {
    /// Domain patterns used by the host's request-domain check.
    pub request_domains: Vec<String>,
}

/// Package and entry selection for one Mini Program launch.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMiniProgramLaunch {
    /// Temporary launch ID bound to the initiating account and token.
    pub launch_id: String,
    /// Page selected for the local runner.
    pub entry: NativeMiniProgramEntry,
    /// Verified archives to download before execution.
    pub packages: Vec<NativeMiniProgramPackage>,
    /// Operations the runner can request for this launch.
    pub capabilities: NativeMiniProgramCapabilities,
    /// Current server policy, or null when the account is offline.
    #[serde(default)]
    pub network_policy: Option<NativeMiniProgramNetworkPolicy>,
}

/// A hub operation requested by a running Mini Program.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(tag = "method")]
#[serde(deny_unknown_fields)]
pub enum NativeMiniProgramIpcRequest {
    /// Issue or replay the code for this app's login invocation.
    #[serde(rename = "wx.login")]
    Login {},
}

/// Temporary app-specific code returned to the authorized local runner.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
pub struct NativeMiniProgramLogin {
    /// WeChat code for the selected app's backend. Treat it as sensitive.
    pub code: String,
}

/// Who a message mentions, read from the mention list saved with the message.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMentions {
    /// `known` when the mention list was read, `unknown` when it is missing or cannot be parsed.
    /// When unknown, `userIds` is empty and the flags are null.
    pub state: String,
    /// IDs of the members mentioned by name, not counting the mention-everyone marker (see `all`).
    pub user_ids: Vec<String>,
    /// True when this account is among the mentioned members, false when it is not, and null when
    /// the mentions are unknown.
    #[serde(rename = "self")]
    pub self_mention: Option<bool>,
    /// True when the message mentions everyone, false when it does not, and null when the mentions
    /// are unknown.
    pub all: Option<bool>,
    /// Where the mentions were read from, currently `msgsource.atuserlist`. Null when there is no
    /// source, for example when `state` is `unknown`.
    pub source: Option<String>,
}
/// A saved message as read from the account's profile. It carries only readable fields and never
/// raw message XML.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMessage {
    /// Message ID. Pass it back unchanged to routes that take a message ID.
    pub id: String,
    /// ID of the account the message belongs to.
    pub account_id: String,
    /// ID of the chat the message is in.
    pub thread_id: String,
    /// WeChat's own ID for the message, as a string because it can exceed the largest whole number
    /// JavaScript holds exactly. Null when the saved message has none.
    pub server_id: Option<String>,
    /// ID of the sender. Null for system notices and when the sender cannot be determined.
    pub sender_id: Option<String>,
    /// WeChat's numeric message type, such as 1 text, 3 image, 34 voice, 43 video, 47 sticker, 48
    /// location, 49 app message (links, files, quotes) and 10000 system notice.
    #[serde(rename = "type")]
    pub message_type: i32,
    /// For app messages (type 49), the app subtype, such as 5 link, 6 file, 8 shared sticker, 19
    /// forwarded chat and 57 quote reply. Null for other messages.
    #[serde(default)]
    #[ts(optional)]
    pub app_message_type: Option<i32>,
    /// The message this one replies to, when it is a quote reply. Null otherwise.
    #[serde(default)]
    #[ts(optional)]
    pub quote: Option<NativeQuoteReference>,
    /// Link card details when the message is a shared link, article, music or video card, or a
    /// Channels post. Null otherwise.
    #[serde(default)]
    #[ts(optional)]
    pub link: Option<wechat_core::LinkCard>,
    /// Mini Program card read from the saved message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mini_program: Option<wechat_core::MiniProgramCard>,
    /// Pin details when the message is a static location. Omitted for other messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<wechat_core::LocationCard>,
    /// Recall details. Present only when the message was recalled, in which case `body` is
    /// `[Message recalled]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recall: Option<NativeRecallInfo>,
    /// `outgoing` when this account sent the message and `incoming` when someone else did. Null for
    /// system notices and when it cannot be determined.
    pub direction: Option<String>,
    /// Readable text of the message, such as the text itself or a summary of a link, location or
    /// forwarded chat. Null when there is nothing readable, for example for a photo.
    pub body: Option<String>,
    /// When the message was created, as an RFC 3339 time. Null when the saved time is missing.
    pub created_at: Option<String>,
    /// Which members the message mentions.
    pub mentions: NativeMentions,
    /// Fingerprint of the saved profile data this message was read from. It changes whenever the
    /// profile files change, not only when this message changes.
    pub version: String,
    /// Always an empty list today. Use the message's media routes to list its attachments.
    #[ts(type = "unknown[]")]
    pub media: Vec<Value>,
}
/// Details of a recalled message.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeRecallInfo {
    /// When the daemon first saw the recall, as an RFC 3339 time.
    pub observed_at: String,
    /// When WeChat says the message was recalled, as an RFC 3339 time. Omitted when that time is
    /// not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recalled_at: Option<String>,
    /// The notice text WeChat saved in place of the message. Omitted or null when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub replacement_text: Option<String>,
}
/// The message a quote reply refers to. Its content comes only from the excerpt carried inside the
/// reply, never from a saved copy of the original.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeQuoteReference {
    /// WeChat's ID of the quoted message, as a string.
    pub server_id: String,
    /// WeChat's numeric type of the quoted message, with the same values as `type` on a message.
    #[serde(rename = "type")]
    pub message_type: i32,
    /// How much of the quote could be shown: `embedded` when the reply carries readable content,
    /// `missing` when it carries none, `unsupported` for a kind of message that cannot be shown as
    /// a quote, and `recalled` when the original was recalled and its content is hidden.
    #[serde(default)]
    #[ts(optional)]
    pub state: Option<String>,
    /// ID of the quoted message's sender, when the reply says who it was.
    #[serde(default)]
    #[ts(optional)]
    pub sender_id: Option<String>,
    /// Sender's display name carried inside the reply, when supplied by WeChat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sender_name: Option<String>,
    /// `outgoing` when the quoted message was sent by this account and `incoming` when someone else
    /// sent it. Null when that cannot be told.
    #[serde(default)]
    #[ts(optional)]
    pub direction: Option<String>,
    /// App subtype of the quoted message when it is an app message (type 49), such as 5 for a link.
    /// Null for other messages.
    #[serde(default)]
    #[ts(optional)]
    pub app_message_type: Option<i32>,
    /// Readable text of the quoted message, taken from the reply. Null when there is none.
    #[serde(default)]
    #[ts(optional)]
    pub embedded_text: Option<String>,
    /// True when the reply carries the quoted image, so it can be downloaded through the reply's
    /// media routes.
    #[serde(default)]
    #[ts(optional)]
    pub embedded_image: Option<bool>,
    /// Link card of the quoted message when it was a shared link, with the same fields as `link` on
    /// a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub embedded_link: Option<wechat_core::LinkCard>,
    /// Mini Program metadata retained by the reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub embedded_mini_program: Option<wechat_core::MiniProgramCard>,
    /// True when the quoted message is a sticker. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub embedded_sticker: Option<bool>,
}
/// A chat in the chat list: a direct chat, a group or a built-in chat such as File Transfer.
/// Summaries come from saved history and are not refreshed from WeChat.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeThread {
    /// Chat ID. For a direct chat it is the contact's username, and for a group it ends with
    /// `@chatroom`.
    pub id: String,
    /// ID of the account the chat belongs to.
    pub account_id: String,
    /// ID of the person or group on the other side of the chat. It is the same as `id`.
    pub peer_id: String,
    /// `direct` for a one-to-one chat, `group` for a group, and `system` for a built-in chat such
    /// as File Transfer.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub classification: Option<wechat_core::PeerClassification>,
    /// Name shown for the chat: your remark if set, else the contact's nickname or the group's
    /// name, else the ID.
    pub display_name: Option<String>,
    /// Number of unread messages as saved in the profile. Null when the saved data does not say.
    #[ts(type = "number | null")]
    pub unread_count: Option<i64>,
    /// Saved notification preference: true when the chat is muted and false when it is not. Null
    /// means the profile cannot establish it.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub muted: Option<bool>,
    /// Saved pin preference: true when the chat is pinned and false when it is not. Null means the
    /// profile cannot establish it.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub pinned: Option<bool>,
    /// Version of the chat's read state. Send it as `expectedVersion` when marking the chat read;
    /// the request is refused if the chat changed since.
    pub read_state_version: String,
    /// Preview of the latest message. Omitted when local message data is unavailable, and null when
    /// the chat has no messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_message: Option<NativeThreadMessage>,
    /// Up to four cached group members, for drawing the group's picture. Empty for direct chats or
    /// when the member list is unavailable.
    #[serde(default)]
    #[ts(optional)]
    pub avatar_members: Option<Vec<NativeThreadAvatarMember>>,
    /// How complete the saved history is. It currently always reports partial coverage and no local
    /// message count.
    #[ts(type = "unknown")]
    pub history: Value,
}
/// Preview of the latest message in a chat.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeThreadMessage {
    /// ID of the message, the same as `id` on the full message.
    pub id: String,
    /// ID of the sender. Null for system notices and when the sender is unknown.
    pub sender_id: Option<String>,
    /// Name of the sender, taken from the group's cached members or your contacts. Null when the
    /// message has no sender or the name is unknown.
    pub sender_name: Option<String>,
    /// `outgoing` when this account sent the message and `incoming` when someone else did. Null
    /// when it cannot be determined.
    pub direction: Option<String>,
    /// WeChat's numeric message type, with the same values as `type` on a message.
    #[serde(rename = "type")]
    pub message_type: i32,
    /// Plain-text preview of up to 160 characters. Media messages use labels such as Photo and
    /// Sticker, and a recalled message shows its recall notice.
    pub text: String,
    /// When the message was created, as an RFC 3339 time. Null when unknown.
    pub created_at: Option<String>,
}
/// A group member shown in the group's picture.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeThreadAvatarMember {
    /// Member ID.
    pub id: String,
    /// Name to show for the member.
    pub display_name: String,
}

/// Messages whose attachments to list without downloading anything.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMediaBatchRequest {
    /// IDs of the messages to look up, from 1 to 40 per request.
    pub message_ids: Vec<String>,
}
/// The attachments found for one message.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMediaBatchItem {
    /// ID of the message that was looked up.
    pub message_id: String,
    /// Attachments of the message. Empty when `error` is set.
    pub assets: Vec<wechat_core::MediaAsset>,
    /// Error code when this message's attachments could not be listed, otherwise null.
    pub error: Option<String>,
}
/// Attachments of several messages.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeMediaBatch {
    /// One entry for each requested message.
    pub items: Vec<NativeMediaBatchItem>,
}
/// A contact saved in the account's profile.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeContact {
    /// Contact ID. It is the same as `username`.
    pub id: String,
    /// The contact's WeChat username.
    pub username: String,
    /// Name to show: the remark if set, else the nickname, else the username.
    pub display_name: Option<String>,
    /// The name the contact chose for themselves. Null when none is saved.
    pub nickname: Option<String>,
    /// Your private name for the contact. Null when none is saved.
    pub remark: Option<String>,
    /// The custom WeChat ID the contact chose, which they can change. Null when none is saved.
    pub alias: Option<String>,
    /// Relationship to this account. It currently always reports `unknown`, including for saved
    /// friends.
    pub relation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub classification: Option<wechat_core::PeerClassification>,
}
/// A group chat saved in the account's profile.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroup {
    /// Group ID, ending with `@chatroom`. It is the same as `roomId`.
    pub id: String,
    /// Group ID, the same as `id`.
    pub room_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub classification: Option<wechat_core::PeerClassification>,
    /// Name to show for the group: your remark if set, else the group's name, else the ID.
    pub display_name: Option<String>,
    /// ID of the member who owns the group. Null when the saved data does not say.
    pub owner_id: Option<String>,
    /// IDs of the group's administrators. The owner is not listed unless WeChat lists them too, and
    /// the field is null when the saved data cannot say.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub administrator_ids: Option<Vec<String>>,
    /// Whether this account may mention everyone: true for the owner and administrators and false
    /// for other members. Null means permission is not established, and older servers omit the
    /// field.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub can_mention_all: Option<bool>,
    /// Text of the group's current announcement. Null when there is none.
    pub announcement: Option<String>,
    /// The group's own shared name. Null when none is saved.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<String>,
    /// Your private name for the group. Null when none is saved.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remark: Option<String>,
    /// True when notifications for the group are muted and false when they are on. Null when the
    /// saved data does not say.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub muted: Option<bool>,
    /// True when the group is minimized (folded) in the chat list and false when it is not. Null
    /// when the saved data does not say.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub minimized: Option<bool>,
    /// True when this account is still a member of the group and false when it is not. Null when
    /// the saved data does not say.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub is_in_group: Option<bool>,
    /// This account's own display name in the group. Null or omitted when it is not known.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub my_display_name: Option<String>,
}
/// A member of a group, as saved in the account's profile.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroupMember {
    /// Member ID. It is the same as `username`.
    pub id: String,
    /// The member's WeChat username.
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub classification: Option<wechat_core::PeerClassification>,
    /// Name to show for the member: your remark, else their name in this group, else their
    /// nickname, else their username.
    pub display_name: Option<String>,
    /// The member's own display name in this group. Null when none is saved.
    pub room_display_name: Option<String>,
    /// Name used for this member in mention text. It comes from their name in the group or their
    /// nickname, never from your private remark, and falls back to the username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mention_name: Option<String>,
}
/// Result of looking up one person in a group's saved member list.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroupMemberLookup {
    /// ID of the group that was searched.
    pub group_id: String,
    /// Username that was looked up.
    pub username: String,
    /// `present` when the person is in the saved member list and `unconfirmed` when they are not. A
    /// missing row does not prove they left, because the saved list can be incomplete.
    pub state: String,
    /// The member's details when `state` is `present`, otherwise null.
    pub member: Option<NativeGroupMember>,
    /// How the answer was obtained. It is currently always `cached`, meaning saved data only.
    pub coverage: String,
}
/// Request to create a group chat.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupCreateRequest {
    /// IDs of the contacts to add, from 2 to 40, without repeats and without this account's own ID.
    pub members: Vec<String>,
    /// Name for the new group, up to 512 bytes. Optional.
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
}
/// Request to rename a group.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupNameRequest {
    /// The new group name. It must not be blank and can be up to 512 bytes.
    pub name: String,
}
/// Members to add to or remove from a group.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupMembersRequest {
    /// IDs of the contacts, from 1 to 40, without repeats and without this account's own ID.
    pub members: Vec<String>,
}
/// Request to change this account's display name in a group.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupMyNameRequest {
    /// The new display name, up to 512 bytes.
    pub display_name: String,
}
/// Request to mute or unmute notifications for a chat.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeThreadNotificationsRequest {
    /// True to mute the chat and false to turn its notifications back on.
    pub muted: bool,
}
/// Request to pin or unpin a chat.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeThreadPinRequest {
    /// True to pin the chat and false to unpin it.
    pub pinned: bool,
}
/// Stands in for a resource that was in the list when it was first read but is no longer in the
/// saved profile. It keeps the resource's place in the list.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeTombstone {
    /// ID of the removed resource.
    pub id: String,
    /// Always true, marking the resource as removed.
    #[serde(deserialize_with = "true_only")]
    #[ts(type = "true")]
    pub deleted: bool,
    /// Fingerprint of the saved profile data in which the resource is missing, the same value as
    /// `sourceGeneration` on the page.
    pub version: String,
}
fn true_only<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<bool, D::Error> {
    let value = bool::deserialize(deserializer)?;
    if !value {
        return Err(serde::de::Error::custom("tombstone must be deleted"));
    }
    Ok(value)
}
/// One entry on a page: the resource itself, or a tombstone when the resource has been removed
/// since the list was first read.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "NativePageItemOf{T}")]
pub enum NativePageItem<T> {
    Resource(T),
    Tombstone(NativeTombstone),
}
/// One page of a list. `items` holds the resources, `nextCursor` continues the list, and the other
/// fields describe the saved data the list was read from.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "NativePageOf{T}")]
pub struct NativePage<T> {
    /// The entries on this page, in list order.
    pub items: Vec<NativePageItem<T>>,
    /// Pass it as `cursor` with the same query to get the next page. Null on the last page.
    pub next_cursor: Option<String>,
    /// Which saved databases the read covered, as `partial` (true when some could not be read) and
    /// `databases` (each one with its state). It describes local data, not completeness on WeChat's
    /// servers.
    #[ts(type = "unknown")]
    pub coverage: Value,
    /// Fingerprint of the saved profile data the page was read from. It changes when the profile
    /// files change.
    pub source_generation: String,
    /// Event cursor that was current when the list was first read. Resume the event stream from it
    /// to catch changes made after the read.
    #[serde(default)]
    #[ts(optional)]
    pub as_of_event_cursor: Option<String>,
}
/// A background operation such as signing in or out, refreshing contacts or changing a group. Poll
/// it by ID until it finishes.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    /// Operation ID.
    pub id: String,
    /// ID of the account the operation belongs to.
    pub account_id: String,
    /// What the operation does, such as `login`, `logout`, `sync`, `metadata`, `group_mutation`,
    /// `message_recall` or `media-writeback`.
    pub kind: String,
    /// Where the operation stands, such as `queued`, `running`, `succeeded`, `failed` or
    /// `cancelled`. Some kinds report more states, for example `needs_review`, or `awaiting_scan`
    /// and `awaiting_approval` during a login.
    pub state: String,
    /// Details of the outcome. Its fields depend on `kind`, and it is empty until the operation has
    /// something to report.
    #[ts(type = "unknown")]
    pub result: Value,
    /// When the operation was created, as an RFC 3339 time.
    pub created_at: String,
    /// When the operation last changed, as an RFC 3339 time.
    pub updated_at: String,
}
/// A text or share-card send and how far it got. Media sends are reported as media jobs instead.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeSendIntent {
    /// Send ID, used to look the send up or cancel it.
    pub id: String,
    /// ID of the account that is sending.
    pub account_id: String,
    /// ID of the chat the message is sent to.
    pub destination: String,
    /// `queued`, `submitting`, `accepted` (WeChat took it), `failed` (WeChat refused it),
    /// `needs_review` (the outcome is unknown, so check the chat before sending again) or
    /// `cancelled`.
    pub state: String,
    /// ID the daemon gave the message when it submitted it to WeChat.
    pub client_id: Option<String>,
    /// WeChat's ID of the message once it was accepted, as a string. Null until then.
    pub server_id: Option<String>,
    /// Why the send failed or needs review. Null otherwise.
    pub error_code: Option<String>,
    /// ID of the saved message that represents this send, once it appears in the profile. Omitted
    /// until then.
    #[serde(default)]
    #[ts(optional)]
    pub local_message_id: Option<String>,
    /// When the send was created, as an RFC 3339 time.
    pub created_at: String,
}
/// An entry in the account's event log, which keeps only about the most recent 10,000 events.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeEvent {
    /// Position of the event in the log, a decimal number written as a string that only increases.
    /// Pass the last one you handled as `after` to resume.
    pub cursor: String,
    /// Kind of event, such as `message.committed`, `thread.updated`, `group.updated`,
    /// `operation.updated` or `send.updated`.
    pub kind: String,
    /// Details of the event. The fields depend on `kind`; for example `message.committed` carries
    /// `messageId` and `threadId`.
    #[ts(type = "Record<string, unknown>")]
    pub data: Value,
    /// When the event was recorded, as an RFC 3339 time.
    pub created_at: String,
}
/// Which daemon is running and what it supports.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeIdentity {
    /// Product name, `agent-wechat`.
    pub product: String,
    /// Backend in use, normally `native`.
    pub backend: String,
    /// Version of the daemon.
    pub version: String,
    /// Version of this HTTP API, currently 1.
    pub api_version: u32,
    /// Platform the daemon was built for, as a Rust target triple such as
    /// `x86_64-unknown-linux-gnu`.
    #[serde(default)]
    #[ts(optional)]
    pub target: Option<String>,
    /// Names of the features this daemon supports, such as `text-send` and `events`.
    pub capabilities: Vec<String>,
}
/// Identity and state of the running daemon.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeStatus {
    /// ID of this daemon's data directory. It is created once and kept across restarts.
    pub server_id: String,
    /// Path of the data directory on the daemon's host.
    pub data_dir: String,
    /// Process ID of the daemon.
    pub pid: u32,
    /// Random ID created each time the daemon starts. A shutdown request must repeat it.
    pub ownership_generation: String,
    /// Identity and build of the daemon.
    pub version: NativeIdentity,
    /// Daemon state. It is `ready` while the daemon is serving requests.
    pub state: String,
}
/// Request to send a text message, optionally as a quote reply or with mentions.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTextSendRequest {
    /// ID of the chat to send to.
    pub thread_id: String,
    /// The message text. A quote reply can hold up to 16 KiB and other text up to 64 KiB, including
    /// mention names.
    pub text: String,
    /// The saved message this text replies to. The token also needs the `read` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quote: Option<wechat_core::QuoteSendReference>,
    /// Group members to mention. Only allowed when sending to a group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mentions: Option<wechat_core::TextMentions>,
}
/// Request to recall a message this account sent.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRecallRequest {
    /// The message's `serverId`, copied as a string. The recall is refused if it does not match the
    /// message in the URL.
    pub server_id: String,
}
/// Request to mark a chat as read.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeReadRequest {
    /// The chat's `readStateVersion` from when you read it. The request is refused if the chat has
    /// changed since.
    pub expected_version: String,
}

/// Request to delete the account's saved login credentials.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeForgetRequest {
    /// Must be true. It confirms that the credentials should be deleted.
    pub confirm: bool,
}
/// Request to repair saved media records from bytes the daemon already holds. It never downloads or
/// sends anything.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMediaWritebackRequest {
    /// How many saved media records to repair in this request, from 1 to 100.
    #[ts(type = "number")]
    pub limit: usize,
}

/// Validate the public response before serializing it, so generated clients and
/// Value-producing profile adapters cannot silently develop incompatible shapes.
pub fn checked<T: serde::de::DeserializeOwned>(value: Value) -> anyhow::Result<Value> {
    let _: T = serde_json::from_value(value.clone())?;
    Ok(value)
}

/// Options for retrying a media job.
#[derive(Default, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMediaJobRetryRequest {
    /// True to reuse the upload a send job already finished instead of uploading again. The
    /// retry-completed-upload route requires true here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reuse_completed_upload: Option<bool>,
}

/// Request to remove a signed-out account.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAccountRemovalRequest {
    /// Must repeat the account ID from the URL, to confirm the removal.
    pub confirm_account_id: String,
}
/// Result of removing an account.
#[derive(Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NativeAccountRemoval {
    /// ID of the account that was removed.
    pub account_id: String,
    /// True when the account was removed.
    pub removed: bool,
    /// True when the account's local files were kept. They are always kept.
    pub files_retained: bool,
}

pub fn typescript() -> String {
    let declarations = [
        wechat_core::call_types::CallClientHello::decl(),
        wechat_core::call_types::CallParticipant::decl(),
        wechat_core::call_types::CallMembership::decl(),
        wechat_core::call_types::CallParticipantStatus::decl(),
        wechat_core::call_types::CallParticipantStatusKind::decl(),
        wechat_core::call_types::CallConferenceInvitation::decl(),
        wechat_core::call_types::CallInvitationResult::decl(),
        wechat_core::call_types::CallInvitationStatus::decl(),
        wechat_core::call_types::CallProtocolErrorCode::decl(),
        wechat_core::call_types::CallCapabilities::decl(),
        wechat_core::call_types::CallAudioFormat::decl(),
        wechat_core::call_types::CallTarget::decl(),
        wechat_core::call_types::CallKind::decl(),
        wechat_core::call_types::CallType::decl(),
        wechat_core::call_types::CallDirection::decl(),
        wechat_core::call_types::CallState::decl(),
        wechat_core::call_types::CallOffer::decl(),
        wechat_core::call_types::CallPendingPhase::decl(),
        wechat_core::call_types::CallPendingOffer::decl(),
        wechat_core::call_types::CallReadyScope::decl(),
        wechat_core::call_types::CallView::decl(),
        wechat_core::call_types::CallControl::decl(),
        wechat_core::call_types::CallParticipantControl::decl(),
        wechat_core::call_types::CallsAvailability::decl(),
        wechat_core::call_types::CallSnapshot::decl(),
        wechat_core::call_types::CallCommand::decl(),
        wechat_core::call_types::CallCommandStatus::decl(),
        wechat_core::call_types::CallErrorCode::decl(),
        wechat_core::call_types::CallTrackDirection::decl(),
        wechat_core::call_types::CallAudioTrack::decl(),
        wechat_core::call_types::CallEvent::decl(),
        wechat_core::call_types::CallMediaCommand::decl(),
        NativeMentions::decl(),
        NativeQuoteReference::decl(),
        wechat_core::LinkCard::decl(),
        wechat_core::LocationCoordinateSystem::decl(),
        wechat_core::LocationCard::decl(),
        NativeRecallInfo::decl(),
        NativeMessage::decl(),
        wechat_core::PeerCategory::decl(),
        wechat_core::PeerNamespace::decl(),
        wechat_core::PeerBusinessKind::decl(),
        wechat_core::PeerWording::decl(),
        wechat_core::PeerApplication::decl(),
        wechat_core::PeerOpenIm::decl(),
        wechat_core::PeerClassification::decl(),
        NativeThread::decl(),
        NativeThreadMessage::decl(),
        NativeThreadAvatarMember::decl(),
        NativeContact::decl(),
        NativeGroup::decl(),
        NativeGroupMember::decl(),
        NativeGroupMemberLookup::decl(),
        NativeGroupCreateRequest::decl(),
        NativeGroupNameRequest::decl(),
        NativeGroupMembersRequest::decl(),
        NativeGroupMyNameRequest::decl(),
        NativeThreadNotificationsRequest::decl(),
        NativeThreadPinRequest::decl(),
        NativeTombstone::decl(),
        NativePageItem::<String>::decl(),
        NativePage::<String>::decl(),
        wechat_core::MiniProgramCard::decl(),
        NativeMiniProgram::decl(),
        NativeMiniPrograms::decl(),
        NativeMiniProgramEntry::decl(),
        NativeMiniProgramPackage::decl(),
        NativeMiniProgramCapabilities::decl(),
        NativeMiniProgramLaunchRequest::decl(),
        NativeMiniProgramLaunch::decl(),
        NativeMiniProgramNetworkPolicy::decl(),
        NativeMiniProgramIpcRequest::decl(),
        NativeMiniProgramLogin::decl(),
        NativeOperation::decl(),
        NativeSendIntent::decl(),
        NativeEvent::decl(),
        NativeIdentity::decl(),
        NativeStatus::decl(),
        wechat_store::AccountSettings::decl(),
        NativeAccountRemovalRequest::decl(),
        NativeAccountRemoval::decl(),
        wechat_core::QuoteSendReference::decl(),
        wechat_core::TextMentionPosition::decl(),
        wechat_core::TextMentions::decl(),
        NativeTextSendRequest::decl(),
        wechat_core::ShareSendRequest::decl(),
        NativeRecallRequest::decl(),
        NativeReadRequest::decl(),
        NativeForgetRequest::decl(),
        NativeMediaWritebackRequest::decl(),
        NativeMediaJobRetryRequest::decl(),
        wechat_core::OfficialProfileRequest::decl(),
        wechat_core::OfficialProfileSelection::decl(),
        wechat_core::LoginStrategy::decl(),
        wechat_core::QrRefreshMode::decl(),
        wechat_core::QrRefresh::decl(),
        wechat_core::LoginRequest::decl(),
        wechat_core::media_types::MediaKind::decl(),
        wechat_core::media_types::MediaVariant::decl(),
        wechat_core::media_types::VideoSendQuality::decl(),
        wechat_core::media_types::MediaTransferRequest::decl(),
        wechat_core::media_types::MediaSendRequest::decl(),
        wechat_core::media_types::MediaImagePreview::decl(),
        wechat_core::media_types::MediaAsset::decl(),
        NativeMediaBatchRequest::decl(),
        NativeMediaBatchItem::decl(),
        NativeMediaBatch::decl(),
        wechat_core::media_types::MediaJob::decl(),
        wechat_store::OwnedMediaSource::decl(),
    ];
    let mut output=format!("// Generated from wechat-api/src/contracts.rs. Do not edit.\nexport const NATIVE_MAX_SOURCE_BYTES = {} as const;\nexport const NATIVE_MAX_ORDINARY_SOURCE_BYTES = {} as const;\nexport const NATIVE_MAX_VIDEO_SOURCE_BYTES = {} as const;\n",wechat_core::MAX_SOURCE_BYTES,wechat_core::MAX_ORDINARY_SOURCE_BYTES,wechat_core::MAX_VIDEO_SOURCE_BYTES);
    for declaration in declarations {
        output.push_str("export ");
        output.push_str(&declaration);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The generated TypeScript without doc comments and with whitespace
    /// collapsed, so shape checks do not depend on documentation.
    fn typescript_shapes() -> String {
        let mut text = typescript();
        while let Some(start) = text.find("/**") {
            let end = text[start..].find("*/").expect("closed doc comment") + start + 2;
            text.replace_range(start..end, "");
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    #[test]
    fn thread_mute_contract_preserves_unknown_and_boolean_preferences() {
        let mut thread = serde_json::json!({"id":"peer","accountId":"one","peerId":"peer","kind":"direct","displayName":null,"unreadCount":0,"readStateVersion":"v","history":{}});
        assert!(serde_json::from_value::<NativeThread>(thread.clone())
            .unwrap()
            .muted
            .is_none());
        for value in [Value::Null, Value::Bool(false), Value::Bool(true)] {
            thread["muted"] = value.clone();
            let parsed: NativeThread = serde_json::from_value(thread.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap()["muted"], value);
        }
        thread["muted"] = serde_json::json!("false");
        assert!(serde_json::from_value::<NativeThread>(thread).is_err());
        assert!(typescript().contains("muted?: boolean | null"));
    }
    #[test]
    fn thread_pin_contract_preserves_unknown_and_requires_a_boolean_desired_state() {
        let mut thread = serde_json::json!({"id":"peer","accountId":"one","peerId":"peer","kind":"direct","displayName":null,"unreadCount":0,"readStateVersion":"v","history":{}});
        assert!(serde_json::from_value::<NativeThread>(thread.clone())
            .unwrap()
            .pinned
            .is_none());
        for value in [Value::Null, Value::Bool(false), Value::Bool(true)] {
            thread["pinned"] = value.clone();
            let parsed: NativeThread = serde_json::from_value(thread.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap()["pinned"], value);
        }
        for value in [serde_json::json!("false"), serde_json::json!(1)] {
            thread["pinned"] = value;
            assert!(serde_json::from_value::<NativeThread>(thread.clone()).is_err());
        }
        for pinned in [false, true] {
            let input: NativeThreadPinRequest =
                serde_json::from_value(serde_json::json!({"pinned":pinned})).unwrap();
            assert_eq!(input.pinned, pinned);
        }
        for input in [
            serde_json::json!({}),
            serde_json::json!({"pinned":null}),
            serde_json::json!({"pinned":"false"}),
            serde_json::json!({"pinned":true,"toggle":true}),
        ] {
            assert!(serde_json::from_value::<NativeThreadPinRequest>(input).is_err());
        }
        let generated = typescript_shapes();
        assert!(generated.contains("pinned?: boolean | null"));
        assert!(generated.contains("NativeThreadPinRequest = { pinned: boolean, }"));
    }
    #[test]
    fn peer_classification_contract_is_additive_and_keeps_unknown_values() {
        let mut contact = serde_json::json!({"id":"peer","username":"peer","displayName":null,"nickname":null,"remark":null,"alias":null,"relation":"unknown"});
        assert!(serde_json::from_value::<NativeContact>(contact.clone())
            .unwrap()
            .classification
            .is_none());
        contact["classification"] = serde_json::json!({"category":"business","namespace":"wechat","businessType":0,"businessKind":"official_account","openIm":null});
        let parsed = serde_json::from_value::<NativeContact>(contact.clone()).unwrap();
        assert_eq!(parsed.classification.unwrap().business_type, Some(0));
        contact["classification"]["category"] = serde_json::json!("unknown");
        contact["classification"]["businessType"] = Value::Null;
        let parsed = serde_json::from_value::<NativeContact>(contact.clone()).unwrap();
        assert_eq!(
            parsed.classification.unwrap().category,
            wechat_core::PeerCategory::Unknown
        );
        contact["classification"]["businessType"] = serde_json::json!("0");
        assert!(serde_json::from_value::<NativeContact>(contact).is_err());
        assert!(typescript().contains("classification?: PeerClassification"));
    }
    #[test]
    fn wire_contract_rejects_numeric_remote_ids_and_bad_scope_shapes() {
        assert!(serde_json::from_value::<NativeMessage>(serde_json::json!({"id":"m","accountId":"a","threadId":"t","serverId":9007199254740992u64,"senderId":null,"type":1,"direction":null,"body":null,"createdAt":null,"mentions":{"state":"unknown","userIds":[],"self":null,"all":null,"source":null},"version":"1","media":[]})).is_err());
    }
    #[test]
    fn recall_notice_is_optional_nullable_and_keeps_text() {
        for value in [
            serde_json::json!({"observedAt":"2026-10-23T00:00:00Z"}),
            serde_json::json!({"observedAt":"2026-10-23T00:00:00Z","replacementText":null}),
            serde_json::json!({"observedAt":"2026-10-23T00:00:00Z","replacementText":"Server notice"}),
        ] {
            assert!(serde_json::from_value::<NativeRecallInfo>(value).is_ok());
        }
        assert!(serde_json::from_value::<NativeRecallInfo>(
            serde_json::json!({"observedAt":"2026-10-23T00:00:00Z","replacementText":42})
        )
        .is_err());
        let generated = typescript();
        assert!(generated.contains("replacementText?: string | null"));
        assert!(!generated.contains("originalSenderId"));
        assert!(!generated.contains("originalDirection"));
    }
    #[test]
    fn deleted_members_preserve_position_without_fabricated_resources() {
        let page = serde_json::json!({"items":[{"id":"gone","deleted":true,"version":"2"}],"nextCursor":null,"coverage":{},"sourceGeneration":"g"});
        assert!(checked::<NativePage<NativeMessage>>(page).is_ok());
        assert!(serde_json::from_value::<NativeTombstone>(
            serde_json::json!({"id":"x","deleted":false,"version":"1"})
        )
        .is_err());
    }
    #[test]
    fn quoted_sender_name_is_optional_and_preserved_in_contracts() {
        let mut value = serde_json::json!({"serverId":"123","type":1,"senderId":"123456@openim"});
        let without_name: NativeQuoteReference = serde_json::from_value(value.clone()).unwrap();
        assert!(serde_json::to_value(without_name)
            .unwrap()
            .get("senderName")
            .is_none());
        value["senderName"] = serde_json::json!("Example member");
        let with_name: NativeQuoteReference = serde_json::from_value(value).unwrap();
        assert_eq!(
            serde_json::to_value(with_name).unwrap()["senderName"],
            "Example member"
        );
        assert!(typescript().contains("senderName?: string"));
    }
    #[test]
    fn generated_contract_uses_lossless_strings_and_nullable_metadata() {
        let generated = typescript_shapes();
        assert!(generated.contains("NATIVE_MAX_SOURCE_BYTES = 536870912 as const"));
        assert!(generated.contains("NATIVE_MAX_ORDINARY_SOURCE_BYTES = 26214383 as const"));
        assert!(generated.contains("NATIVE_MAX_VIDEO_SOURCE_BYTES = 536870912 as const"));
        assert!(generated.contains("videoQuality?: VideoSendQuality"));
        assert!(generated.contains("serverId: string | null"));
        assert!(generated.contains("unreadCount: number | null"));
        assert!(generated.contains("type NativePage<T>"));
        assert!(generated.contains("quote?: QuoteSendReference"));
        assert!(
            generated.contains("type QuoteSendReference = { messageId: string, serverId: string,")
        );
    }
    #[test]
    fn text_mentions_contract_has_optional_fields_without_accepting_raw_source() {
        for mentions in [
            serde_json::json!({"userIds":["member_a"]}),
            serde_json::json!({"all":true}),
        ] {
            let body =
                serde_json::json!({"threadId":"room@chatroom","text":"Hello","mentions":mentions});
            let parsed: NativeTextSendRequest = serde_json::from_value(body.clone()).unwrap();
            parsed.mentions.as_ref().unwrap().validate().unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), body);
        }
        for invalid in [
            serde_json::json!({"userIds":"a"}),
            serde_json::json!({"all":"true"}),
            serde_json::json!({"userIds":["a"],"source":"xml"}),
        ] {
            assert!(serde_json::from_value::<NativeTextSendRequest>(
                serde_json::json!({"threadId":"room@chatroom","text":"Hello","mentions":invalid})
            )
            .is_err());
        }
        let generated = typescript();
        assert!(generated.contains("mentions?: TextMentions"));
        assert!(generated.contains("userIds?: Array<string>"));
        assert!(generated.contains("all?: boolean"));
    }
    #[test]
    fn text_send_contract_preserves_both_quote_ids_and_rejects_untrusted_content() {
        let request = serde_json::json!({"threadId":"peer","text":"Reply","quote":{"messageId":"opaque/source","serverId":"18446744073709551615"}});
        let parsed: NativeTextSendRequest = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);
        let plain = serde_json::json!({"threadId":"peer","text":"Reply"});
        let parsed: NativeTextSendRequest = serde_json::from_value(plain.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), plain);
        for quote in [
            serde_json::json!({"messageId":"source"}),
            serde_json::json!({"serverId":"42"}),
            serde_json::json!({"messageId":"source","serverId":9007199254740993u64}),
            serde_json::json!({"messageId":"source","serverId":"42","content":"caller text"}),
        ] {
            let mut invalid = request.clone();
            invalid["quote"] = quote;
            assert!(serde_json::from_value::<NativeTextSendRequest>(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod read_contract_tests {
    use super::*;
    #[test]
    fn location_is_additive_and_keeps_coordinate_system_unknown() {
        let mut value = serde_json::json!({"id":"pin","accountId":"a","threadId":"t","serverId":"42",
            "senderId":"u","type":48,"direction":"incoming","body":"[Location]",
            "createdAt":null,"mentions":{"state":"unknown","userIds":[],"self":null,"all":null,"source":null},"version":"v","media":[]});
        let old: NativeMessage = serde_json::from_value(value.clone()).unwrap();
        assert!(old.location.is_none());
        value["location"] = serde_json::json!({"latitude":12.5,"longitude":-45.25,"name":"Example park","address":null,"scale":-2,"mapType":"supplied","poiId":null,"coordinateSystem":"unknown"});
        let parsed: NativeMessage = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap()["location"],
            value["location"]
        );
        value["location"]["coordinateSystem"] = serde_json::json!("wgs84");
        assert!(serde_json::from_value::<NativeMessage>(value).is_err());
    }
    #[test]
    fn message_contract_keeps_embedded_quote_content_without_an_original_local_id() {
        let value = serde_json::json!({"id":"reply","accountId":"a","threadId":"t","serverId":"42",
            "senderId":"u","type":49,"appMessageType":57,"direction":"incoming","body":"Current comment",
            "createdAt":null,"mentions":{"state":"unknown","userIds":[],"self":null,"all":null,"source":null},"version":"v","media":[],
            "quote":{"serverId":"18446744073709551615","type":49,"state":"embedded","senderId":"other",
                "direction":"incoming","appMessageType":5,"embeddedText":"[Link] Article\nhttps://example.com/article","embeddedImage":false}});
        let parsed: NativeMessage = serde_json::from_value(value.clone()).unwrap();
        let result = serde_json::to_value(parsed).unwrap();
        assert_eq!(result["quote"], value["quote"]);
        assert!(result["quote"].get("id").is_none());
        let mut suppressed = value;
        suppressed["quote"] =
            serde_json::json!({"serverId":"18446744073709551615","type":49,"state":"recalled"});
        let parsed: NativeMessage = serde_json::from_value(suppressed).unwrap();
        let result = serde_json::to_value(parsed).unwrap();
        assert_eq!(result["body"], "Current comment");
        assert_eq!(result["quote"]["state"], "recalled");
        assert!(result["quote"]["embeddedText"].is_null());
    }
    #[test]
    fn thread_read_rejects_retired_message_selection_instead_of_broadening_it() {
        assert!(serde_json::from_value::<NativeReadRequest>(
            serde_json::json!({"expectedVersion":"v"})
        )
        .is_ok());
        for field in ["messageIds", "throughMessageId"] {
            let mut value = serde_json::json!({"expectedVersion":"v"});
            value[field] = serde_json::json!("retired-boundary");
            assert!(serde_json::from_value::<NativeReadRequest>(value).is_err());
        }
    }
}
