//! Local replies follow the checked-in Linux group codec fixtures. Core keeps
//! admission, immutable requests, one-attempt steps, receipts and projection.
use super::*;
use wechat_protocol::GroupRpc;

fn text(fields: &[p::Field], field: u32) -> Result<String> {
    Ok(std::str::from_utf8(p::data(fields, field)?)?.into())
}
fn sk(fields: &[p::Field], field: u32) -> Result<String> {
    text(&p::decode(p::data(fields, field)?)?, 1)
}
fn label(username: &str) -> &str {
    match username {
        "sim_mei" => "Mei Chen",
        "sim_daniel" => "Daniel Park",
        "filehelper" => "File Transfer",
        _ => "Alex Morgan",
    }
}
pub(super) fn contact(db: &Connection, account: &str, username: &str) -> Result<Vec<u8>> {
    let group = username.ends_with("@chatroom");
    let mut fields = vec![
        p::bytes(1, &p::bytes(1, username.as_bytes())),
        p::uint(7, u32::MAX.into()),
        p::uint(
            8,
            if group {
                2
            } else if username == "filehelper" {
                0
            } else {
                1
            },
        ),
    ];
    let name = if group {
        let (name, muted, joined): (String, bool, bool) = db.query_row(
            "SELECT name,muted,joined FROM groups WHERE account=?1 AND room=?2",
            params![account, username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        fields.extend([
            p::uint(17, u64::from(!muted)),
            p::uint(85, if joined { 1 } else { 2 }),
        ]);
        name
    } else {
        label(username).into()
    };
    fields.push(p::bytes(2, &p::bytes(1, name.as_bytes())));
    fields.push(p::bytes(
        40,
        format!("agentwechat-simulator://avatar/{username}").as_bytes(),
    ));
    p::message(&fields)
}
fn member_reply(username: &str, remove: bool, status: u64) -> Result<Vec<u8>> {
    let mut fields = vec![p::bytes(1, &p::bytes(1, username.as_bytes()))];
    if !remove {
        fields.extend([p::uint(2, status), p::int32(6, 0), p::uint(12, 1)]);
        for field in [3, 4, 5, 9, 10, 11] {
            fields.push(p::bytes(
                field,
                &p::bytes(
                    1,
                    if field == 3 {
                        label(username).as_bytes()
                    } else {
                        b""
                    },
                ),
            ));
        }
    }
    p::message(&fields)
}
pub(super) fn mutate(db: &mut Connection, session: &Session, rpc: &GroupRpc) -> Result<Vec<u8>> {
    check_session(db, session)?;
    let fields = p::decode(&rpc.body)?;
    let tx = db.transaction()?;
    let fault: GroupFault = tx
        .query_row(
            "SELECT fault FROM group_faults WHERE account=?1",
            [&session.account],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?
        .unwrap_or(GroupFault::None);
    tx.execute(
        "DELETE FROM group_faults WHERE account=?1",
        [&session.account],
    )?;
    if fault == GroupFault::Reject {
        tx.commit()?;
        return if rpc.cgi == 681 {
            Ok(p::int32(1, -1))
        } else {
            Ok(p::bytes(1, &p::int32(1, -1)))
        };
    }
    let base = p::bytes(1, &p::int32(1, 0));
    let mut response = vec![base];
    match rpc.cgi {
        119 | 120 | 179 => {
            let create = rpc.cgi == 119;
            let remove = rpc.cgi == 179;
            let member_field = if create { 4 } else { 3 };
            let mut requested = vec![];
            for field in fields.iter().filter(|f| f.number == member_field) {
                let p::Value::Bytes(raw) = &field.value else {
                    bail!("invalid_simulator_group_request")
                };
                let username = sk(&p::decode(raw)?, 1)?;
                ensure!(
                    ["sim_mei", "sim_daniel"].contains(&username.as_str()),
                    "unknown_synthetic_peer"
                );
                requested.push(username);
            }
            ensure!(
                !requested.is_empty() && requested.len() <= 40,
                "invalid_simulator_group_request"
            );
            let room = if create {
                format!("simroom_{}@chatroom", uuid::Uuid::new_v4().simple())
            } else if remove {
                text(&fields, 4)?
            } else {
                sk(&fields, 4)?
            };
            let name = if create {
                sk(&fields, 2)?
            } else {
                String::new()
            };
            let accepted = if fault == GroupFault::PartialMembers {
                requested[..requested.len() - 1].to_vec()
            } else {
                requested.clone()
            };
            if create {
                ensure!(requested.len() >= 2, "invalid_simulator_group_request");
                tx.execute(
                    "INSERT INTO groups(account,room,name,members) VALUES(?1,?2,?3,?4)",
                    params![
                        session.account,
                        room,
                        name,
                        serde_json::to_string(&accepted)?
                    ],
                )?;
            } else {
                validate_destination(&tx, &session.account, &room)?;
                let raw: String = tx.query_row(
                    "SELECT members FROM groups WHERE account=?1 AND room=?2",
                    params![session.account, room],
                    |r| r.get(0),
                )?;
                let mut members: Vec<String> = serde_json::from_str(&raw)?;
                for username in &accepted {
                    if remove {
                        members.retain(|u| u != username)
                    } else if !members.contains(username) {
                        members.push(username.clone())
                    }
                }
                tx.execute(
                    "UPDATE groups SET members=?3 WHERE account=?1 AND room=?2",
                    params![session.account, room, serde_json::to_string(&members)?],
                )?;
            }
            response.push(p::uint(if create { 5 } else { 2 }, requested.len() as u64));
            for username in &requested {
                if remove && !accepted.contains(username) {
                    continue;
                }
                response.push(p::bytes(
                    if create { 6 } else { 3 },
                    &member_reply(
                        username,
                        remove,
                        if accepted.contains(username) { 0 } else { 4 },
                    )?,
                ));
            }
            if create {
                for field in [2, 3, 4] {
                    response.push(p::bytes(
                        field,
                        &p::bytes(1, if field == 2 { name.as_bytes() } else { b"" }),
                    ));
                }
                response.push(p::bytes(7, &p::bytes(1, room.as_bytes())));
                response.push(p::bytes(8, &p::uint(1, 0)));
                // A synthetic notice gives the new group a visible conversation.
                let (_, notice) = store_message(
                    &tx,
                    &session.account,
                    10000,
                    &room,
                    &own(&session.account),
                    b"Group created in the simulator.",
                    clock(&tx)?,
                )?;
                append(&tx, &session.account, &notice)?;
                notify(&tx, &session.account, 1)?;
            }
        }
        681 => {
            let list = p::decode(p::data(&fields, 1)?)?;
            ensure!(p::scalar(&list, 1)? == 1, "invalid_simulator_group_request");
            let item = p::decode(p::data(&list, 2)?)?;
            let opcode = p::scalar(&item, 1)?;
            ensure!(
                rpc.opcode == Some(opcode as u32),
                "invalid_simulator_group_request"
            );
            let payload = p::decode(&p::unbuffer(p::data(&item, 2)?)?)?;
            let room = if opcode == 48 {
                text(&payload, 1)?
            } else {
                sk(&payload, 1)?
            };
            validate_destination(&tx, &session.account, &room)?;
            match opcode {
                4 | 7 => {} // Leave cleanup is acknowledged; cached history remains.
                16 => {
                    tx.execute(
                        "UPDATE groups SET joined=0 WHERE account=?1 AND room=?2",
                        params![session.account, room],
                    )?;
                }
                27 => {
                    tx.execute(
                        "UPDATE groups SET name=?3 WHERE account=?1 AND room=?2",
                        params![session.account, room, sk(&payload, 2)?],
                    )?;
                }
                48 => {
                    ensure!(
                        text(&payload, 2)? == own(&session.account),
                        "invalid_simulator_group_request"
                    );
                    tx.execute(
                        "UPDATE groups SET my_name=?3 WHERE account=?1 AND room=?2",
                        params![session.account, room, text(&payload, 3)?],
                    )?;
                }
                20 => {
                    tx.execute(
                        "UPDATE groups SET muted=?3 WHERE account=?1 AND room=?2",
                        params![session.account, room, p::scalar(&payload, 2)? == 0],
                    )?;
                }
                74 => {}
                _ => bail!("unsupported_simulator_group_operation"),
            }
            response = vec![
                p::int32(1, 0),
                p::bytes(
                    2,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &p::varint(0)), p::bytes(3, b"")])?,
                ),
            ];
        }
        993 => {
            let room = text(&fields, 2)?;
            validate_destination(&tx, &session.account, &room)?;
            ensure!(
                text(&fields, 3)?.is_empty(),
                "unsupported_simulator_group_operation"
            );
            tx.execute(
                "UPDATE groups SET announcement='' WHERE account=?1 AND room=?2",
                params![session.account, room],
            )?;
        }
        _ => bail!("unsupported_simulator_group_operation"),
    }
    tx.commit()?;
    if fault == GroupFault::LostAcknowledgment {
        bail!("simulated_lost_acknowledgment");
    }
    p::message(&response)
}
pub(super) fn roster(db: &Connection, session: &Session, room: &str) -> Result<Vec<u8>> {
    check_session(db, session)?;
    validate_destination(db, &session.account, room)?;
    let (raw, my_name): (String, String) = db.query_row(
        "SELECT members,my_name FROM groups WHERE account=?1 AND room=?2",
        params![session.account, room],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut members: Vec<String> = serde_json::from_str(&raw)?;
    members.push(own(&session.account));
    let mut data = vec![p::uint(1, members.len() as u64)];
    let mut fields = vec![
        p::bytes(1, &p::uint(1, 0)),
        p::bytes(2, room.as_bytes()),
        p::uint(3, 1),
        p::bytes(5, own(&session.account).as_bytes()),
        p::uint(6, members.len() as u64),
    ];
    for member in members {
        data.push(p::bytes(
            2,
            &p::message(&[
                p::bytes(1, member.as_bytes()),
                p::bytes(2, label(&member).as_bytes()),
                p::bytes(
                    3,
                    if member == own(&session.account) {
                        my_name.as_bytes()
                    } else {
                        b""
                    },
                ),
                p::uint(6, 0),
            ])?,
        ));
        fields.push(p::bytes(7, &p::bytes(1, member.as_bytes())));
    }
    fields.push(p::bytes(4, &p::message(&data)?));
    p::message(&fields)
}
pub(super) fn detail(db: &Connection, session: &Session, room: &str) -> Result<Vec<u8>> {
    check_session(db, session)?;
    validate_destination(db, &session.account, room)?;
    let announcement: String = db.query_row(
        "SELECT announcement FROM groups WHERE account=?1 AND room=?2",
        params![session.account, room],
        |r| r.get(0),
    )?;
    p::message(&[
        p::bytes(1, &p::int32(1, 0)),
        p::bytes(2, announcement.as_bytes()),
        p::uint(3, 1),
        p::bytes(4, own(&session.account).as_bytes()),
        p::uint(5, clock(db)?.into()),
    ])
}
