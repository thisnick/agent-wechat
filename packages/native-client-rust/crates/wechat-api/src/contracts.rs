//! Canonical wire contracts, consumed by response validation and SDK generation.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeMentions {
    pub state: String,
    pub user_ids: Vec<String>,
    #[serde(rename = "self")]
    pub self_mention: Option<bool>,
    pub all: Option<bool>,
    pub source: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeMessage {
    pub id: String,
    pub account_id: String,
    pub thread_id: String,
    pub server_id: Option<String>,
    pub sender_id: Option<String>,
    #[serde(rename = "type")]
    pub message_type: i32,
    #[serde(default)]
    #[ts(optional)]
    pub app_message_type: Option<i32>,
    #[serde(default)]
    #[ts(optional)]
    pub quote: Option<NativeQuoteReference>,
    #[serde(default)]
    #[ts(optional)]
    pub link: Option<wechat_core::LinkCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recall: Option<NativeRecallInfo>,
    pub direction: Option<String>,
    pub body: Option<String>,
    pub created_at: Option<String>,
    pub mentions: NativeMentions,
    pub version: String,
    #[ts(type = "unknown[]")]
    pub media: Vec<Value>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeRecallInfo {
    pub observed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recalled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub replacement_text: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeQuoteReference {
    pub server_id: String,
    #[serde(rename = "type")]
    pub message_type: i32,
    #[serde(default)]
    #[ts(optional)]
    pub state: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub sender_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub direction: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub app_message_type: Option<i32>,
    #[serde(default)]
    #[ts(optional)]
    pub embedded_text: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub embedded_image: Option<bool>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeThread {
    pub id: String,
    pub account_id: String,
    pub peer_id: String,
    pub kind: String,
    pub display_name: Option<String>,
    #[ts(type = "number | null")]
    pub unread_count: Option<i64>,
    pub read_state_version: String,
    // Absent when local message shards are unavailable; null for empty history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_message: Option<NativeThreadMessage>,
    #[serde(default)]
    #[ts(optional)]
    pub avatar_members: Option<Vec<NativeThreadAvatarMember>>,
    #[ts(type = "unknown")]
    pub history: Value,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeThreadMessage {
    pub id: String,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub direction: Option<String>,
    #[serde(rename = "type")]
    pub message_type: i32,
    pub text: String,
    pub created_at: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeThreadAvatarMember {
    pub id: String,
    pub display_name: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeContact {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub nickname: Option<String>,
    pub remark: Option<String>,
    pub alias: Option<String>,
    pub relation: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroup {
    pub id: String,
    pub room_id: String,
    pub display_name: Option<String>,
    pub owner_id: Option<String>,
    pub announcement: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remark: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub muted: Option<bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub minimized: Option<bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub is_in_group: Option<bool>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub my_display_name: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroupMember {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub room_display_name: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeGroupMemberLookup {
    pub group_id: String,
    pub username: String,
    pub state: String,
    pub member: Option<NativeGroupMember>,
    pub coverage: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupCreateRequest {
    pub members: Vec<String>,
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupNameRequest {
    pub name: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupMembersRequest {
    pub members: Vec<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGroupMyNameRequest {
    pub display_name: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeThreadNotificationsRequest {
    pub muted: bool,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeTombstone {
    pub id: String,
    #[serde(deserialize_with = "true_only")]
    #[ts(type = "true")]
    pub deleted: bool,
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
#[derive(Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum NativePageItem<T> {
    Resource(T),
    Tombstone(NativeTombstone),
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativePage<T> {
    pub items: Vec<NativePageItem<T>>,
    pub next_cursor: Option<String>,
    #[ts(type = "unknown")]
    pub coverage: Value,
    pub source_generation: String,
    #[serde(default)]
    #[ts(optional)]
    pub as_of_event_cursor: Option<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    pub id: String,
    pub account_id: String,
    pub kind: String,
    pub state: String,
    #[ts(type = "unknown")]
    pub result: Value,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeSendIntent {
    pub id: String,
    pub account_id: String,
    pub destination: String,
    pub state: String,
    pub client_id: Option<String>,
    pub server_id: Option<String>,
    pub error_code: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub local_message_id: Option<String>,
    pub created_at: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeEvent {
    pub cursor: String,
    pub kind: String,
    #[ts(type = "Record<string, unknown>")]
    pub data: Value,
    pub created_at: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeIdentity {
    pub product: String,
    pub backend: String,
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    #[ts(optional)]
    pub target: Option<String>,
    pub capabilities: Vec<String>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeStatus {
    pub server_id: String,
    pub data_dir: String,
    pub pid: u32,
    pub ownership_generation: String,
    pub version: NativeIdentity,
    pub state: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTextSendRequest {
    pub thread_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quote: Option<wechat_core::QuoteSendReference>,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRecallRequest {
    // Exact expected remote identity; an opaque source ID can be reused locally.
    pub server_id: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeReadRequest {
    pub expected_version: String,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeForgetRequest {
    pub confirm: bool,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMediaWritebackRequest {
    #[ts(type = "number")]
    pub limit: usize,
}

/// Validate the public response before serializing it, so generated clients and
/// Value-producing profile adapters cannot silently develop incompatible shapes.
pub fn checked<T: serde::de::DeserializeOwned>(value: Value) -> anyhow::Result<Value> {
    let _: T = serde_json::from_value(value.clone())?;
    Ok(value)
}

#[derive(Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMediaJobRetryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reuse_completed_upload: Option<bool>,
}

#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAccountRemovalRequest {
    pub confirm_account_id: String,
}
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeAccountRemoval {
    pub account_id: String,
    pub removed: bool,
    pub files_retained: bool,
}

pub fn typescript() -> String {
    let declarations = [
        NativeMentions::decl(),
        NativeQuoteReference::decl(),
        wechat_core::LinkCard::decl(),
        NativeRecallInfo::decl(),
        NativeMessage::decl(),
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
        NativeTombstone::decl(),
        NativePageItem::<String>::decl(),
        NativePage::<String>::decl(),
        NativeOperation::decl(),
        NativeSendIntent::decl(),
        NativeEvent::decl(),
        NativeIdentity::decl(),
        NativeStatus::decl(),
        NativeAccountRemovalRequest::decl(),
        NativeAccountRemoval::decl(),
        wechat_core::QuoteSendReference::decl(),
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
    fn generated_contract_uses_lossless_strings_and_nullable_metadata() {
        let generated = typescript();
        assert!(generated.contains("NATIVE_MAX_SOURCE_BYTES = 536870912 as const"));
        assert!(generated.contains("NATIVE_MAX_ORDINARY_SOURCE_BYTES = 26214383 as const"));
        assert!(generated.contains("NATIVE_MAX_VIDEO_SOURCE_BYTES = 52428800 as const"));
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
