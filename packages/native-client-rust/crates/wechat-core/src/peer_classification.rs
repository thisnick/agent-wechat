use crate::Core;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ts_rs::TS;
use wechat_profile::{PeerMetadata, ProfileReader};

/// Entity category is independent of conversation shape and contact relationship.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PeerCategory {
    #[default]
    Unknown,
    System,
    Group,
    Business,
    Openim,
    OpenimCustomerService,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PeerNamespace {
    #[default]
    Wechat,
    Openim,
    OpenimCustomerService,
    ImChatroom,
    Qy,
    Weclaw,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PeerBusinessKind {
    OfficialAccount,
    ServiceAccount,
    Enterprise,
    EnterpriseChild,
    PhotoAccount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerWording {
    pub language_id: i32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerApplication {
    pub language_id: i32,
    pub account_type_id: Option<String>,
    pub account_type_cached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerOpenIm {
    pub app_id: Option<String>,
    pub wording_id: Option<String>,
    pub wordings: Vec<PeerWording>,
    pub applications: Vec<PeerApplication>,
}

/// Derived on each read from the same official-profile view as the resource.
/// Null metadata does not establish an ordinary person or an unsubscribed peer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerClassification {
    pub category: PeerCategory,
    pub namespace: PeerNamespace,
    pub business_type: Option<i32>,
    pub business_kind: Option<PeerBusinessKind>,
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
