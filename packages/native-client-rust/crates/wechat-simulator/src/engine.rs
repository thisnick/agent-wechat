use crate::contract::*;
use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::Path, sync::Mutex};
use wechat_protocol::{
    protobuf as p,
    wire::{Heartbeat, SyncBatch, SyncCommand},
    AccountState, DeviceProfile, LoginStatus,
};

const INITIAL_TIME: u32 = 1_700_000_000;
const PAGE_SIZE: usize = 32;
const SERVER_ID_BASE: u64 = 9_007_199_254_740_993;

#[path = "group_engine.rs"]
mod groups;
#[path = "media_engine.rs"]
mod media;

/// This database is synthetic remote state, never the hub's registry/profile.
pub struct Engine {
    db: Mutex<Connection>,
}
impl Engine {
    pub fn open(path: &Path) -> Result<Self> {
        wechat_platform::private_dir(
            path.parent()
                .context("simulator_database_parent_required")?,
        )?;
        let file = wechat_platform::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(wechat_platform::NOFOLLOW)
            .open(path)?;
        ensure!(
            wechat_platform::private_file(&file)?,
            "simulator_database_must_be_private"
        );
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            INSERT OR IGNORE INTO meta VALUES('clock',1700000000);
            INSERT OR IGNORE INTO meta VALUES('challenge',0);
            CREATE TABLE IF NOT EXISTS accounts(
                id TEXT PRIMARY KEY, generation INTEGER NOT NULL, revoked INTEGER NOT NULL,
                fault TEXT NOT NULL, notification INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS challenges(
                id TEXT PRIMARY KEY, account TEXT NOT NULL, device TEXT NOT NULL,
                computer TEXT NOT NULL, status TEXT NOT NULL, expires INTEGER NOT NULL,
                saved INTEGER NOT NULL, authorization TEXT);
            CREATE TABLE IF NOT EXISTS changes(
                account TEXT NOT NULL, sequence INTEGER NOT NULL, command TEXT NOT NULL,
                PRIMARY KEY(account,sequence));
            CREATE TABLE IF NOT EXISTS messages(
                account TEXT NOT NULL, sequence INTEGER NOT NULL, server_id TEXT NOT NULL,
                command TEXT NOT NULL, PRIMARY KEY(account,sequence), UNIQUE(account,server_id));
            CREATE TABLE IF NOT EXISTS sends(
                account TEXT NOT NULL, client_id INTEGER NOT NULL, hash TEXT NOT NULL,
                server_id TEXT, accepted INTEGER NOT NULL, PRIMARY KEY(account,client_id));
            CREATE TABLE IF NOT EXISTS media_objects(account TEXT NOT NULL,id TEXT NOT NULL,quality INTEGER NOT NULL,metadata TEXT NOT NULL,encrypted BLOB NOT NULL,PRIMARY KEY(account,id,quality));
            CREATE TABLE IF NOT EXISTS media_sends(account TEXT NOT NULL,client_id TEXT NOT NULL,hash TEXT NOT NULL,server_id TEXT,accepted INTEGER NOT NULL,PRIMARY KEY(account,client_id));
            CREATE TABLE IF NOT EXISTS voice_objects(account TEXT NOT NULL,server_id TEXT NOT NULL,data BLOB NOT NULL,PRIMARY KEY(account,server_id));
            CREATE TABLE IF NOT EXISTS read_receipts(account TEXT NOT NULL,client_id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(account,client_id));
            CREATE TABLE IF NOT EXISTS group_creations(account TEXT NOT NULL,key TEXT NOT NULL,hash TEXT NOT NULL,room TEXT NOT NULL,name TEXT NOT NULL,members TEXT NOT NULL,PRIMARY KEY(account,key),UNIQUE(account,room));",
        )?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS groups(account TEXT NOT NULL,room TEXT NOT NULL,name TEXT NOT NULL,members TEXT NOT NULL,my_name TEXT NOT NULL DEFAULT '',muted INTEGER NOT NULL DEFAULT 0,joined INTEGER NOT NULL DEFAULT 1,announcement TEXT NOT NULL DEFAULT '',PRIMARY KEY(account,room));
        CREATE TABLE IF NOT EXISTS group_faults(account TEXT PRIMARY KEY, fault TEXT NOT NULL);
        INSERT OR IGNORE INTO groups(account,room,name,members) SELECT account,room,name,members FROM group_creations;")?;
        // Upgrade only synthetic fixture snapshots from the earlier UI preview.
        // Existing messages, group identities and membership stay intact.
        if db
            .query_row(
                "SELECT value FROM meta WHERE key='group-fixtures'",
                [],
                |r| r.get::<_, u32>(0),
            )
            .optional()?
            != Some(2)
        {
            let accounts = db
                .prepare("SELECT id FROM accounts")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for account in accounts {
                for username in [
                    own(&account),
                    "sim_mei".into(),
                    "sim_daniel".into(),
                    "filehelper".into(),
                ] {
                    append(
                        &db,
                        &account,
                        &SyncCommand {
                            id: 2,
                            payload: groups::contact(&db, &account, &username)?,
                        },
                    )?;
                }
                let rooms = db
                    .prepare("SELECT room FROM groups WHERE account=?1")?
                    .query_map([&account], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for room in rooms {
                    append(
                        &db,
                        &account,
                        &SyncCommand {
                            id: 2,
                            payload: groups::contact(&db, &account, &room)?,
                        },
                    )?;
                }
                notify(&db, &account, 1)?;
            }
            db.execute("INSERT OR REPLACE INTO meta VALUES('group-fixtures',2)", [])?;
        }
        Ok(Self { db: Mutex::new(db) })
    }
    pub(crate) fn request(&self, request: Request) -> Result<Value> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("simulator_unavailable"))?;
        match request {
            Request::GroupMutation { session, request } => Ok(serde_json::to_value(
                groups::mutate(&mut db, &session, &request)?,
            )?),
            Request::GroupDetail { session, room } => {
                Ok(serde_json::to_value(groups::detail(&db, &session, &room)?)?)
            }
            Request::Room { session, room } => {
                Ok(serde_json::to_value(groups::roster(&db, &session, &room)?)?)
            }
            Request::Read {
                session,
                notification,
            } => {
                check_session(&db, &session)?;
                validate_destination(&db, &session.account, &notification.peer_username)?;
                ensure!(
                    notification.client_message_id.len() <= 256
                        && !notification.client_message_id.is_empty()
                        && notification.last_message_create_time > 0,
                    "invalid_read_notification"
                );
                ensure!(
                    db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE account=?1 AND server_id=?2)",
                        params![session.account, notification.last_message_server_id],
                        |r| r.get::<_, bool>(0)
                    )?,
                    "read_boundary_not_found"
                );
                let payload = serde_json::to_string(&notification)?;
                let previous: Option<String> = db
                    .query_row(
                        "SELECT payload FROM read_receipts WHERE account=?1 AND client_id=?2",
                        params![session.account, notification.client_message_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                ensure!(
                    previous.as_ref().is_none_or(|value| value == &payload),
                    "read_identity_conflict"
                );
                db.execute(
                    "INSERT OR IGNORE INTO read_receipts VALUES(?1,?2,?3)",
                    params![session.account, notification.client_message_id, payload],
                )?;
                Ok(json!({"accepted":true}))
            }
            Request::Contacts { session, ids } => {
                check_session(&db, &session)?;
                ensure!(!ids.is_empty() && ids.len() <= 200, "invalid_contact_batch");
                let mut fields = vec![p::bytes(1, &p::uint(1, 0)), p::uint(2, ids.len() as u64)];
                for username in &ids {
                    validate_destination(&db, &session.account, username)?;
                    fields.push(p::bytes(
                        3,
                        &groups::contact(&db, &session.account, username)?,
                    ));
                    fields.push(p::uint(4, 0));
                }
                Ok(serde_json::to_value(p::message(&fields)?)?)
            }
            Request::Avatar { session, username } => {
                check_session(&db, &session)?;
                validate_destination(&db, &session.account, &username)?;
                let color = Sha256::digest(username.as_bytes());
                let image = image::RgbImage::from_fn(64, 64, |x, y| {
                    if (x / 12 + y / 12) % 2 == 0 {
                        image::Rgb([color[0], color[1], color[2]])
                    } else {
                        image::Rgb([color[0] / 2, color[1] / 2, color[2] / 2])
                    }
                });
                let mut bytes = Cursor::new(Vec::new());
                image.write_to(&mut bytes, image::ImageFormat::Png)?;
                Ok(serde_json::to_value(bytes.into_inner())?)
            }
            Request::Media { session, request } => media::request(&mut db, &session, request),
            Request::StartLogin {
                account,
                device,
                computer_name,
                saved,
            } => {
                ensure!(computer_name.len() <= 100, "invalid_computer_name");
                device.validate()?;
                seed(&mut db, &account)?;
                Ok(serde_json::to_value(start_login(
                    &mut db,
                    &account,
                    &device,
                    &computer_name,
                    saved,
                )?)?)
            }
            Request::PollLogin { challenge } => {
                Ok(serde_json::to_value(read_challenge(&db, &challenge)?)?)
            }
            Request::RefreshLogin { challenge } => {
                let (account, device, computer): (String, String, String) = db.query_row(
                    "SELECT account,device,computer FROM challenges WHERE id=?1",
                    [&challenge],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                ensure!(
                    read_challenge(&db, &challenge)?.status == LoginStatus::Expired,
                    "challenge_not_expired"
                );
                Ok(serde_json::to_value(start_login(
                    &mut db,
                    &account,
                    &serde_json::from_str(&device)?,
                    &computer,
                    false,
                )?)?)
            }
            Request::CompleteLogin { challenge } => {
                let current = read_challenge(&db, &challenge)?;
                let (account, device, stored): (String, String, Option<String>) = db.query_row(
                    "SELECT account,device,authorization FROM challenges WHERE id=?1",
                    [&challenge],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                if let Some(stored) = stored {
                    return Ok(serde_json::from_str(&stored)?);
                }
                ensure!(
                    current.status == LoginStatus::Approved,
                    "challenge_not_approved"
                );
                let tx = db.transaction()?;
                tx.execute(
                    "UPDATE accounts SET generation=generation+1,revoked=0 WHERE id=?1",
                    [&account],
                )?;
                let generation: u64 = tx.query_row(
                    "SELECT generation FROM accounts WHERE id=?1",
                    [&account],
                    |r| r.get(0),
                )?;
                let state = authorization(&account, generation, serde_json::from_str(&device)?)?;
                let value = serde_json::to_value(Authorization { state })?;
                tx.execute(
                    "UPDATE challenges SET status='completed',authorization=?2 WHERE id=?1",
                    params![challenge, serde_json::to_string(&value)?],
                )?;
                tx.commit()?;
                Ok(value)
            }
            Request::Logout { session } => {
                check_session(&db, &session)?;
                db.execute(
                    "UPDATE accounts SET revoked=1 WHERE id=?1",
                    [session.account],
                )?;
                Ok(serde_json::to_value(SendReply {
                    accepted: true,
                    server_id: None,
                })?)
            }
            Request::Heartbeat { session, cursor } => {
                check_session(&db, &session)?;
                let sequence = decode_cursor(&cursor)?;
                let head = head(&db, &session.account)?;
                Ok(serde_json::to_value(Heartbeat {
                    next_interval: 30,
                    selector: if head > sequence { 1 } else { 0 },
                })?)
            }
            Request::Sync { session, cursor } => {
                check_session(&db, &session)?;
                let after = decode_cursor(&cursor)?;
                ensure!(
                    after <= head(&db, &session.account)?,
                    "simulator_cursor_ahead"
                );
                let mut q=db.prepare("SELECT sequence,command FROM changes WHERE account=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3")?;
                let rows = q
                    .query_map(params![session.account, after, PAGE_SIZE as u32], |r| {
                        Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut bounded = Vec::new();
                let mut size = 0;
                for row in rows {
                    if !bounded.is_empty() && size + row.1.len() > MAX_BODY / 2 {
                        break;
                    }
                    size += row.1.len();
                    bounded.push(row);
                }
                let next = bounded.last().map_or(after, |r| r.0);
                let more = next < head(&db, &session.account)?;
                let commands = bounded
                    .into_iter()
                    .map(|(_, raw)| serde_json::from_str(&raw))
                    .collect::<serde_json::Result<Vec<SyncCommand>>>()?;
                Ok(serde_json::to_value(SyncBatch {
                    code: 0,
                    flags: if more { 1 } else { 0 },
                    cursor: encode_cursor(next)?,
                    commands,
                    continuation: more,
                })?)
            }
            Request::SendText {
                session,
                destination,
                content,
                created_at,
                client_id,
            } => {
                check_session(&db, &session)?;
                validate_destination(&db, &session.account, &destination)?;
                ensure!(
                    !content.is_empty()
                        && content.len() <= 64 * 1024
                        && created_at > 0
                        && client_id > 0,
                    "invalid_send"
                );
                let hash = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&(
                        destination.as_str(),
                        content.as_str(),
                        created_at
                    ))?)
                );
                let old:Option<(String,Option<String>,bool)>=db.query_row("SELECT hash,server_id,accepted FROM sends WHERE account=?1 AND client_id=?2",
                    params![session.account,client_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                if let Some((old_hash, server_id, accepted)) = old {
                    ensure!(old_hash == hash, "simulator_send_identity_conflict");
                    return Ok(serde_json::to_value(SendReply {
                        accepted,
                        server_id,
                    })?);
                }
                let tx = db.transaction()?;
                let fault: String = tx.query_row(
                    "SELECT fault FROM accounts WHERE id=?1",
                    [&session.account],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "UPDATE accounts SET fault='none' WHERE id=?1",
                    [&session.account],
                )?;
                let accepted = fault != "reject";
                let server_id = if accepted {
                    let (sid, _) = store_message(
                        &tx,
                        &session.account,
                        1,
                        &own(&session.account),
                        &destination,
                        content.as_bytes(),
                        created_at,
                    )?;
                    // A hub-originated send is committed from its receipt. It is
                    // not echoed into this session's sync stream by default.
                    Some(sid)
                } else {
                    None
                };
                tx.execute(
                    "INSERT INTO sends VALUES(?1,?2,?3,?4,?5)",
                    params![session.account, client_id, hash, server_id, accepted],
                )?;
                tx.commit()?;
                if fault == "lost_acknowledgment" {
                    bail!("simulated_lost_acknowledgment");
                }
                Ok(serde_json::to_value(SendReply {
                    accepted,
                    server_id,
                })?)
            }
            Request::Poll { session, after } => {
                check_session(&db, &session)?;
                let sequence: u64 = db.query_row(
                    "SELECT notification FROM accounts WHERE id=?1",
                    [session.account],
                    |r| r.get(0),
                )?;
                Ok(serde_json::to_value(PollReply {
                    sequence,
                    notified: sequence > after,
                })?)
            }
        }
    }
    pub fn control(&self, control: Control) -> Result<Value> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("simulator_unavailable"))?;
        match control {
            Control::GroupFault { account, fault } => {
                seed(&mut db, &account)?;
                db.execute(
                    "INSERT OR REPLACE INTO group_faults VALUES(?1,?2)",
                    params![account, serde_json::to_string(&fault)?],
                )?;
            }

            Control::ReceiveMedia {
                account,
                peer,
                kind,
            } => media::receive(&mut db, &account, &peer, &kind)?,
            Control::Login { account, status } => {
                ensure!(
                    matches!(
                        status,
                        LoginStatus::Pending
                            | LoginStatus::Scanned
                            | LoginStatus::Approved
                            | LoginStatus::Expired
                            | LoginStatus::Rejected
                            | LoginStatus::Cancelled
                    ),
                    "invalid_control_status"
                );
                let id:String=db.query_row("SELECT id FROM challenges WHERE account=?1 AND authorization IS NULL AND status NOT IN ('expired','rejected','cancelled') ORDER BY rowid DESC LIMIT 1",[&account],|r|r.get(0))?;
                ensure!(
                    read_challenge(&db, &id)?.status != LoginStatus::Expired,
                    "challenge_expired"
                );
                let next = serde_json::to_value(status)?
                    .as_str()
                    .context("invalid_control_status")?
                    .to_owned();
                db.execute(
                    "UPDATE challenges SET status=?2 WHERE id=?1",
                    params![id, next],
                )?;
            }
            Control::Advance { seconds } => {
                ensure!(seconds <= 86400, "simulator_clock_step_limit");
                let now = clock(&db)?
                    .checked_add(seconds)
                    .context("simulator_clock_limit")?;
                db.execute("UPDATE meta SET value=?1 WHERE key='clock'", [now])?;
            }
            Control::Inbound {
                account,
                peer,
                body,
                self_sent,
                notify: notification,
            } => {
                validate_peer(&peer)?;
                ensure!(
                    !body.is_empty() && body.len() <= 64 * 1024,
                    "invalid_message"
                );
                seed(&mut db, &account)?;
                let tx = db.transaction()?;
                let time = clock(&tx)?;
                let (from, to) = if self_sent {
                    (own(&account), peer)
                } else {
                    (peer, own(&account))
                };
                let (_, command) =
                    store_message(&tx, &account, 1, &from, &to, body.as_bytes(), time)?;
                append(&tx, &account, &command)?;
                if notification {
                    notify(&tx, &account, 1)?;
                }
                tx.commit()?;
            }
            Control::SendFault { account, fault } => {
                seed(&mut db, &account)?;
                db.execute(
                    "UPDATE accounts SET fault=?2 WHERE id=?1",
                    params![
                        account,
                        serde_json::to_value(fault)?
                            .as_str()
                            .context("invalid_fault")?
                    ],
                )?;
            }
            Control::Revoke { account } => {
                db.execute(
                    "UPDATE accounts SET revoked=1,generation=generation+1 WHERE id=?1",
                    [account],
                )?;
            }
            Control::Notify { account, count } => {
                ensure!((1..=10).contains(&count), "notification_limit");
                notify(&db, &account, count)?;
            }
            Control::UnknownSync { account } => {
                seed(&mut db, &account)?;
                let tx = db.transaction()?;
                append(
                    &tx,
                    &account,
                    &SyncCommand {
                        id: 9999,
                        payload: vec![8, 1],
                    },
                )?;
                notify(&tx, &account, 1)?;
                tx.commit()?;
            }
            Control::EchoSend { account, client_id } => {
                let tx = db.transaction()?;
                let server: String = tx.query_row(
                    "SELECT server_id FROM sends WHERE account=?1 AND client_id=?2 AND accepted=1",
                    params![account, client_id],
                    |r| r.get(0),
                )?;
                let raw: String = tx.query_row(
                    "SELECT command FROM messages WHERE account=?1 AND server_id=?2",
                    params![account, server],
                    |r| r.get(0),
                )?;
                append(&tx, &account, &serde_json::from_str(&raw)?)?;
                notify(&tx, &account, 1)?;
                tx.commit()?;
            }
            Control::StatusNotification {
                account,
                operation_id,
            } => {
                ensure!(
                    [2, 11].contains(&operation_id),
                    "unsupported_simulator_status_operation"
                );
                seed(&mut db, &account)?;
                let tx = db.transaction()?;
                let xml=format!("<msg><op id=\"{operation_id}\"><name>filehelper</name><arg>synthetic</arg></op></msg>");
                let (_, command) = store_message(
                    &tx,
                    &account,
                    51,
                    &own(&account),
                    "filehelper",
                    xml.as_bytes(),
                    clock(&tx)?,
                )?;
                append(&tx, &account, &command)?;
                notify(&tx, &account, 1)?;
                tx.commit()?;
            }
            Control::AdvertiseImage { account, peer } => {
                validate_peer(&peer)?;
                seed(&mut db, &account)?;
                let tx = db.transaction()?;
                let xml=b"<msg><img aeskey=\"00112233445566778899aabbccddeeff\" cdnmidimgurl=\"synthetic-media-reference\" length=\"100\" md5=\"00112233445566778899aabbccddeeff\"/></msg>";
                let (_, command) =
                    store_message(&tx, &account, 3, &peer, &own(&account), xml, clock(&tx)?)?;
                append(&tx, &account, &command)?;
                notify(&tx, &account, 1)?;
                tx.commit()?;
            }
        }
        Ok(json!({"ok":true,"clock":clock(&db)?}))
    }
    pub fn snapshot(&self) -> Result<Value> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("simulator_unavailable"))?;
        let mut q =
            db.prepare("SELECT id,generation,revoked,notification FROM accounts ORDER BY id")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, u64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut accounts = vec![];
        for (id, generation, revoked, notification) in rows {
            let accepted: u64 = db.query_row(
                "SELECT count(*) FROM sends WHERE account=?1 AND accepted=1",
                [&id],
                |r| r.get(0),
            )?;
            let media_accepted: u64 = db.query_row(
                "SELECT count(*) FROM media_sends WHERE account=?1 AND accepted=1",
                [&id],
                |r| r.get(0),
            )?;
            accounts.push(json!({"id":id,"generation":generation,"revoked":revoked,"notifications":notification,"head":head(&db,&id)?,"acceptedSends":accepted,"acceptedMediaSends":media_accepted,"groupCount":db.query_row("SELECT COUNT(*) FROM groups WHERE account=?1",[&id],|r|r.get::<_,u64>(0))?}));
        }
        Ok(json!({"scenario":"basic","clock":clock(&db)?,"accounts":accounts}))
    }
}

fn own(account: &str) -> String {
    format!("sim_{account}")
}
fn validate_account(account: &str) -> Result<()> {
    ensure!(
        !account.is_empty()
            && account.len() <= 32
            && account
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'),
        "invalid_synthetic_account"
    );
    Ok(())
}
fn validate_peer(peer: &str) -> Result<()> {
    ensure!(
        matches!(peer, "sim_mei" | "sim_daniel" | "filehelper"),
        "unknown_synthetic_peer"
    );
    Ok(())
}
fn validate_destination(db: &Connection, account: &str, peer: &str) -> Result<()> {
    if validate_peer(peer).is_ok() {
        return Ok(());
    }
    ensure!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM groups WHERE account=?1 AND room=?2 AND joined=1)",
            params![account, peer],
            |r| r.get::<_, bool>(0)
        )?,
        "unknown_synthetic_peer"
    );
    Ok(())
}
fn clock(db: &Connection) -> Result<u32> {
    Ok(db.query_row("SELECT value FROM meta WHERE key='clock'", [], |r| r.get(0))?)
}
fn head(db: &Connection, account: &str) -> Result<u64> {
    Ok(db.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM changes WHERE account=?1",
        [account],
        |r| r.get(0),
    )?)
}
fn notify(db: &Connection, account: &str, count: u32) -> Result<()> {
    ensure!(
        db.execute(
            "UPDATE accounts SET notification=notification+?2 WHERE id=?1",
            params![account, count]
        )? == 1,
        "unknown_synthetic_account"
    );
    Ok(())
}
fn append(db: &Connection, account: &str, command: &SyncCommand) -> Result<()> {
    let next = head(db, account)? + 1;
    db.execute(
        "INSERT INTO changes VALUES(?1,?2,?3)",
        params![account, next, serde_json::to_string(command)?],
    )?;
    Ok(())
}
fn store_message(
    db: &Connection,
    account: &str,
    kind: u32,
    from: &str,
    to: &str,
    body: &[u8],
    time: u32,
) -> Result<(String, SyncCommand)> {
    let next: u64 = db.query_row(
        "SELECT COALESCE(MAX(sequence),0)+1 FROM messages WHERE account=?1",
        [account],
        |r| r.get(0),
    )?;
    let sid = SERVER_ID_BASE
        .checked_add(next)
        .context("simulator_message_limit")?;
    let command = message(kind, from, to, body, sid, time, next)?;
    let sid = sid.to_string();
    db.execute(
        "INSERT INTO messages VALUES(?1,?2,?3,?4)",
        params![account, next, sid, serde_json::to_string(&command)?],
    )?;
    Ok((sid, command))
}
fn seed(db: &mut Connection, account: &str) -> Result<()> {
    validate_account(account)?;
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
        [account],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let tx = db.transaction()?;
    tx.execute("INSERT INTO accounts VALUES(?1,0,0,'none',1)", [account])?;
    for (id, name) in [
        (own(account), "Alex Morgan"),
        ("sim_mei".into(), "Mei Chen"),
        ("sim_daniel".into(), "Daniel Park"),
        ("filehelper".into(), "File Transfer"),
    ] {
        append(
            &tx,
            account,
            &SyncCommand {
                id: 2,
                payload: p::message(&[
                    p::bytes(1, &p::bytes(1, id.as_bytes())),
                    p::bytes(2, &p::bytes(1, name.as_bytes())),
                    p::uint(7, u32::MAX.into()),
                    p::uint(8, if id == "filehelper" { 0 } else { 1 }),
                ])?,
            },
        )?;
    }
    let (_, command) = store_message(
        &tx,
        account,
        1,
        "sim_mei",
        &own(account),
        b"Synthetic conversation ready.",
        INITIAL_TIME,
    )?;
    append(&tx, account, &command)?;
    tx.commit()?;
    Ok(())
}
fn check_session(db: &Connection, session: &Session) -> Result<()> {
    validate_account(&session.account)?;
    let current: Option<(u64, bool)> = db
        .query_row(
            "SELECT generation,revoked FROM accounts WHERE id=?1",
            [&session.account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    ensure!(
        current == Some((session.generation, false)) && session.generation > 0,
        "auth_required"
    );
    Ok(())
}
fn start_login(
    db: &mut Connection,
    account: &str,
    device: &DeviceProfile,
    computer: &str,
    saved: bool,
) -> Result<Challenge> {
    let tx = db.transaction()?;
    let number: u64 = tx.query_row("SELECT value FROM meta WHERE key='challenge'", [], |r| {
        r.get(0)
    })?;
    tx.execute("UPDATE meta SET value=value+1 WHERE key='challenge'", [])?;
    let id = format!("challenge-{}", number + 1);
    let expires = clock(&tx)?
        .checked_add(120)
        .context("simulator_clock_limit")?;
    // Only the newest active challenge for an account is actionable.
    tx.execute("UPDATE challenges SET status='cancelled' WHERE account=?1 AND authorization IS NULL AND status IN ('pending','scanned','approved')",[account])?;
    tx.execute("INSERT INTO challenges(id,account,device,computer,status,expires,saved) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![id,account,serde_json::to_string(device)?,computer,if saved {"approved"}else{"pending"},expires,saved])?;
    tx.commit()?;
    read_challenge(db, &id)
}
fn read_challenge(db: &Connection, id: &str) -> Result<Challenge> {
    let (status, expires, saved): (String, u32, bool) = db.query_row(
        "SELECT status,expires,saved FROM challenges WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut status: LoginStatus = serde_json::from_value(json!(status))?;
    if (status == LoginStatus::Pending && clock(db)? >= expires)
        || (matches!(status, LoginStatus::Scanned | LoginStatus::Approved)
            && clock(db)? >= expires.saturating_add(780))
    {
        status = LoginStatus::Expired;
    }
    Ok(Challenge {
        id: id.into(),
        qr_png: if saved { vec![] } else { qr_png(id)? },
        expires_at: expires,
        status,
    })
}
fn qr_png(id: &str) -> Result<Vec<u8>> {
    // A valid QR with a non-network synthetic payload, never a WeChat login URL.
    let payload = format!("agentwechat-simulator:login:{id}");
    let qr = qrcodegen::QrCode::encode_text(&payload, qrcodegen::QrCodeEcc::Medium)
        .map_err(|_| anyhow::anyhow!("synthetic_qr_encoding_failed"))?;
    let scale = 6;
    let border = 4;
    let size = (qr.size() + border * 2) as u32 * scale;
    let image = image::RgbImage::from_fn(size, size, |x, y| {
        let dark = qr.get_module(
            x as i32 / scale as i32 - border,
            y as i32 / scale as i32 - border,
        );
        image::Rgb(if dark { [0, 0, 0] } else { [255, 255, 255] })
    });
    let mut bytes = Cursor::new(vec![]);
    image.write_to(&mut bytes, image::ImageFormat::Png)?;
    Ok(bytes.into_inner())
}
fn authorization(account: &str, generation: u64, device: DeviceProfile) -> Result<AccountState> {
    let digest = Sha256::digest(account.as_bytes());
    let key = Sha256::digest(format!("synthetic:{account}:{generation}"));
    let state = AccountState {
        schema: 1,
        device,
        uin: u32::from_le_bytes(digest[..4].try_into()?).max(1),
        own_username: own(account),
        cookie: generation.to_le_bytes().to_vec(),
        session_key: key[..16].to_vec(),
        request_key: key[..24].to_vec(),
        response_key: key[8..].to_vec(),
        auto_auth_key: p::message(&[
            p::bytes(1, &p::buffer(b"synthetic-a")),
            p::bytes(2, &p::buffer(b"synthetic-b")),
        ])?,
        key_info: vec![4; 180],
        database_password: vec![42; 32],
        auth_snapshot: Default::default(),
        long_host: HOST.into(),
        short_host: HOST.into(),
    };
    state.validate()?;
    Ok(state)
}
fn encode_cursor(sequence: u64) -> Result<Vec<u8>> {
    p::message(&[
        p::uint(1, 1),
        p::bytes(2, &p::message(&[p::uint(1, 1), p::uint(2, sequence)])?),
    ])
}
fn decode_cursor(cursor: &[u8]) -> Result<u64> {
    if cursor.is_empty() || cursor == [0] {
        return Ok(0);
    }
    let fields = p::decode(cursor)?;
    if p::scalar(&fields, 1)? == 0 {
        return Ok(0);
    }
    ensure!(p::scalar(&fields, 1)? == 1, "invalid_simulator_cursor");
    let item = p::decode(p::data(&fields, 2)?)?;
    ensure!(p::scalar(&item, 1)? == 1, "invalid_simulator_cursor");
    p::scalar(&item, 2)
}
fn message(
    kind: u32,
    from: &str,
    to: &str,
    body: &[u8],
    sid: u64,
    time: u32,
    sequence: u64,
) -> Result<SyncCommand> {
    Ok(SyncCommand {
        id: 5,
        payload: p::message(&[
            p::uint(1, 1),
            p::bytes(2, &p::bytes(1, from.as_bytes())),
            p::bytes(3, &p::bytes(1, to.as_bytes())),
            p::uint(4, kind.into()),
            p::bytes(5, &p::bytes(1, body)),
            p::uint(6, 0),
            p::uint(7, 0),
            p::bytes(8, &p::buffer(&[])),
            p::uint(9, time.into()),
            p::bytes(10, b"<msgsource/>"),
            p::uint(12, sid),
            p::uint(13, sequence),
        ])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> DeviceProfile {
        DeviceProfile {
            device_id: vec![b'A'; 16],
            device_type: "UnifiedPCLinux x86_64".into(),
            client_version: -227271398,
        }
    }
    fn login(engine: &Engine, account: &str) -> Session {
        let challenge = engine
            .request(Request::StartLogin {
                account: account.into(),
                device: device(),
                computer_name: "Synthetic computer".into(),
                saved: false,
            })
            .unwrap();
        engine
            .control(Control::Login {
                account: account.into(),
                status: LoginStatus::Approved,
            })
            .unwrap();
        let reply = engine
            .request(Request::CompleteLogin {
                challenge: challenge["id"].as_str().unwrap().into(),
            })
            .unwrap();
        let reply: Authorization = serde_json::from_value(reply).unwrap();
        Session {
            account: account.into(),
            generation: u64::from_le_bytes(reply.state.cookie.as_slice().try_into().unwrap()),
        }
    }
    fn send(session: &Session, client_id: u32) -> Request {
        Request::SendText {
            session: Session {
                account: session.account.clone(),
                generation: session.generation,
            },
            destination: "sim_mei".into(),
            content: "Synthetic send".into(),
            created_at: INITIAL_TIME,
            client_id,
        }
    }
    #[test]
    fn synthetic_qr_png_round_trips_through_an_independent_decoder() {
        for id in ["sim-personal-1", "sim-account-with-a-long-name-100"] {
            let png = qr_png(id).unwrap();
            let image = image::load_from_memory(&png).unwrap().to_luma8();
            assert_eq!(image.width(), image.height());
            let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(
                image.width() as usize,
                image.height() as usize,
                |x, y| image.get_pixel(x as u32, y as u32).0[0],
            );
            let grids = prepared.detect_grids();
            assert_eq!(grids.len(), 1);
            assert_eq!(
                grids[0].decode().unwrap().1,
                format!("agentwechat-simulator:login:{id}")
            );
            assert_eq!(image.get_pixel(0, 0).0, [255]);
        }
        assert_ne!(qr_png("first").unwrap(), qr_png("second").unwrap());
    }
    #[test]
    fn remote_acceptance_survives_lost_acknowledgment_and_service_restart() {
        let scratch = wechat_platform::private_tempdir("simulator-persistence-").unwrap();
        let path = scratch.path().join("remote.sqlite");
        let engine = Engine::open(&path).unwrap();
        let session = login(&engine, "personal");
        let initial_head = engine.snapshot().unwrap()["accounts"][0]["head"]
            .as_u64()
            .unwrap();
        engine
            .control(Control::SendFault {
                account: "personal".into(),
                fault: SendFault::LostAcknowledgment,
            })
            .unwrap();
        assert!(engine.request(send(&session, 123)).is_err());
        assert_eq!(
            engine.snapshot().unwrap()["accounts"][0]["acceptedSends"],
            1
        );
        assert_eq!(
            engine.snapshot().unwrap()["accounts"][0]["head"],
            initial_head
        );
        drop(engine);
        let engine = Engine::open(&path).unwrap();
        let replay = engine.request(send(&session, 123)).unwrap();
        assert_eq!(replay["accepted"], true);
        engine
            .control(Control::EchoSend {
                account: "personal".into(),
                client_id: 123,
            })
            .unwrap();
        let echoed = engine
            .request(Request::Sync {
                session: Session {
                    account: session.account.clone(),
                    generation: session.generation,
                },
                cursor: encode_cursor(initial_head).unwrap(),
            })
            .unwrap();
        assert_eq!(echoed["commands"].as_array().unwrap().len(), 1);
        assert_eq!(
            engine.snapshot().unwrap()["accounts"][0]["acceptedSends"],
            1
        );
        engine
            .control(Control::SendFault {
                account: "personal".into(),
                fault: SendFault::Reject,
            })
            .unwrap();
        assert_eq!(
            engine.request(send(&session, 124)).unwrap()["accepted"],
            false
        );
        assert_eq!(
            engine.snapshot().unwrap()["accounts"][0]["acceptedSends"],
            1
        );
    }
    #[test]
    fn named_fixture_ids_timestamps_and_cursors_are_reproducible() {
        let mut snapshots = vec![];
        for _ in 0..2 {
            let scratch = wechat_platform::private_tempdir("simulator-determinism-").unwrap();
            let engine = Engine::open(&scratch.path().join("remote.sqlite")).unwrap();
            let session = login(&engine, "personal");
            let batch = engine
                .request(Request::Sync {
                    session,
                    cursor: vec![],
                })
                .unwrap();
            snapshots.push(batch);
        }
        assert_eq!(snapshots[0], snapshots[1]);
    }
    #[test]
    fn clock_expiry_revocation_and_lost_notifications_do_not_need_wall_clock_sleeps() {
        let scratch = wechat_platform::private_tempdir("simulator-clock-").unwrap();
        let engine = Engine::open(&scratch.path().join("remote.sqlite")).unwrap();
        let challenge = engine
            .request(Request::StartLogin {
                account: "personal".into(),
                device: device(),
                computer_name: "Synthetic computer".into(),
                saved: false,
            })
            .unwrap();
        engine.control(Control::Advance { seconds: 121 }).unwrap();
        assert_eq!(
            engine
                .request(Request::PollLogin {
                    challenge: challenge["id"].as_str().unwrap().into()
                })
                .unwrap()["status"],
            "expired"
        );
        let session = login(&engine, "personal");
        let before = engine.snapshot().unwrap()["accounts"][0]["notifications"].clone();
        engine
            .control(Control::Inbound {
                account: "personal".into(),
                peer: "sim_mei".into(),
                body: "Missed notification".into(),
                self_sent: false,
                notify: false,
            })
            .unwrap();
        assert_eq!(
            engine.snapshot().unwrap()["accounts"][0]["notifications"],
            before
        );
        let batch = engine
            .request(Request::Sync {
                session: Session {
                    account: session.account.clone(),
                    generation: session.generation,
                },
                cursor: vec![],
            })
            .unwrap();
        assert_eq!(batch["commands"].as_array().unwrap().len(), 6);
        engine
            .control(Control::Revoke {
                account: "personal".into(),
            })
            .unwrap();
        assert!(engine
            .request(Request::Heartbeat {
                session,
                cursor: vec![]
            })
            .is_err());
    }
}
