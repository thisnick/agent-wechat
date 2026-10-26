//! Ordinary Linux group mutations. Each call submits exactly once. The caller
//! must journal request admission and retain raw replies before decoding them.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum GroupMutation {
    Create {
        members: Vec<String>,
        name: Option<String>,
    },
    Rename {
        group_id: String,
        name: String,
    },
    Leave {
        group_id: String,
    },
    AddMembers {
        group_id: String,
        members: Vec<String>,
    },
    RemoveMembers {
        group_id: String,
        members: Vec<String>,
    },
    MyName {
        group_id: String,
        display_name: String,
    },
    ClearAnnouncement {
        group_id: String,
    },
    Mute {
        thread_id: String,
        muted: bool,
    },
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupMemberResult {
    pub username: String,
    /// Delete replies have no member status. Never synthesize one.
    pub status: Option<u32>,
    pub nickname: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMutationReply {
    pub service_code: i32,
    pub command_results: Vec<i32>,
    pub group_id: Option<String>,
    pub name: Option<String>,
    pub name_initials: Option<String>,
    pub name_pinyin: Option<String>,
    pub members: Vec<GroupMemberResult>,
    pub declared_count: Option<u32>,
    pub accepted: bool,
    pub uncertain: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct GroupRpc {
    pub cgi: u16,
    pub path: String,
    pub opcode: Option<u32>,
    pub body: Vec<u8>,
}
pub fn ordinary_identity(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && !value.chars().any(|c| c.is_control() || c.is_whitespace()),
        "invalid_group_identity"
    );
    Ok(())
}
fn group(value: &str) -> Result<()> {
    ordinary_identity(value)?;
    ensure!(
        value.ends_with("@chatroom") && value.len() > 9,
        "invalid_group_identity"
    );
    Ok(())
}
fn title(value: &str, empty: bool) -> Result<()> {
    ensure!(
        (empty || !value.trim().is_empty())
            && value.len() <= 512
            && !value.chars().any(char::is_control),
        "invalid_group_text"
    );
    Ok(())
}
impl GroupMutation {
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Mute { thread_id, .. } => Some(thread_id),
            Self::Rename { group_id, .. }
            | Self::Leave { group_id }
            | Self::AddMembers { group_id, .. }
            | Self::RemoveMembers { group_id, .. }
            | Self::MyName { group_id, .. }
            | Self::ClearAnnouncement { group_id } => Some(group_id),
        }
    }
    pub fn validate(&self, own: &str) -> Result<()> {
        ordinary_identity(own)?;
        if let Some(id) = self.target() {
            if matches!(self, Self::Mute { .. }) && !id.ends_with("@chatroom") {
                ordinary_identity(id)?;
                ensure!(id != own && !id.contains('@'), "ordinary_contact_required");
            } else {
                group(id)?;
            }
        }
        match self {
            Self::Create { members, name } => {
                validate_members(members, own)?;
                ensure!(members.len() >= 2, "invalid_group_members");
                if let Some(name) = name {
                    title(name, true)?;
                }
            }
            Self::AddMembers { members, .. } | Self::RemoveMembers { members, .. } => {
                validate_members(members, own)?
            }
            Self::Rename { name, .. } => title(name, false)?,
            Self::MyName { display_name, .. } => title(display_name, true)?,
            _ => {}
        }
        Ok(())
    }
}
fn validate_members(members: &[String], own: &str) -> Result<()> {
    ensure!(
        !members.is_empty() && members.len() <= 40,
        "invalid_group_members"
    );
    let mut seen = HashSet::new();
    for id in members {
        ordinary_identity(id)?;
        ensure!(
            id != own && !id.contains('@') && seen.insert(id),
            "invalid_group_members"
        );
    }
    Ok(())
}
fn sk(value: &str) -> Vec<u8> {
    p::bytes(1, value.as_bytes())
}
fn text_field(fields: &[p::Field], n: u32) -> Result<String> {
    let raw = p::data(fields, n)?;
    // The Linux MemberResp requires the outer wrapper, but accepts an empty
    // SKString message. A missing outer field remains invalid.
    if raw.is_empty() {
        return Ok(String::new());
    }
    Ok(std::str::from_utf8(p::data(&p::decode(raw)?, 1)?)?.to_owned())
}
fn u32_field(fields: &[p::Field], n: u32) -> Result<u32> {
    Ok(u32::try_from(p::scalar(fields, n)?)?)
}
fn signed(value: u64) -> Result<i32> {
    let code = value as u32 as i32;
    ensure!(
        value <= u64::from(u32::MAX) || value == code as i64 as u64,
        "invalid_group_response"
    );
    Ok(code)
}
fn oplog(opcode: u32, payload: Vec<u8>) -> Result<GroupRpc> {
    let item = p::message(&[p::uint(1, opcode.into()), p::bytes(2, &p::buffer(&payload))])?;
    Ok(GroupRpc {
        cgi: 681,
        path: "/cgi-bin/micromsg-bin/oplog".into(),
        opcode: Some(opcode),
        body: p::bytes(1, &p::message(&[p::uint(1, 1), p::bytes(2, &item)])?),
    })
}
/// Direct contact writes use the official full-contact oplog shape. Fetch a
/// current record first; change only the mute bit and preserve all other bytes.
pub fn contact_notification_request(record: &ContactRecord, muted: bool) -> Result<GroupRpc> {
    ordinary_identity(&record.username)?;
    ensure!(!record.username.contains('@'), "ordinary_contact_required");
    let fields = p::decode(&record.raw)?;
    ensure!(
        wire::decode_contact_record(&record.raw)?.username == record.username,
        "contact_identity_mismatch"
    );
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
    let flag = u32::try_from(p::scalar(&fields, 8)?)?;
    ensure!(flag & 1 != 0, "ordinary_contact_required");
    let flag = (flag & !0x200) | if muted { 0x200 } else { 0 };
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
        if tag >> 3 == 8 {
            payload.extend(p::uint(8, flag.into()));
        } else {
            payload.extend_from_slice(&record.raw[start..at]);
        }
    }
    oplog(2, payload)
}
/// Steps are separate requests. An uncertain step must stop the entire flow.
/// A known cleanup rejection can be retained while a later quit is attempted.
pub fn group_mutation_requests(
    state: &AccountState,
    action: &GroupMutation,
    minimized: bool,
) -> Result<Vec<GroupRpc>> {
    state.validate()?;
    action.validate(&state.own_username)?;
    let target = action.target().unwrap_or_default();
    let base = || wire::base_request(&state.device, state.uin, 0);
    let rpc = |cgi, path: &str, body| GroupRpc {
        cgi,
        path: path.into(),
        opcode: None,
        body,
    };
    Ok(match action {
        GroupMutation::Create { members, name } => {
            let mut fields = vec![
                p::bytes(1, &base()?),
                p::bytes(2, &sk(name.as_deref().unwrap_or_default())),
                p::uint(3, members.len() as u64),
            ];
            fields.extend(members.iter().map(|m| p::bytes(4, &p::bytes(1, &sk(m)))));
            vec![rpc(
                119,
                "/cgi-bin/micromsg-bin/createchatroom",
                p::message(&fields)?,
            )]
        }
        GroupMutation::AddMembers { members, .. }
        | GroupMutation::RemoveMembers { members, .. } => {
            let add = matches!(action, GroupMutation::AddMembers { .. });
            let mut fields = vec![p::bytes(1, &base()?), p::uint(2, members.len() as u64)];
            fields.extend(members.iter().map(|m| p::bytes(3, &p::bytes(1, &sk(m)))));
            fields.push(p::bytes(
                4,
                &if add {
                    sk(target)
                } else {
                    target.as_bytes().to_vec()
                },
            ));
            if !add {
                fields.push(p::uint(5, 0));
            }
            vec![rpc(
                if add { 120 } else { 179 },
                if add {
                    "/cgi-bin/micromsg-bin/addchatroommember"
                } else {
                    "/cgi-bin/micromsg-bin/delchatroommember"
                },
                p::message(&fields)?,
            )]
        }
        GroupMutation::Rename { name, .. } => vec![oplog(
            27,
            p::message(&[p::bytes(1, &sk(target)), p::bytes(2, &sk(name))])?,
        )?],
        GroupMutation::Leave { .. } => vec![
            oplog(4, p::bytes(1, &sk(target)))?,
            oplog(7, p::bytes(1, &sk(target)))?,
            oplog(
                16,
                p::message(&[
                    p::bytes(1, &sk(target)),
                    p::bytes(2, &sk(&state.own_username)),
                ])?,
            )?,
        ],
        GroupMutation::MyName { display_name, .. } => vec![oplog(
            48,
            p::message(&[
                p::bytes(1, target.as_bytes()),
                p::bytes(2, state.own_username.as_bytes()),
                p::bytes(3, display_name.as_bytes()),
            ])?,
        )?],
        GroupMutation::ClearAnnouncement { .. } => vec![rpc(
            993,
            "/cgi-bin/micromsg-bin/setchatroomannouncement",
            p::message(&[
                p::bytes(1, &base()?),
                p::bytes(2, target.as_bytes()),
                p::bytes(3, b""),
                p::bytes(5, b""),
                p::uint(8, 1),
            ])?,
        )],
        GroupMutation::Mute { .. } if !target.ends_with("@chatroom") => {
            bail!("contact_snapshot_required")
        }
        GroupMutation::Mute { muted, .. } => {
            let mut out = vec![oplog(
                20,
                p::message(&[p::bytes(1, &sk(target)), p::uint(2, u64::from(!muted))])?,
            )?];
            if !muted && minimized {
                out.push(oplog(
                    74,
                    p::message(&[p::bytes(1, &sk(target)), p::uint(2, 0)])?,
                )?);
            }
            out
        }
    })
}
/// Recover a valid create identity even when another success field is malformed.
/// This is evidence for review, never permission to repeat the creation.
pub fn observed_created_group(raw: &[u8]) -> Option<String> {
    let fields = p::decode(raw).ok()?;
    let id = text_field(&fields, 7).ok()?;
    group(&id).ok()?;
    Some(id)
}
pub fn decode_group_mutation(
    rpc: &GroupRpc,
    raw: &[u8],
    requested: &[String],
) -> Result<GroupMutationReply> {
    let f = p::decode(raw)?;
    let mut out = GroupMutationReply {
        service_code: 0,
        command_results: vec![],
        group_id: None,
        name: None,
        name_initials: None,
        name_pinyin: None,
        members: vec![],
        declared_count: None,
        accepted: false,
        uncertain: false,
    };
    if rpc.cgi == 681 {
        out.service_code = signed(p::scalar(&f, 1)?)?;
        if out.service_code != 0 {
            out.uncertain = out.service_code == -433;
            return Ok(out);
        }
        let ret = p::decode(p::data(&f, 2)?)?;
        let count = u32_field(&ret, 1)?;
        let mut values = vec![];
        for field in ret.iter().filter(|f| f.number == 2) {
            match &field.value {
                p::Value::Bytes(bytes) => {
                    let mut at = 0;
                    while at < bytes.len() {
                        values.push(signed(p::read_varint(bytes, &mut at)?)?);
                    }
                }
                p::Value::Varint(value) => values.push(signed(*value)?),
                _ => bail!("invalid_group_response"),
            }
        }
        ensure!(
            count == 1 && values.len() == 1,
            "invalid_group_result_count"
        );
        out.accepted = values[0] == 0;
        out.uncertain = values[0] == -433;
        out.command_results = values;
        return Ok(out);
    }
    out.service_code = p::base_code(&f)?;
    if rpc.cgi == 119 {
        out.group_id = observed_created_group(raw);
    }
    if out.service_code != 0 {
        out.uncertain = out.service_code == -433;
        return Ok(out);
    }
    if rpc.cgi == 993 {
        out.accepted = true;
        return Ok(out);
    }
    let create = rpc.cgi == 119;
    let remove = rpc.cgi == 179;
    ensure!(create || remove || rpc.cgi == 120, "invalid_group_rpc");
    out.declared_count = Some(u32_field(&f, if create { 5 } else { 2 })?);
    if create {
        ensure!(out.group_id.is_some(), "missing_created_group");
        out.name = Some(text_field(&f, 2)?);
        out.name_initials = Some(text_field(&f, 3)?);
        out.name_pinyin = Some(text_field(&f, 4)?);
        p::unbuffer(p::data(&f, 8)?)?;
    }
    let mut seen = HashSet::new();
    for field in f.iter().filter(|x| x.number == if create { 6 } else { 3 }) {
        let p::Value::Bytes(bytes) = &field.value else {
            bail!("invalid_group_member_reply")
        };
        let m = p::decode(bytes)?;
        let username = text_field(&m, 1)?;
        ensure!(
            requested.contains(&username) && seen.insert(username.clone()),
            "foreign_group_member_reply"
        );
        let (status, nickname) = if remove {
            (None, None)
        } else {
            for n in [3, 4, 5, 9, 10, 11] {
                text_field(&m, n)?;
            }
            signed(p::scalar(&m, 6)?)?;
            u32_field(&m, 12)?;
            (Some(u32_field(&m, 2)?), Some(text_field(&m, 3)?))
        };
        out.members.push(GroupMemberResult {
            username,
            status,
            nickname,
        });
    }
    // Acceptance and completeness remain distinct. Preserve unknown statuses,
    // subsets and count-like values rather than inventing missing successes.
    out.accepted = true;
    Ok(out)
}
impl NativeClient {
    pub fn group_mutation_raw(
        &self,
        state: &mut AccountState,
        request: &GroupRpc,
    ) -> Result<Vec<u8>> {
        ensure!(
            matches!(request.cgi, 119 | 120 | 179 | 681 | 993),
            "invalid_group_rpc"
        );
        let expected = match request.cgi {
            119 => "createchatroom",
            120 => "addchatroommember",
            179 => "delchatroommember",
            681 => "oplog",
            _ => "setchatroomannouncement",
        };
        ensure!(
            request.path == format!("/cgi-bin/micromsg-bin/{expected}"),
            "invalid_group_rpc"
        );
        self.authenticated_rpc(state, request.cgi, &request.path, &request.body)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn member(id: &str, status: u32) -> Vec<u8> {
        let mut f = vec![
            p::bytes(1, &sk(id)),
            p::uint(2, status.into()),
            p::int32(6, 0),
            p::uint(12, 1),
        ];
        for n in [3, 4, 5, 9, 10, 11] {
            f.push(p::bytes(n, &sk("")));
        }
        p::message(&f).unwrap()
    }
    #[test]
    fn packed_signed_results_and_uncertain_special_code() {
        let rpc = oplog(16, vec![]).unwrap();
        for n in [0, -1, -433, i32::MIN] {
            let raw = p::message(&[
                p::int32(1, 0),
                p::bytes(
                    2,
                    &p::message(&[
                        p::uint(1, 1),
                        p::bytes(2, &p::varint(n as i64 as u64)),
                        p::bytes(3, b""),
                    ])
                    .unwrap(),
                ),
            ])
            .unwrap();
            let out = decode_group_mutation(&rpc, &raw, &[]).unwrap();
            assert_eq!(out.command_results, [n]);
            assert_eq!(out.accepted, n == 0);
            assert_eq!(out.uncertain, n == -433);
        }
        assert!(decode_group_mutation(
            &rpc,
            &p::message(&[p::uint(1, 0), p::bytes(2, &p::uint(1, 1))]).unwrap(),
            &[]
        )
        .is_err());
    }
    #[test]
    fn create_preserves_partial_unknown_status_and_identity_on_malformed_member() {
        let rpc = GroupRpc {
            cgi: 119,
            path: String::new(),
            opcode: None,
            body: vec![],
        };
        let mut f = vec![
            p::bytes(1, &p::int32(1, 0)),
            p::uint(5, 17),
            p::bytes(7, &sk("synthetic@chatroom")),
            p::bytes(8, &p::uint(1, 0)),
        ];
        for n in [2, 3, 4] {
            f.push(p::bytes(n, &sk("")));
        }
        f.push(p::bytes(6, &member("peer", u32::MAX)));
        let raw = p::message(&f).unwrap();
        let out = decode_group_mutation(&rpc, &raw, &["peer".into()]).unwrap();
        assert!(out.accepted);
        assert_eq!(out.declared_count, Some(17));
        assert_eq!(out.members[0].status, Some(u32::MAX));
        f.push(p::bytes(6, &p::bytes(1, &sk("other"))));
        let bad = p::message(&f).unwrap();
        assert!(decode_group_mutation(&rpc, &bad, &["peer".into(), "other".into()]).is_err());
        assert_eq!(
            observed_created_group(&bad).as_deref(),
            Some("synthetic@chatroom")
        );
    }
    #[test]
    fn required_outer_wrappers_can_be_empty_but_cannot_be_missing() {
        let rpc = GroupRpc {
            cgi: 119,
            path: String::new(),
            opcode: None,
            body: vec![],
        };
        let mut member = vec![
            p::bytes(1, &sk("peer")),
            p::uint(2, 0),
            p::uint(6, 0),
            p::uint(12, 1),
        ];
        for n in [3, 4, 5, 9, 10, 11] {
            member.push(p::bytes(n, b""));
        }
        let f = [
            p::bytes(1, &p::int32(1, 0)),
            p::bytes(2, b""),
            p::bytes(3, b""),
            p::bytes(4, b""),
            p::uint(5, 1),
            p::bytes(6, &p::message(&member).unwrap()),
            p::bytes(7, &sk("room@chatroom")),
            p::bytes(8, &p::uint(1, 0)),
        ];
        assert!(
            decode_group_mutation(&rpc, &p::message(&f).unwrap(), &["peer".into()])
                .unwrap()
                .accepted
        );
        let missing = f
            .into_iter()
            .enumerate()
            .filter_map(|(i, f)| (i != 1).then_some(f))
            .collect::<Vec<_>>();
        assert!(
            decode_group_mutation(&rpc, &p::message(&missing).unwrap(), &["peer".into()]).is_err()
        );
    }
}
