use crate::Core;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ts_rs::TS;
use wechat_profile::{PeerMetadata, ProfileReader};

/// What kind of peer this is. It is separate from the chat type and from whether the peer is in your contacts.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PeerCategory {
    /// No category is established. Do not assume a personal account or a friend.
    #[default]
    Unknown,
    /// A built-in peer such as File Transfer.
    System,
    /// A group chat.
    Group,
    /// An official or business account.
    Business,
    /// A WeCom contact.
    Openim,
    /// A WeCom customer-service identity.
    OpenimCustomerService,
}

/// The family of IDs a peer's ID belongs to, worked out from the ID itself. It does not grant any
/// extra actions.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PeerNamespace {
    /// An ordinary WeChat ID, including groups ending with `@chatroom`.
    #[default]
    Wechat,
    /// An ID containing `@openim`, a WeCom contact.
    Openim,
    /// An ID containing `@kefu.openim`, a WeCom customer-service identity.
    OpenimCustomerService,
    /// A group ID containing `@im.chatroom`.
    ImChatroom,
    /// An ID ending with `@qy_u` (a person) or `@qy_g` (a group).
    Qy,
    /// An ID containing `@weclaw`.
    Weclaw,
    /// Any other ID containing `@` that matches no known family.
    Other,
}

/// Kind of business account, worked out from the numeric `businessType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PeerBusinessKind {
    /// An official account (business type 0).
    OfficialAccount,
    /// A service account (business type 1).
    ServiceAccount,
    /// An enterprise account (business type 2).
    Enterprise,
    /// An enterprise child account (business type 3).
    EnterpriseChild,
    /// A photo account (business type 5).
    PhotoAccount,
}

/// A label in one language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerWording {
    /// The language of the text, as the number WeChat stores. Choose the language to show in your
    /// app.
    pub language_id: i32,
    /// The label text.
    pub text: String,
}

/// Reference to an account type for one language, from the saved WeCom application data. The label
/// of the account type is not included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerApplication {
    /// The language this entry applies to, as the number WeChat stores.
    pub language_id: i32,
    /// ID of the account type for this language. Null when none is saved.
    pub account_type_id: Option<String>,
    /// True when the profile also holds the label for this account type and language. The label
    /// itself is not returned.
    pub account_type_cached: bool,
}

/// Saved WeCom (OpenIM) details for a peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerOpenIm {
    /// ID of the WeCom application the peer belongs to. Null when none is saved.
    pub app_id: Option<String>,
    /// ID of the set of labels in `wordings`. Null when none is saved.
    pub wording_id: Option<String>,
    /// Localized labels for the peer, one per language.
    pub wordings: Vec<PeerWording>,
    /// Account type references, one per language.
    pub applications: Vec<PeerApplication>,
}

/// What kind of peer this is, worked out from the saved profile each time it is read.
/// Missing details do not mean the peer is an ordinary person or that you are not subscribed to it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct PeerClassification {
    pub category: PeerCategory,
    pub namespace: PeerNamespace,
    /// Numeric service type saved for a business account: 0 official account, 1 service account, 2
    /// enterprise, 3 enterprise child, 5 photo account. Null for other peers and when the saved
    /// values conflict.
    pub business_type: Option<i32>,
    pub business_kind: Option<PeerBusinessKind>,
    /// Saved WeCom details, present for `openim` and `openim_customer_service` peers when the
    /// profile has them. Null otherwise.
    pub open_im: Option<PeerOpenIm>,
}

fn system_peer(id: &str) -> bool {
    matches!(
        id,
        "filehelper"
            | "weixin"
            | "weibo"
            | "fmessage"
            | "floatbottle"
            | "medianote"
            | "qmessage"
            | "tmessage"
            | "masssendapp"
            | "newsapp"
            | "blogapp"
            | "qqmail"
            | "qqsync"
            | "lbsapp"
            | "shakeapp"
            | "brandsessionholder"
            | "brandservicesessionholder"
            | "appbrandcustomerservicemsg"
            | "service_officialaccounts"
            | "notifymessage"
            | "notification_messages"
            | "@placeholder_foldgroup"
            | "@opencustomerservicemsg"
            | "wa_app_kefu_message"
            | "wxzhifu"
            | "gh_22b87fa7cb3c"
            | "gh_43f2581f6fd6"
            | "gh_3dfda90e39d6"
            | "gh_25d9ac85a4bc"
            | "gh_579db1f2cf89"
    )
}

pub(crate) fn peer_classification(id: &str, metadata: Option<&PeerMetadata>) -> PeerClassification {
    let namespace = if id.contains("@kefu.openim") {
        PeerNamespace::OpenimCustomerService
    } else if id.contains("@openim") {
        PeerNamespace::Openim
    } else if id.contains("@im.chatroom") {
        PeerNamespace::ImChatroom
    } else if id.ends_with("@qy_u") || id.ends_with("@qy_g") {
        PeerNamespace::Qy
    } else if id.contains("@weclaw") {
        PeerNamespace::Weclaw
    } else if id.contains('@') && !id.contains("@chatroom") && !id.ends_with("@stranger") {
        PeerNamespace::Other
    } else {
        PeerNamespace::Wechat
    };
    let mut classification = PeerClassification {
        namespace,
        ..Default::default()
    };
    if system_peer(id) {
        classification.category = PeerCategory::System;
    } else if id.contains("@chatroom")
        || namespace == PeerNamespace::ImChatroom
        || id.ends_with("@qy_g")
    {
        classification.category = PeerCategory::Group;
    } else if matches!(
        namespace,
        PeerNamespace::Openim | PeerNamespace::OpenimCustomerService
    ) {
        classification.category = if namespace == PeerNamespace::Openim {
            PeerCategory::Openim
        } else {
            PeerCategory::OpenimCustomerService
        };
        classification.open_im = metadata.and_then(|metadata| {
            let openim = metadata.openim.as_ref()?;
            Some(PeerOpenIm {
                app_id: openim.app_id.clone(),
                wording_id: openim.wording_id.clone(),
                wordings: openim
                    .wordings
                    .iter()
                    .filter_map(|wording| {
                        Some(PeerWording {
                            language_id: i32::try_from(wording.language_id).ok()?,
                            text: wording.wording.clone(),
                        })
                    })
                    .collect(),
                applications: openim
                    .applications
                    .iter()
                    .filter_map(|application| {
                        Some(PeerApplication {
                            language_id: i32::try_from(application.language_id).ok()?,
                            account_type_id: application.account_type_id.clone(),
                            account_type_cached: application.account_type_cached,
                        })
                    })
                    .collect(),
            })
        });
    } else if namespace == PeerNamespace::Wechat && !id.contains('@') {
        if let Some(metadata) = metadata {
            let admitted = matches!(metadata.local_type, Some(0..=4))
                && metadata
                    .verify_flag
                    .and_then(|flag| u32::try_from(flag).ok())
                    .is_some_and(|flag| flag & 8 != 0);
            if admitted && (metadata.business.is_some() || metadata.contact_service_type.is_some())
            {
                classification.category = PeerCategory::Business;
                let table_type = metadata
                    .business
                    .as_ref()
                    .and_then(|business| business.service_type)
                    .and_then(|value| i32::try_from(value).ok());
                classification.business_type = match (metadata.contact_service_type, table_type) {
                    (Some(contact), Some(table)) if contact != table => None,
                    (Some(contact), _) => Some(contact),
                    (_, table) => table,
                };
                classification.business_kind = match classification.business_type {
                    Some(0) => Some(PeerBusinessKind::OfficialAccount),
                    Some(1) => Some(PeerBusinessKind::ServiceAccount),
                    Some(2) => Some(PeerBusinessKind::Enterprise),
                    Some(3) => Some(PeerBusinessKind::EnterpriseChild),
                    Some(5) => Some(PeerBusinessKind::PhotoAccount),
                    _ => None,
                };
            }
        }
    }
    classification
}

impl Core {
    pub(crate) fn classify_peers(&self, rows: &mut [Value], reader: &ProfileReader) -> Result<()> {
        let ids: Vec<_> = rows
            .iter()
            .filter(|row| row["deleted"] != true)
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect();
        // Thread history can remain readable while contact metadata is unavailable.
        // Preserve those resources and report unknown category/metadata.
        let metadata = reader.peer_metadata(&ids).unwrap_or_default();
        for row in rows.iter_mut().filter(|row| row["deleted"] != true) {
            let Some(id) = row["id"].as_str() else {
                continue;
            };
            row["classification"] = json!(peer_classification(id, metadata.get(id)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> PeerMetadata {
        PeerMetadata {
            local_type: Some(1),
            flag: Some(1),
            delete_flag: Some(0),
            verify_flag: Some(8),
            contact_service_type: None,
            business: None,
            openim: None,
        }
    }

    #[test]
    fn ordinary_contact_and_missing_metadata_do_not_claim_a_person_or_friend() {
        let metadata = metadata();
        assert_eq!(
            peer_classification("person", Some(&metadata)).category,
            PeerCategory::Unknown
        );
        assert_eq!(
            peer_classification("gh_synthetic", None).category,
            PeerCategory::Unknown
        );
        assert_eq!(
            peer_classification("weixin_synthetic", None).category,
            PeerCategory::Unknown
        );
    }

    #[test]
    fn builtins_precede_business_metadata_and_zero_is_not_an_absent_subtype() {
        let mut metadata = metadata();
        metadata.contact_service_type = Some(0);
        assert_eq!(
            peer_classification("weixin", Some(&metadata)).category,
            PeerCategory::System
        );
        let business = peer_classification("gh_synthetic", Some(&metadata));
        assert_eq!(business.category, PeerCategory::Business);
        assert_eq!(business.business_type, Some(0));
        metadata.local_type = Some(8);
        assert_eq!(
            peer_classification("gh_synthetic", Some(&metadata)).category,
            PeerCategory::Unknown
        );
        metadata.local_type = Some(1);
        metadata.verify_flag = None;
        assert_eq!(
            peer_classification("gh_synthetic", Some(&metadata)).category,
            PeerCategory::Unknown
        );
    }

    #[test]
    fn namespace_predicates_preserve_separate_routes() {
        for (id, category, namespace) in [
            ("room@chatroom", PeerCategory::Group, PeerNamespace::Wechat),
            (
                "room@im.chatroom",
                PeerCategory::Group,
                PeerNamespace::ImChatroom,
            ),
            ("room@qy_g", PeerCategory::Group, PeerNamespace::Qy),
            ("person@openim", PeerCategory::Openim, PeerNamespace::Openim),
            (
                "service@kefu.openim",
                PeerCategory::OpenimCustomerService,
                PeerNamespace::OpenimCustomerService,
            ),
            ("person@qy_u", PeerCategory::Unknown, PeerNamespace::Qy),
            ("peer@weclaw", PeerCategory::Unknown, PeerNamespace::Weclaw),
        ] {
            let classification = peer_classification(id, None);
            assert_eq!(
                (classification.category, classification.namespace),
                (category, namespace)
            );
        }
    }

    #[test]
    fn conflicting_canonical_business_types_are_reported_as_unknown_subtype() {
        let mut metadata = metadata();
        metadata.contact_service_type = Some(0);
        metadata.business = Some(wechat_profile::PeerBusinessMetadata {
            service_type: Some(1),
            accept_type: Some(0),
            child_type: Some(0),
        });
        let result = peer_classification("gh_synthetic", Some(&metadata));
        assert_eq!(result.category, PeerCategory::Business);
        assert_eq!(result.business_type, None);
    }

    #[test]
    fn openim_labels_preserve_language_and_account_type_without_inventing_a_company() {
        let mut metadata = metadata();
        metadata.openim = Some(wechat_profile::PeerOpenImMetadata {
            app_id: Some("application".into()),
            wording_id: Some("affiliation".into()),
            wordings: vec![
                wechat_profile::PeerOpenImWording {
                    language_id: 0,
                    wording: "Example".into(),
                },
                wechat_profile::PeerOpenImWording {
                    language_id: 1,
                    wording: "示例".into(),
                },
            ],
            applications: vec![wechat_profile::PeerOpenImApplication {
                language_id: 0,
                account_type_id: Some("account-type".into()),
                account_type_cached: true,
            }],
        });
        let result = peer_classification("person@openim", Some(&metadata));
        assert_eq!(result.category, PeerCategory::Openim);
        let details = result.open_im.unwrap();
        assert_eq!(details.wordings.len(), 2);
        assert_eq!(
            details.applications[0].account_type_id.as_deref(),
            Some("account-type")
        );
        assert_eq!(details.wordings[1].text, "示例");
    }
}
