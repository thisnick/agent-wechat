//! Contact discovery and verification. Each mutation submits once. Applications
//! retain authenticated replies before decoding and own official-profile writes.
use super::*;

/// Resolved verification material. Contains account-specific secrets; do not log
/// or serialize it into a public API response.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRequestTarget {
    /// Exact canonical per-peer identity.
    pub username: String,
    /// Private verification ticket; may be empty in a discovery match that cannot start verification. Never expose it in public responses.
    pub ticket: String,
    /// Verification source value accompanying the target.
    pub scene: u32,
}
/// A discovery match. Display text is the submitted query, not an inferred server nickname.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactSearchResult {
    /// Original discovery query.
    pub query: String,
    /// Exact resolved verification target and private ticket.
    pub target: ContactRequestTarget,
}
/// Outgoing request input, bound to the actual discovery result.
#[derive(Clone, Serialize, Deserialize)]
pub struct SendContactRequest {
    /// Exact resolved verification target and private ticket.
    pub target: ContactRequestTarget,
    /// Verification greeting or history entry for this operation.
    pub message: String,
    /// Optional privacy selections applied with this verification request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<ContactPrivacySettings>,
}
/// Acceptance input, bound to the latest received request.
#[derive(Clone, Serialize, Deserialize)]
pub struct AcceptContactRequest {
    /// Exact resolved verification target and private ticket.
    pub target: ContactRequestTarget,
    /// Optional privacy selections applied while accepting this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<ContactPrivacySettings>,
}
/// Authenticated verification acknowledgement; does not imply profile writeback.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactRequestReply {
    /// Authenticated service response code.
    pub service_code: i32,
}
/// A prepared contact operation; raw bytes contain tickets and require protected storage.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRpc {
    /// Supported contact service command number.
    pub cgi: u16,
    /// Exact supported service request path.
    pub path: String,
    /// Serialized request bytes containing private account material.
    pub body: Vec<u8>,
}
fn bounded(value: &str, max: usize, empty: bool) -> Result<()> {
    ensure!(
        (empty || !value.trim().is_empty())
            && value.len() <= max
            && !value.chars().any(char::is_control),
        "invalid_contact_input"
    );
    Ok(())
}
/// Build the supported direct discovery request with its authenticated base.
pub fn contact_search_request(state: &AccountState, query: &str) -> Result<ContactRpc> {
    bounded(query, 512, false)?;
    Ok(ContactRpc {
        cgi: 106,
        path: "/cgi-bin/micromsg-bin/searchcontact".into(),
        body: p::message(&[
            p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
            p::bytes(2, &p::bytes(1, query.as_bytes())),
            p::uint(6, 1),
        ])?,
    })
}
/// Decode only established discovery fields. A result is not proof of friendship.
pub fn decode_contact_search(reply: &[u8], query: &str) -> Result<ContactSearchResult> {
    bounded(query, 512, false)?;
    let fields = p::decode(reply)?;
    ServiceRejected::check(Service::Contact, p::base_code(&fields)?)?;
    let name = p::decode(p::data(&fields, 2)?)?;
    let username = std::str::from_utf8(p::data(&name, 1)?)?.to_owned();
    let ticket =
        std::str::from_utf8(p::optional_data(&fields, 30)?.unwrap_or_default())?.to_owned();
    bounded(&username, 512, false)?;
    bounded(&ticket, 65536, true)?;
    // The supported direct WeChat-ID discovery producer uses scene 3.
    Ok(ContactSearchResult {
        query: query.into(),
        target: ContactRequestTarget {
            username,
            ticket,
            scene: 3,
        },
    })
}
fn verification(
    state: &AccountState,
    target: &ContactRequestTarget,
    message: &str,
    operation: u32,
    privacy: Option<&ContactPrivacySettings>,
) -> Result<ContactRpc> {
    bounded(&target.username, 512, false)?;
    bounded(&target.ticket, 65536, false)?;
    bounded(message, 4096, true)?;
    let identity = target
        .username
        .strip_suffix("@stranger")
        .unwrap_or(&target.username);
    ensure!(
        !identity.is_empty()
            && !identity.contains('@')
            && !target.username.chars().any(char::is_whitespace),
        "ordinary_contact_required"
    );
    let mut user_fields = vec![
        p::bytes(1, target.username.as_bytes()),
        p::bytes(if operation == 2 { 3 } else { 2 }, target.ticket.as_bytes()),
    ];
    if let Some(flags) = privacy
        .map(ContactPrivacySettings::verify_flags)
        .filter(|flags| *flags != 0)
    {
        user_fields.push(p::uint(4, flags.into()));
    }
    let user = p::message(&user_fields)?;
    Ok(ContactRpc {
        cgi: 137,
        path: "/cgi-bin/micromsg-bin/verifyuser".into(),
        body: p::message(&[
            p::bytes(1, &wire::base_request(&state.device, state.uin, 0)?),
            p::uint(2, operation.into()),
            p::uint(3, 1),
            p::bytes(4, &user),
            p::bytes(5, message.as_bytes()),
            p::uint(6, 1),
            p::bytes(7, &p::varint(target.scene.into())),
        ])?,
    })
}
/// Prepare operation 2 using the discovery ticket; no automatic fallback or retry.
pub fn send_contact_request_rpc(
    state: &AccountState,
    input: &SendContactRequest,
) -> Result<ContactRpc> {
    verification(
        state,
        &input.target,
        &input.message,
        2,
        input.privacy.as_ref(),
    )
}
/// Prepare operation 3 using the received ticket and empty verification text.
pub fn accept_contact_request_rpc(
    state: &AccountState,
    input: &AcceptContactRequest,
) -> Result<ContactRpc> {
    verification(state, &input.target, "", 3, input.privacy.as_ref())
}
/// Prepare account-scoped contact removal; preserves the other account's relationship.
pub fn remove_contact_request(username: &str) -> Result<ContactRpc> {
    ordinary_identity(username)?;
    ensure!(!username.contains('@'), "ordinary_contact_required");
    let payload = p::bytes(1, &p::bytes(1, username.as_bytes()));
    let item = p::message(&[p::uint(1, 4), p::bytes(2, &p::buffer(&payload))])?;
    Ok(ContactRpc {
        cgi: 681,
        path: "/cgi-bin/micromsg-bin/oplog".into(),
        body: p::bytes(1, &p::message(&[p::uint(1, 1), p::bytes(2, &item)])?),
    })
}
/// Longest personal remark accepted for submission, in UTF-8 bytes.
pub const MAX_CONTACT_REMARK_BYTES: usize = 256;
/// Build the official personal-remark write for an existing ordinary contact:
/// operation-log opcode 2 carrying the contact's complete current `ModContact`
/// with only the remark (field 10) replaced and its phonetic fields (11, 12)
/// sent empty. Every other field, including the flags, is copied byte for byte,
/// so `record` must come from a fresh exact lookup. An empty remark clears it.
pub fn contact_remark_request(record: &ContactRecord, remark: &str) -> Result<ContactRpc> {
    ordinary_identity(&record.username)?;
    ensure!(!record.username.contains('@'), "ordinary_contact_required");
    bounded(remark, MAX_CONTACT_REMARK_BYTES, true)?;
    ensure!(remark.trim() == remark, "invalid_contact_input");
    ensure!(
        wire::decode_contact_record(&record.raw)?.username == record.username,
        "contact_identity_mismatch"
    );
    let fields = p::decode(&record.raw)?;
    for number in 1..=14 {
        ensure!(
            p::optional(&fields, number)?.is_some(),
            "contact_snapshot_incomplete"
        );
    }
    ensure!(
        p::scalar(&fields, 7)? == u64::from(u32::MAX)
            && !controlled_contact_fields(&record.raw)?.contains(&8),
        "contact_flags_unresolved"
    );
    let mut payload = Vec::new();
    let mut at = 0;
    while at < record.raw.len() {
        let start = at;
        let tag = p::read_varint(&record.raw, &mut at)?;
        match tag & 7 {
            0 => {
                p::read_varint(&record.raw, &mut at)?;
            }
            1 => at += 8,
            2 => {
                let length = usize::try_from(p::read_varint(&record.raw, &mut at)?)?;
                at += length;
            }
            5 => at += 4,
            _ => bail!("invalid_contact_snapshot"),
        }
        ensure!(at <= record.raw.len(), "invalid_contact_snapshot");
        match tag >> 3 {
            10 => payload.extend(p::bytes(10, &p::bytes(1, remark.as_bytes()))),
            11 | 12 => payload.extend(p::bytes((tag >> 3) as u32, &p::bytes(1, b""))),
            _ => payload.extend_from_slice(&record.raw[start..at]),
        }
    }
    let item = p::message(&[p::uint(1, 2), p::bytes(2, &p::buffer(&payload))])?;
    Ok(ContactRpc {
        cgi: 681,
        path: "/cgi-bin/micromsg-bin/oplog".into(),
        body: p::bytes(1, &p::message(&[p::uint(1, 1), p::bytes(2, &item)])?),
    })
}
/// Decode one verification reply; rejection is a verified submission, never a retry hint.
pub fn decode_contact_request(reply: &[u8]) -> Result<ContactRequestReply> {
    let service_code = p::base_code(&p::decode(reply)?)?;
    ServiceRejected::check(Service::Contact, service_code)?;
    Ok(ContactRequestReply { service_code })
}
impl NativeClient {
    pub(crate) fn contact_rpc_raw_snapshot(
        &self,
        state: &mut AccountState,
        request: &ContactRpc,
    ) -> Result<Vec<u8>> {
        let expected = match request.cgi {
            106 => "searchcontact",
            137 => "verifyuser",
            3990 => "batchmodcontacttype",
            681 => "oplog",
            _ => bail!("invalid_contact_rpc"),
        };
        ensure!(
            request.path == format!("/cgi-bin/micromsg-bin/{expected}"),
            "invalid_contact_rpc"
        );
        self.authenticated_rpc(state, request.cgi, &request.path, &request.body)
    }
    pub(crate) fn search_contact_snapshot(
        &self,
        state: &mut AccountState,
        query: &str,
    ) -> Result<ContactSearchResult> {
        let request = contact_search_request(state, query)?;
        decode_contact_search(&self.contact_rpc_raw_snapshot(state, &request)?, query)
    }
    pub(crate) fn send_contact_request_snapshot(
        &self,
        state: &mut AccountState,
        input: &SendContactRequest,
    ) -> Result<ContactRequestReply> {
        let request = send_contact_request_rpc(state, input)?;
        decode_contact_request(&self.contact_rpc_raw_snapshot(state, &request)?)
    }
    pub(crate) fn accept_contact_request_snapshot(
        &self,
        state: &mut AccountState,
        input: &AcceptContactRequest,
    ) -> Result<ContactRequestReply> {
        let request = accept_contact_request_rpc(state, input)?;
        decode_contact_request(&self.contact_rpc_raw_snapshot(state, &request)?)
    }
    pub(crate) fn remove_contact_snapshot(
        &self,
        state: &mut AccountState,
        username: &str,
    ) -> Result<GroupMutationReply> {
        let rpc = remove_contact_request(username)?;
        let reply = self.contact_rpc_raw_snapshot(state, &rpc)?;
        decode_group_mutation(
            &GroupRpc {
                cgi: rpc.cgi,
                path: rpc.path,
                opcode: Some(4),
                body: rpc.body,
            },
            &reply,
            &[],
        )
    }
}
/// Decoded verification data. Contains tickets; never expose it directly in HTTP or logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct FriendMessage {
    /// Per-peer profile row identity selected from the sync envelope.
    pub row_username: String,
    /// Exact resolved verification target and private ticket.
    pub target: ContactRequestTarget,
    /// Stored discovery alias, preserved independently of the row identity.
    pub encrypted_username: String,
    /// Verification greeting retained in this history entry.
    pub content: String,
    /// Verification operation value from the message.
    pub operation: u32,
    /// Whether this account sent the verification message.
    pub sender: bool,
    /// Decimal server message ID; locally sent entries use zero.
    pub server_id: String,
    /// Unix timestamp in seconds.
    pub timestamp: u32,
    /// Verification message type from the supported sync envelope.
    pub message_type: i32,
}
/// Decode supported ordinary verification records. Other message types return
/// None. Malformed or unsupported verification XML returns an error so callers
/// can retain the sync obligation instead of silently advancing its cursor.
pub fn decode_friend_message(
    message: &wire::AddMessage,
    own: &str,
) -> Result<Option<FriendMessage>> {
    if !matches!(message.message_type, 37 | 65) {
        return Ok(None);
    }
    ensure!(
        message.content.len() <= 256 * 1024,
        "invalid_friend_message"
    );
    let xml = std::str::from_utf8(&message.content)?;
    ensure!(
        !xml.to_ascii_uppercase().contains("<!DOCTYPE")
            && !xml.to_ascii_uppercase().contains("<!ENTITY"),
        "invalid_friend_message"
    );
    let document = roxmltree::Document::parse(xml)?;
    ensure!(
        document.descendants().count() <= 4096,
        "invalid_friend_message"
    );
    let root = document.root_element();
    ensure!(
        root.tag_name().name() == "msg" && root.tag_name().namespace().is_none(),
        "invalid_friend_message"
    );
    let attr = |name: &str| {
        root.attribute(name)
            .ok_or_else(|| anyhow::anyhow!("incomplete_friend_message"))
    };
    let operation = attr("opcode")?.parse::<u32>()?;
    ensure!((1..=6).contains(&operation), "unsupported_friend_operation");
    let mut username = attr("fromusername")?.to_owned();
    if username.is_empty() {
        username = root.attribute("encryptusername").unwrap_or("").to_owned();
    }
    let ticket = attr("ticket")?.to_owned();
    let scene = attr("scene")?.parse::<u32>()?;
    bounded(&username, 512, false)?;
    bounded(&ticket, 65536, operation != 2)?;
    let encrypted_username = if message.to == own {
        root.attribute("encryptusername").unwrap_or("").to_owned()
    } else {
        String::new()
    };
    let row_username = if message.to == own {
        username.clone()
    } else {
        message.to.clone()
    };
    bounded(&row_username, 512, false)?;
    let content = attr("content")?.to_owned();
    bounded(&content, 4096, true)?;
    let sender = username == own;
    Ok(Some(FriendMessage {
        row_username,
        target: ContactRequestTarget {
            username,
            ticket,
            scene,
        },
        encrypted_username,
        content,
        operation,
        sender,
        server_id: message
            .server_message_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing_friend_message_id"))?,
        timestamp: message.created_at,
        message_type: message.message_type,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![1; 16],
                device_type: "UnifiedPCLinux synthetic x86_64".into(),
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
    #[test]
    fn send_and_accept_use_different_ticket_fields_and_only_one_scene() {
        let state = state();
        let target = ContactRequestTarget {
            username: "opaque@stranger".into(),
            ticket: "ticket".into(),
            scene: 3,
        };
        let send = send_contact_request_rpc(
            &state,
            &SendContactRequest {
                target: target.clone(),
                message: "hello".into(),
                privacy: None,
            },
        )
        .unwrap();
        let accept = accept_contact_request_rpc(
            &state,
            &AcceptContactRequest {
                target,
                privacy: None,
            },
        )
        .unwrap();
        for (rpc, opcode, ticket_field, text) in [(send, 2, 3, "hello"), (accept, 3, 2, "")] {
            let fields = p::decode(&rpc.body).unwrap();
            assert_eq!(p::scalar(&fields, 2).unwrap(), opcode);
            let user = p::decode(p::data(&fields, 4).unwrap()).unwrap();
            assert_eq!(p::data(&user, ticket_field).unwrap(), b"ticket");
            assert!(p::optional(&user, if ticket_field == 3 { 2 } else { 3 })
                .unwrap()
                .is_none());
            assert_eq!(p::data(&fields, 5).unwrap(), text.as_bytes());
            assert_eq!(p::data(&fields, 7).unwrap(), [3]);
            assert!(p::optional(&fields, 11).unwrap().is_none());
        }
    }
    #[test]
    fn discovery_rejection_and_missing_ticket_are_not_sendable_results() {
        let rejected = p::bytes(1, &p::int32(1, -24));
        let e = decode_contact_search(&rejected, "example").err().unwrap();
        assert_eq!(ServiceRejected::find(&e).unwrap().code, -24);
        let missing = p::message(&[
            p::bytes(1, &p::int32(1, 0)),
            p::bytes(2, &p::bytes(1, b"opaque@stranger")),
        ])
        .unwrap();
        let found = decode_contact_search(&missing, "example").unwrap();
        assert!(found.target.ticket.is_empty());
        assert!(send_contact_request_rpc(
            &state(),
            &SendContactRequest {
                target: found.target,
                message: "hello".into(),
                privacy: None,
            }
        )
        .is_err());
    }
    #[test]
    fn verification_sender_and_row_identity_come_from_different_sources() {
        let xml=br#"<msg fromusername="self" encryptusername="opaque@stranger" opcode="2" scene="3" ticket="ticket" content="hello"/>"#;
        let raw = p::message(&[
            p::int32(1, 1),
            p::bytes(2, &p::bytes(1, b"self")),
            p::bytes(3, &p::bytes(1, b"peer")),
            p::uint(4, 37),
            p::bytes(5, &p::bytes(1, xml)),
            p::uint(6, 0),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, 100),
            p::uint(12, u64::MAX),
        ])
        .unwrap();
        let msg = wire::decode_add_message(&raw, "self").unwrap();
        let friend = decode_friend_message(&msg, "self").unwrap().unwrap();
        assert!(friend.sender);
        assert_eq!(friend.row_username, "peer");
        assert_eq!(friend.encrypted_username, "");
        assert_eq!(friend.server_id, u64::MAX.to_string());
        let mut altered = msg;
        altered.outgoing = false;
        assert!(
            decode_friend_message(&altered, "self")
                .unwrap()
                .unwrap()
                .sender
        );
        altered.content =
            br#"<!DOCTYPE msg [<!ENTITY x SYSTEM "file:///etc/passwd">]><msg/>"#.to_vec();
        assert!(decode_friend_message(&altered, "self").is_err());
    }
}

#[cfg(test)]
mod remark_tests {
    use super::*;

    /// A fresh synthetic `ModContact` with required fields 1-14, full flag mask,
    /// an unrelated field and a remark-controlling field 89.
    fn record(remark: &str) -> ContactRecord {
        let mut fields = vec![p::bytes(1, &p::bytes(1, b"peer"))];
        fields.push(p::bytes(2, &p::bytes(1, b"Peer")));
        fields.push(p::bytes(3, &p::bytes(1, b"P")));
        fields.push(p::bytes(4, &p::bytes(1, b"peer")));
        fields.push(p::uint(5, 0));
        fields.push(p::bytes(6, &p::buffer(b"")));
        fields.push(p::uint(7, u64::from(u32::MAX)));
        fields.push(p::uint(8, 0x803));
        fields.push(p::uint(9, 0));
        fields.push(p::bytes(10, &p::bytes(1, remark.as_bytes())));
        fields.push(p::bytes(11, &p::bytes(1, b"old")));
        fields.push(p::bytes(12, &p::bytes(1, b"old")));
        fields.push(p::uint(13, 0));
        fields.push(p::uint(14, 0));
        fields.push(p::bytes(123, b"preserve unknown"));
        let control = p::message(&[
            p::uint(1, 1),
            p::uint(2, 12),
            p::uint(3, 3),
            p::bytes(4, &p::uint(1, 10)),
            p::bytes(4, &p::uint(1, 11)),
            p::bytes(4, &p::uint(1, 12)),
        ])
        .unwrap();
        fields.push(p::bytes(
            89,
            &p::message(&[p::uint(1, 1), p::bytes(2, &control)]).unwrap(),
        ));
        let raw = p::message(&fields).unwrap();
        wire::decode_contact_record(&raw).unwrap()
    }
    fn payload(rpc: &ContactRpc) -> Vec<p::Field> {
        let outer = p::decode(&rpc.body).unwrap();
        let list = p::decode(p::data(&outer, 1).unwrap()).unwrap();
        assert_eq!(p::scalar(&list, 1).unwrap(), 1);
        let item = p::decode(p::data(&list, 2).unwrap()).unwrap();
        assert_eq!(p::scalar(&item, 1).unwrap(), 2, "operation-log opcode 2");
        let buffer = p::decode(p::data(&item, 2).unwrap()).unwrap();
        p::decode(p::data(&buffer, 2).unwrap()).unwrap()
    }
    fn text(fields: &[p::Field], n: u32) -> Vec<u8> {
        let nested = p::decode(p::data(fields, n).unwrap()).unwrap();
        p::optional_data(&nested, 1)
            .unwrap()
            .unwrap_or_default()
            .to_vec()
    }

    #[test]
    fn remark_write_replaces_only_remark_fields_and_copies_the_rest() {
        let old = record("Old name");
        let rpc = contact_remark_request(&old, "新名字").unwrap();
        assert_eq!(rpc.cgi, 681);
        assert_eq!(rpc.path, "/cgi-bin/micromsg-bin/oplog");
        let fields = payload(&rpc);
        assert_eq!(text(&fields, 10), "新名字".as_bytes());
        assert!(text(&fields, 11).is_empty());
        assert!(text(&fields, 12).is_empty());
        let before = p::decode(&old.raw).unwrap();
        for n in [1, 2, 3, 4, 6, 89, 123] {
            assert_eq!(
                p::optional_data(&fields, n).unwrap(),
                p::optional_data(&before, n).unwrap(),
                "field {n} must be copied unchanged"
            );
        }
        for n in [5, 7, 8, 9, 13, 14] {
            assert_eq!(
                p::scalar(&fields, n).unwrap(),
                p::scalar(&before, n).unwrap(),
                "field {n} must be copied unchanged"
            );
        }
        let numbers = |f: &[p::Field]| f.iter().map(|f| f.number).collect::<Vec<_>>();
        assert_eq!(
            numbers(&fields),
            numbers(&before),
            "field order is preserved"
        );
    }

    #[test]
    fn an_empty_remark_clears_it() {
        let fields = payload(&contact_remark_request(&record("Old name"), "").unwrap());
        assert!(text(&fields, 10).is_empty());
    }

    #[test]
    fn remark_writes_reject_unsupported_targets_and_input() {
        let ok = record("x");
        for bad in [
            " padded",
            "line\nbreak",
            &"x".repeat(MAX_CONTACT_REMARK_BYTES + 1),
        ] {
            assert!(contact_remark_request(&ok, bad).is_err(), "{bad:?}");
        }
        let mut group = ok.clone();
        group.username = "room@chatroom".into();
        assert!(contact_remark_request(&group, "x").is_err());
        // A partial record is not a complete snapshot to copy.
        let partial = wire::decode_contact_record(
            &p::message(&[
                p::bytes(1, &p::bytes(1, b"peer")),
                p::uint(7, u64::from(u32::MAX)),
            ])
            .unwrap(),
        )
        .unwrap();
        assert!(contact_remark_request(&partial, "x").is_err());
    }
}
