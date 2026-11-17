//! Contact relationship privacy settings and correlated modification replies.
use super::*;

const PRIVACY_PATH: &str = "/cgi-bin/micromsg-bin/batchmodcontacttype";
const CHATS_ONLY: u32 = 0x800000;
const HIDE_MY_POSTS: u32 = 0x100;
const HIDE_THEIR_POSTS: u32 = 0x10000;

/// Complete relationship privacy selections; these do not grant application permissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactPrivacySettings {
    /// Restrict the relationship to chats.
    pub chats_only: bool,
    /// Hide this account's posts from the contact.
    pub hide_my_posts: bool,
    /// Hide the contact's posts from this account.
    pub hide_their_posts: bool,
}
impl ContactPrivacySettings {
    /// Compact privacy value used when sending or accepting verification.
    pub const fn verify_flags(&self) -> u32 {
        ((self.chats_only as u32) * 8)
            | self.hide_my_posts as u32
            | ((self.hide_their_posts as u32) * 2)
    }
    /// Selected privacy bits in the canonical contact flag representation.
    pub const fn contact_flags(&self) -> u32 {
        ((self.chats_only as u32) * CHATS_ONLY)
            | ((self.hide_my_posts as u32) * HIDE_MY_POSTS)
            | ((self.hide_their_posts as u32) * HIDE_THEIR_POSTS)
    }
    /// Read privacy selections from contact flags while ignoring unrelated bits.
    pub const fn from_contact_flags(flags: u32) -> Self {
        Self {
            chats_only: flags & CHATS_ONLY != 0,
            hide_my_posts: flags & HIDE_MY_POSTS != 0,
            hide_their_posts: flags & HIDE_THEIR_POSTS != 0,
        }
    }
}

/// Authenticated privacy results, correlated with each submitted change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactPrivacyReply {
    /// True only when the aggregate and every requested item succeeded.
    pub accepted: bool,
    /// Aggregate service result, preserved separately from item results.
    pub service_code: i32,
    /// Per-item service results in request order; empty for an aggregate-only rejection.
    pub item_codes: Vec<i32>,
    /// Whether some requested items succeeded and others failed.
    pub partial: bool,
}

#[derive(PartialEq, Eq)]
struct PrivacyItem {
    username: String,
    mask: u32,
    action: u32,
}
fn privacy_identity(username: &str) -> Result<()> {
    ordinary_identity(username)?;
    ensure!(!username.contains('@'), "ordinary_contact_required");
    Ok(())
}
/// Prepare changed privacy masks only; an unchanged selection is not a request.
/// Each mutation is submitted once, with its original request and reply retained by the caller.
pub fn contact_privacy_request(
    state: &AccountState,
    username: &str,
    before: &ContactPrivacySettings,
    after: &ContactPrivacySettings,
) -> Result<ContactRpc> {
    privacy_identity(username)?;
    let mut items = Vec::new();
    for (mask, old, new) in [
        (CHATS_ONLY, before.chats_only, after.chats_only),
        (HIDE_MY_POSTS, before.hide_my_posts, after.hide_my_posts),
        (
            HIDE_THEIR_POSTS,
            before.hide_their_posts,
            after.hide_their_posts,
        ),
    ] {
        if old != new {
            items.push(p::message(&[
                p::bytes(1, username.as_bytes()),
                p::uint(2, u64::from(mask)),
                p::uint(3, if new { 1 } else { 2 }),
            ])?);
        }
    }
    ensure!(!items.is_empty(), "unchanged_contact_privacy");
    let mut fields = vec![
        p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
        p::uint(2, items.len() as u64),
    ];
    fields.extend(items.iter().map(|item| p::bytes(3, item)));
    Ok(ContactRpc {
        cgi: 3990,
        path: PRIVACY_PATH.into(),
        body: p::message(&fields)?,
    })
}
fn privacy_item(raw: &[u8]) -> Result<PrivacyItem> {
    let fields = p::decode(raw)?;
    let username = std::str::from_utf8(p::data(&fields, 1)?)?.to_owned();
    privacy_identity(&username)?;
    let mask = u32::try_from(p::scalar(&fields, 2)?)?;
    let action = u32::try_from(p::scalar(&fields, 3)?)?;
    ensure!(
        matches!(mask, CHATS_ONLY | HIDE_MY_POSTS | HIDE_THEIR_POSTS) && matches!(action, 1 | 2),
        "invalid_contact_privacy_item"
    );
    Ok(PrivacyItem {
        username,
        mask,
        action,
    })
}
fn privacy_request_items(request: &ContactRpc) -> Result<Vec<PrivacyItem>> {
    ensure!(
        request.cgi == 3990 && request.path == PRIVACY_PATH,
        "invalid_contact_rpc"
    );
    let fields = p::decode(&request.body)?;
    let count = usize::try_from(p::scalar(&fields, 2)?)?;
    ensure!((1..=3).contains(&count), "invalid_contact_privacy_count");
    let mut items: Vec<PrivacyItem> = Vec::new();
    for field in fields.iter().filter(|field| field.number == 3) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("invalid_contact_privacy_item")
        };
        let item = privacy_item(raw)?;
        ensure!(
            items.len() < count
                && items.iter().all(|existing| {
                    existing.username == item.username && existing.mask != item.mask
                }),
            "invalid_contact_privacy_item"
        );
        items.push(item);
    }
    ensure!(items.len() == count, "invalid_contact_privacy_count");
    Ok(items)
}
fn privacy_code(value: u64) -> Result<i32> {
    let code = value as u32 as i32;
    ensure!(
        value <= u64::from(u32::MAX) || value == code as i64 as u64,
        "invalid_contact_privacy_result"
    );
    Ok(code)
}
/// Validate aggregate and per-item results against the original request's echoed changes.
/// Service rejections are returned as outcomes; malformed or mismatched replies are errors.
/// This decoder never resubmits a mutation or treats a partial result as retry permission.
pub fn decode_contact_privacy_reply(
    request: &ContactRpc,
    reply: &[u8],
) -> Result<ContactPrivacyReply> {
    let expected = privacy_request_items(request)?;
    let fields = p::decode(reply)?;
    let base = p::decode(p::data(&fields, 1)?)?;
    let service_code = privacy_code(p::scalar(&base, 1)?)?;
    let declared_count = match p::optional(&fields, 2)? {
        Some(p::Value::Varint(value)) => usize::try_from(*value)?,
        None if service_code != 0 => 0,
        _ => bail!("invalid_contact_privacy_count"),
    };
    ensure!(
        declared_count <= expected.len(),
        "invalid_contact_privacy_count"
    );
    p::optional_data(&fields, 4)?;
    let mut results = vec![None; expected.len()];
    let mut received_count = 0;
    for field in fields.iter().filter(|field| field.number == 3) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("invalid_contact_privacy_result")
        };
        let item = p::decode(raw)?;
        let code = privacy_code(p::scalar(&item, 1)?)?;
        let echo = privacy_item(p::data(&item, 2)?)?;
        if let Some(value) = p::optional(&item, 3)? {
            ensure!(
                matches!(value, p::Value::Varint(_)),
                "invalid_contact_privacy_result"
            );
        }
        let index = expected
            .iter()
            .position(|wanted| *wanted == echo)
            .ok_or_else(|| anyhow::anyhow!("mismatched_contact_privacy_result"))?;
        ensure!(results[index].is_none(), "duplicate_contact_privacy_result");
        results[index] = Some(code);
        received_count += 1;
    }
    ensure!(
        received_count == declared_count,
        "invalid_contact_privacy_count"
    );
    if service_code != 0 && received_count == 0 {
        return Ok(ContactPrivacyReply {
            accepted: false,
            service_code,
            item_codes: vec![],
            partial: false,
        });
    }
    ensure!(
        received_count == expected.len(),
        "incomplete_contact_privacy_result"
    );
    let item_codes: Vec<i32> = results
        .into_iter()
        .map(|code| code.ok_or_else(|| anyhow::anyhow!("incomplete_contact_privacy_result")))
        .collect::<Result<_>>()?;
    let succeeded = item_codes.contains(&0);
    let failed = item_codes.iter().any(|code| *code != 0);
    Ok(ContactPrivacyReply {
        accepted: service_code == 0 && !failed,
        service_code,
        item_codes,
        partial: succeeded && failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![1; 16],
                device_type: "synthetic".into(),
                client_version: -227271398,
            },
            uin: 123,
            own_username: "self".into(),
            cookie: vec![],
            session_key: vec![6; 16],
            request_key: vec![7; 24],
            response_key: vec![8; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: AuthSnapshot::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }
    fn changed_request() -> ContactRpc {
        contact_privacy_request(
            &state(),
            "peer",
            &ContactPrivacySettings {
                hide_my_posts: true,
                ..Default::default()
            },
            &ContactPrivacySettings {
                chats_only: true,
                hide_my_posts: false,
                hide_their_posts: true,
            },
        )
        .unwrap()
    }
    // Independent synthetic protobuf fixtures: field1 signed result, field2 echoed request item.
    const CHATS_OK: &str = "1a110800120d0a047065657210808080041801";
    const HIDE_MY_FAILED: &str = "1a1808f9ffffffffffffffff01120b0a04706565721080021802";
    const HIDE_THEIR_OK: &str = "1a100800120c0a0470656572108080041801";
    fn partial_reply() -> Vec<u8> {
        hex::decode(format!(
            "0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{HIDE_THEIR_OK}"
        ))
        .unwrap()
    }
    #[test]
    fn settings_preserve_all_selection_combinations_and_ignore_unrelated_contact_bits() {
        for (selection, packed, flags) in [
            (0, 0, 0),
            (1, 8, 0x800000),
            (2, 1, 0x100),
            (3, 9, 0x800100),
            (4, 2, 0x10000),
            (5, 10, 0x810000),
            (6, 3, 0x10100),
            (7, 11, 0x810100),
        ] {
            let settings = ContactPrivacySettings {
                chats_only: selection & 1 != 0,
                hide_my_posts: selection & 2 != 0,
                hide_their_posts: selection & 4 != 0,
            };
            assert_eq!(settings.verify_flags(), packed);
            assert_eq!(settings.contact_flags(), flags);
            assert_eq!(
                ContactPrivacySettings::from_contact_flags(flags | 0x41),
                settings
            );
        }
        assert!(serde_json::from_str::<ContactPrivacySettings>(
            r#"{"chatsOnly":false,"hideMyPosts":false,"hideTheirPosts":false,"extra":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ContactPrivacySettings>(r#"{"chatsOnly":false}"#).is_err());
        assert_eq!(
            serde_json::to_value(ContactPrivacySettings::default()).unwrap(),
            serde_json::json!({"chatsOnly":false,"hideMyPosts":false,"hideTheirPosts":false})
        );
    }
    #[test]
    fn send_and_accept_include_compact_privacy_in_their_independent_user_shapes() {
        let target = ContactRequestTarget {
            username: "peer".into(),
            ticket: "t".into(),
            scene: 3,
        };
        let privacy = Some(ContactPrivacySettings {
            chats_only: true,
            hide_my_posts: true,
            hide_their_posts: true,
        });
        let send = send_contact_request_rpc(
            &state(),
            &SendContactRequest {
                target: target.clone(),
                message: "hello".into(),
                privacy,
            },
        )
        .unwrap();
        let accept = accept_contact_request_rpc(
            &state(),
            &AcceptContactRequest {
                target: target.clone(),
                privacy,
            },
        )
        .unwrap();
        for (request, expected) in [
            (send, b"\x0a\x04peer\x1a\x01t\x20\x0b".as_slice()),
            (accept, b"\x0a\x04peer\x12\x01t\x20\x0b".as_slice()),
        ] {
            assert_eq!(
                p::data(&p::decode(&request.body).unwrap(), 4).unwrap(),
                expected
            );
        }
        for privacy in [None, Some(ContactPrivacySettings::default())] {
            let rpc = accept_contact_request_rpc(
                &state(),
                &AcceptContactRequest {
                    target: target.clone(),
                    privacy,
                },
            )
            .unwrap();
            assert_eq!(
                p::data(&p::decode(&rpc.body).unwrap(), 4).unwrap(),
                b"\x0a\x04peer\x12\x01t"
            );
        }
        let legacy: AcceptContactRequest = serde_json::from_value(
            serde_json::json!({"target":{"username":"peer","ticket":"t","scene":3}}),
        )
        .unwrap();
        assert!(legacy.privacy.is_none());
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("privacy")
            .is_none());
    }
    #[test]
    fn changed_masks_have_independent_exact_item_bytes_and_noop_is_rejected() {
        let request = changed_request();
        assert_eq!(request.cgi, 3990);
        assert_eq!(request.path, PRIVACY_PATH);
        let fields = p::decode(&request.body).unwrap();
        assert_eq!(p::scalar(&fields, 2).unwrap(), 3);
        let items: Vec<_> = fields
            .iter()
            .filter(|field| field.number == 3)
            .map(|field| match &field.value {
                p::Value::Bytes(raw) => raw.as_slice(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(
            items,
            [
                b"\x0a\x04peer\x10\x80\x80\x80\x04\x18\x01".as_slice(),
                b"\x0a\x04peer\x10\x80\x02\x18\x02".as_slice(),
                b"\x0a\x04peer\x10\x80\x80\x04\x18\x01".as_slice()
            ]
        );
        let initial = ContactPrivacySettings::default();
        assert!(contact_privacy_request(&state(), "peer", &initial, &initial).is_err());
        let single = contact_privacy_request(
            &state(),
            "peer",
            &initial,
            &ContactPrivacySettings {
                hide_my_posts: true,
                ..initial
            },
        )
        .unwrap();
        let single_fields = p::decode(&single.body).unwrap();
        assert_eq!(p::scalar(&single_fields, 2).unwrap(), 1);
        assert_eq!(
            single_fields
                .iter()
                .filter(|field| field.number == 3)
                .count(),
            1
        );
        assert!(contact_privacy_request(
            &state(),
            "group@chatroom",
            &initial,
            &ContactPrivacySettings {
                chats_only: true,
                ..initial
            }
        )
        .is_err());
    }
    #[test]
    fn reply_preserves_partial_results_and_correlates_reordered_echoes() {
        let request = changed_request();
        let result = decode_contact_privacy_reply(&request, &partial_reply()).unwrap();
        assert_eq!(
            result,
            ContactPrivacyReply {
                accepted: false,
                service_code: 0,
                item_codes: vec![0, -7, 0],
                partial: true
            }
        );
        let reordered = hex::decode(format!(
            "0a0208001003{HIDE_THEIR_OK}{CHATS_OK}{HIDE_MY_FAILED}"
        ))
        .unwrap();
        assert_eq!(
            decode_contact_privacy_reply(&request, &reordered).unwrap(),
            result
        );
        let rejected = hex::decode("0a0b08fdffffffffffffffff01").unwrap();
        assert_eq!(
            decode_contact_privacy_reply(&request, &rejected).unwrap(),
            ContactPrivacyReply {
                accepted: false,
                service_code: -3,
                item_codes: vec![],
                partial: false
            }
        );
    }
    #[test]
    fn reply_requires_exact_result_count_unique_echo_and_matching_action_peer_and_mask() {
        let request = changed_request();
        for raw in [
            format!("0a0208001003{CHATS_OK}{CHATS_OK}{HIDE_THEIR_OK}"),
            format!("0a0208001002{CHATS_OK}{HIDE_MY_FAILED}{HIDE_THEIR_OK}"),
            format!("0a0208001002{CHATS_OK}{HIDE_MY_FAILED}"),
            format!(
                "0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{}",
                HIDE_THEIR_OK.replace("1801", "1802")
            ),
            format!(
                "0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{}",
                HIDE_THEIR_OK.replace("70656572", "666f7265")
            ),
            format!(
                "0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{}",
                HIDE_THEIR_OK.replace("10808004", "10808008")
            ),
            format!("0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{HIDE_THEIR_OK}1003"),
            format!("0a0208001003{CHATS_OK}{HIDE_MY_FAILED}{HIDE_THEIR_OK}2201"),
        ] {
            assert!(decode_contact_privacy_reply(&request, &hex::decode(raw).unwrap()).is_err());
        }
        let mut wrong = request.clone();
        wrong.cgi = 137;
        assert!(decode_contact_privacy_reply(&wrong, &partial_reply()).is_err());
    }
    #[test]
    fn one_successful_item_is_accepted_and_aggregate_rejection_stays_separate() {
        let initial = ContactPrivacySettings::default();
        let request = contact_privacy_request(
            &state(),
            "peer",
            &initial,
            &ContactPrivacySettings {
                chats_only: true,
                ..initial
            },
        )
        .unwrap();
        let success = hex::decode(format!("0a0208001001{CHATS_OK}")).unwrap();
        assert_eq!(
            decode_contact_privacy_reply(&request, &success).unwrap(),
            ContactPrivacyReply {
                accepted: true,
                service_code: 0,
                item_codes: vec![0],
                partial: false
            }
        );
        let rejected = hex::decode(format!("0a0b08fdffffffffffffffff011001{CHATS_OK}")).unwrap();
        let result = decode_contact_privacy_reply(&request, &rejected).unwrap();
        assert!(!result.accepted);
        assert_eq!(result.service_code, -3);
        assert_eq!(result.item_codes, [0]);
        assert!(!result.partial);
    }
}
