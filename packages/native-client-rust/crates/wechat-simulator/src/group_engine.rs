use super::*;

pub(super) fn create(
    db: &mut Connection,
    session: &Session,
    input: wechat_core::GroupCreateRequest,
    key: &str,
) -> Result<Value> {
    check_session(db, session)?;
    input.validate()?;
    ensure!(
        !key.is_empty() && key.len() <= 256,
        "invalid_group_creation"
    );
    for member in &input.members {
        ensure!(
            ["sim_mei", "sim_daniel"].contains(&member.as_str()),
            "unknown_synthetic_peer"
        );
    }
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&input)?));
    let tx = db.transaction()?;
    let prior: Option<(String, String)> = tx
        .query_row(
            "SELECT hash,room FROM group_creations WHERE account=?1 AND key=?2",
            params![session.account, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((previous, room)) = prior {
        ensure!(previous == hash, "idempotency_conflict");
        return Ok(json!(room));
    }
    let identity = format!("{:x}", Sha256::digest(format!("{}:{key}", session.account)));
    let room = format!("simroom_{}@chatroom", &identity[..20]);
    tx.execute(
        "INSERT INTO group_creations VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            session.account,
            key,
            hash,
            room,
            input.name,
            serde_json::to_string(&input.members)?
        ],
    )?;
    append(
        &tx,
        &session.account,
        &SyncCommand {
            id: 2,
            payload: p::message(&[
                p::bytes(1, &p::bytes(1, room.as_bytes())),
                p::bytes(2, &p::bytes(1, input.name.as_bytes())),
                p::uint(7, 3),
                p::uint(8, 1),
            ])?,
        },
    )?;
    let (_, message) = store_message(
        &tx,
        &session.account,
        10000,
        &room,
        &own(&session.account),
        b"Group created in the simulator.",
        clock(&tx)?,
    )?;
    append(&tx, &session.account, &message)?;
    notify(&tx, &session.account, 1)?;
    tx.commit()?;
    Ok(json!(room))
}
pub(super) fn roster(db: &Connection, session: &Session, room: &str) -> Result<Vec<u8>> {
    check_session(db, session)?;
    let raw: String = db.query_row(
        "SELECT members FROM group_creations WHERE account=?1 AND room=?2",
        params![session.account, room],
        |r| r.get(0),
    )?;
    let mut members: Vec<String> = serde_json::from_str(&raw)?;
    members.push(own(&session.account));
    let mut fields = vec![p::uint(1, members.len() as u64)];
    for member in members {
        let name = match member.as_str() {
            "sim_mei" => "Mei Chen",
            "sim_daniel" => "Daniel Park",
            _ => "Alex Morgan",
        };
        fields.push(p::bytes(
            2,
            &p::message(&[p::bytes(1, member.as_bytes()), p::bytes(2, name.as_bytes())])?,
        ));
    }
    p::message(&[
        p::bytes(1, &p::uint(1, 0)),
        p::bytes(2, room.as_bytes()),
        p::uint(3, 1),
        p::bytes(4, &p::message(&fields)?),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_identity_roster_and_replay_are_account_scoped() {
        let root = wechat_platform::private_tempdir("sim-group-test-").unwrap();
        let engine = Engine::open(&root.path().join("state.sqlite")).unwrap();
        let mut db = engine.db.lock().unwrap();
        seed(&mut db, "a").unwrap();
        seed(&mut db, "b").unwrap();
        db.execute("UPDATE accounts SET generation=1", []).unwrap();
        let first = Session {
            account: "a".into(),
            generation: 1,
        };
        let other = Session {
            account: "b".into(),
            generation: 1,
        };
        let input = wechat_core::GroupCreateRequest {
            name: "Team".into(),
            members: vec!["sim_mei".into(), "sim_daniel".into()],
        };
        let room = create(&mut db, &first, input.clone(), "same").unwrap();
        assert_eq!(
            create(&mut db, &first, input.clone(), "same").unwrap(),
            room
        );
        let mut changed = input;
        changed.name = "Different".into();
        assert!(create(&mut db, &first, changed, "same").is_err());
        let room = room.as_str().unwrap();
        assert!(validate_destination(&db, "a", room).is_ok());
        assert!(validate_destination(&db, "b", room).is_err());
        assert!(roster(&db, &other, room).is_err());
        let decoded =
            wechat_protocol::wire::decode_room(&roster(&db, &first, room).unwrap(), room).unwrap();
        assert_eq!(decoded.members.len(), 3);
        let command: String = db
            .query_row(
                "SELECT command FROM messages WHERE account='a' ORDER BY sequence DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let command: SyncCommand = serde_json::from_str(&command).unwrap();
        let notice =
            wechat_protocol::wire::decode_add_message(&command.payload, &own("a")).unwrap();
        assert_eq!(notice.peer, room);
        assert_eq!(notice.message_type, 10000);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM group_creations", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
}
