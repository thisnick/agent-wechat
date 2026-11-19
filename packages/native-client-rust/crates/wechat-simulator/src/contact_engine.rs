//! Contact edits the hub sends as contact RPCs: the personal remark (operation-log
//! opcode 2 carrying a full `ModContact`) and relationship privacy (CGI 3990).
//! Replies take the shapes `decode_group_mutation` and
//! `decode_contact_privacy_reply` accept. Core keeps admission, receipts and
//! projection; the simulator only stores the result and serves it in lookups.
use super::*;
use wechat_protocol::ContactRpc;

/// Chats Only, Hide My Posts and Hide Their Posts, as contact flag bits.
pub(super) const PRIVACY_MASKS: u64 = 0x800000 | 0x100 | 0x10000;
/// The synthetic friends whose remark and privacy can change.
const FRIENDS: [&str; 2] = ["sim_mei", "sim_daniel"];

fn text(fields: &[p::Field], field: u32) -> Result<String> {
    Ok(std::str::from_utf8(p::data(fields, field)?)?.into())
}
fn sk(fields: &[p::Field], field: u32) -> Result<String> {
    text(&p::decode(p::data(fields, field)?)?, 1)
}
fn friend(username: &str) -> Result<()> {
    ensure!(FRIENDS.contains(&username), "unknown_synthetic_peer");
    Ok(())
}

/// The saved remark and privacy bits for a contact; empty and none by default.
pub(super) fn settings(db: &Connection, account: &str, username: &str) -> Result<(String, u64)> {
    Ok(db
        .query_row(
            "SELECT remark,privacy FROM contact_settings WHERE account=?1 AND username=?2",
            params![account, username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or_default())
}
fn save(db: &Connection, account: &str, username: &str, remark: &str, privacy: u64) -> Result<()> {
    db.execute(
        "INSERT INTO contact_settings VALUES(?1,?2,?3,?4) ON CONFLICT(account,username) DO UPDATE SET remark=excluded.remark,privacy=excluded.privacy",
        params![account, username, remark, privacy],
    )?;
    Ok(())
}

pub(super) fn mutate(db: &mut Connection, session: &Session, rpc: &ContactRpc) -> Result<Vec<u8>> {
    check_session(db, session)?;
    let fields = p::decode(&rpc.body)?;
    let tx = db.transaction()?;
    let reply = match rpc.cgi {
        681 => remark(&tx, &session.account, &fields)?,
        3990 => privacy(&tx, &session.account, &fields)?,
        _ => bail!("unsupported_simulator_contact_operation"),
    };
    tx.commit()?;
    Ok(reply)
}

/// Operation-log opcode 2: a copy of the current record with fields 10–12 replaced.
fn remark(db: &Connection, account: &str, fields: &[p::Field]) -> Result<Vec<u8>> {
    let list = p::decode(p::data(fields, 1)?)?;
    ensure!(
        p::scalar(&list, 1)? == 1,
        "invalid_simulator_contact_request"
    );
    let item = p::decode(p::data(&list, 2)?)?;
    ensure!(
        p::scalar(&item, 1)? == 2,
        "unsupported_simulator_contact_operation"
    );
    let payload = p::decode(&p::unbuffer(p::data(&item, 2)?)?)?;
    let username = sk(&payload, 1)?;
    friend(&username)?;
    for n in 1..=14 {
        ensure!(
            p::optional(&payload, n)?.is_some(),
            "incomplete_simulator_contact"
        );
    }
    // The hub copies the record from a fresh lookup, so everything but the remark matches.
    let current = p::decode(&groups::contact(db, account, &username)?)?;
    ensure!(
        p::scalar(&payload, 7)? == p::scalar(&current, 7)?
            && p::scalar(&payload, 8)? == p::scalar(&current, 8)?,
        "invalid_simulator_contact_flags"
    );
    ensure!(
        sk(&payload, 11)?.is_empty() && sk(&payload, 12)?.is_empty(),
        "invalid_simulator_contact_request"
    );
    let (_, privacy) = settings(db, account, &username)?;
    save(db, account, &username, &sk(&payload, 10)?, privacy)?;
    // No push: the hub confirms the remark with its own exact lookup, as with WeChat.
    p::message(&[
        p::int32(1, 0),
        p::bytes(
            2,
            &p::message(&[p::uint(1, 1), p::bytes(2, &p::varint(0)), p::bytes(3, b"")])?,
        ),
    ])
}

/// CGI 3990: one item per changed mask for a single contact, each echoed with code 0.
fn privacy(db: &Connection, account: &str, fields: &[p::Field]) -> Result<Vec<u8>> {
    let count = usize::try_from(p::scalar(fields, 2)?)?;
    let mut peer: Option<String> = None;
    let mut flags = 0;
    let mut results = Vec::new();
    for field in fields.iter().filter(|field| field.number == 3) {
        let p::Value::Bytes(raw) = &field.value else {
            bail!("invalid_simulator_privacy_request")
        };
        let item = p::decode(raw)?;
        let username = text(&item, 1)?;
        friend(&username)?;
        if peer.is_none() {
            flags = settings(db, account, &username)?.1;
        }
        ensure!(
            peer.get_or_insert_with(|| username.clone()) == &username,
            "invalid_simulator_privacy_request"
        );
        let mask = p::scalar(&item, 2)?;
        ensure!(
            mask.count_ones() == 1 && mask & PRIVACY_MASKS == mask,
            "invalid_simulator_privacy_request"
        );
        flags = match p::scalar(&item, 3)? {
            1 => flags | mask,
            2 => flags & !mask,
            _ => bail!("invalid_simulator_privacy_request"),
        };
        results.push(p::bytes(
            3,
            &p::message(&[p::uint(1, 0), p::bytes(2, raw)])?,
        ));
    }
    ensure!(
        count > 0 && results.len() == count,
        "invalid_simulator_privacy_request"
    );
    let username = peer.context("invalid_simulator_privacy_request")?;
    let (remark, _) = settings(db, account, &username)?;
    save(db, account, &username, &remark, flags)?;
    // WeChat confirms a relationship change with a full contact update.
    append(
        db,
        account,
        &SyncCommand {
            id: 2,
            payload: groups::contact(db, account, &username)?,
        },
    )?;
    notify(db, account, 1)?;
    let mut reply = vec![p::bytes(1, &p::int32(1, 0)), p::uint(2, count as u64)];
    reply.extend(results);
    p::message(&reply)
}
