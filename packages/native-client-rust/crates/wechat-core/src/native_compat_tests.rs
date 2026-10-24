//! Pins the message JSON that consumers persist at intake.
//!
//! The OpenClaw extension stores each received message as the daemon returned
//! it and later compares that copy with a fresh read before dispatching group
//! context. A projection change therefore breaks messages that were stored by
//! an earlier release. The golden file below is the daemon side of that
//! contract; `packages/openclaw-extension/src/native-consumer-compat.test.ts`
//! replays a consumer file frozen at 1.0.0-beta.13 against it.
//!
//! Regenerate with `UPDATE_NATIVE_COMPAT_FIXTURES=1` only for an intended
//! projection change, then run the extension test: the frozen consumer file
//! must keep dispatching without being regenerated.
use crate::Core;
use md5::Digest;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Arc;

const GOLDEN: &str = "tests/fixtures/native-compat/messages.json";
const LINK: &str = "<msg><appmsg><type>5</type><title>Example Venue</title><des>Reserve a table</des><url>https://example.com/booking?a=1&amp;b=2</url></appmsg></msg>";

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn quote(comment: &str, kind: i32, server: u64, content: &str) -> String {
    format!("<msg><appmsg><type>57</type><title>{comment}</title><refermsg><type>{kind}</type><svrid>{server}</svrid><fromusr>room@chatroom</fromusr><chatusr>sender</chatusr><content>{}</content></refermsg></appmsg></msg>", escape(content))
}

#[test]
fn persisted_message_projection_matches_the_consumer_contract() {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // The key file binds the profile to its directory name, which must match the
    // own username in the identity bound below.
    let profile = temp.path().join("self");
    let password = [42u8; 32];
    let (_, keys, _) = wechat_profile::initialize_profile(&profile, &password).unwrap();
    let db = rusqlite::Connection::open(profile.join("db_storage/message/message_0.db")).unwrap();
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
        0
    );
    db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000; INSERT INTO Name2Id(rowid,user_name,is_session)VALUES(1,'room@chatroom',1),(2,'sender',0);").unwrap();
    let table = format!("Msg_{:x}", md5::Md5::digest(b"room@chatroom"));
    db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER)")).unwrap();
    let image = "<msg><img aeskey='01010101010101010101010101010101' cdnmidimgurl='synthetic-cdn-id' length='123'/></msg>";
    let mention = "<msgsource><atuserlist>self</atuserlist></msgsource>";
    // Local ID n has server ID 1000 + n; "quoteText" quotes "text" (1003).
    let rows: [(&str, i64, String, &str); 10] = [
        (
            "answeredMention",
            1,
            "@Helper when does it open?".into(),
            mention,
        ),
        ("heldMention", 1, "@Helper please book it".into(), mention),
        ("text", 1, "Shall we go on Saturday?".into(), ""),
        (
            "quoteText",
            (57 << 32) | 49,
            quote("Sounds good", 1, 1003, "Shall we go on Saturday?"),
            "",
        ),
        (
            "quoteImage",
            (57 << 32) | 49,
            quote("Is this the place?", 3, 999, &format!("sender:\n{image}")),
            "",
        ),
        (
            "quoteLink",
            (57 << 32) | 49,
            quote("Explain this link", 49, 998, &format!("sender:\n{LINK}")),
            "",
        ),
        ("link", (5 << 32) | 49, LINK.into(), ""),
        (
            "sticker",
            47,
            "<msg><emoji md5='0123456789abcdef0123456789abcdef' len='18727' type='2'/></msg>"
                .into(),
            "",
        ),
        ("image", 3, image.into(), ""),
        ("mention", 1, "@Helper can you check this?".into(), mention),
    ];
    for (index, (_, kind, content, source)) in rows.iter().enumerate() {
        let local = index as i64 + 1;
        db.execute(
            &format!("INSERT INTO {table} VALUES(?1,?2,?3,?4,2,?5,?6,0,0)"),
            rusqlite::params![
                local,
                1000 + local,
                kind,
                1_792_800_000 + local,
                format!("sender:\n{content}"),
                source
            ],
        )
        .unwrap();
    }
    drop(db);
    let store = Arc::new(wechat_store::Store::open(&temp.path().join("data")).unwrap());
    store.attach("a", &profile, true).unwrap();
    let key_path = temp.path().join("keys.json");
    keys.write_new(&key_path).unwrap();
    store.set_keys_file("a", &key_path).unwrap();
    // A bound profile identity supplies the own username, as it does for a
    // deployed account, so direction and self mentions are projected.
    let format_key = [9; 16];
    let pair = wechat_profile::create_mmkv_v4(&format_key, [3; 16], 4096).unwrap();
    let pair = wechat_profile::stage_mmkv_values(
        pair.data(),
        pair.metadata(),
        &format_key,
        &[
            (
                "mmkv_key_user_name".into(),
                Some(wechat_profile::mmkv_string_value(b"self").unwrap().to_vec()),
            ),
            ("mmkv_key_latest_login_uin".into(), Some(vec![1])),
            (
                "key_wechat_device_id".into(),
                Some(
                    wechat_profile::mmkv_string_value(b"synthetic-device")
                        .unwrap()
                        .to_vec(),
                ),
            ),
        ],
    )
    .unwrap();
    let mmkv = profile.join("identity");
    std::fs::write(&mmkv, pair.data()).unwrap();
    std::fs::write(mmkv.with_extension("crc"), pair.metadata()).unwrap();
    let core = Core::new(store, None).unwrap();
    core.bind_profile_identity("a", &mmkv, &format_key).unwrap();

    let mut projected = Map::new();
    for (index, (name, ..)) in rows.iter().enumerate() {
        let id = crate::message_id(&wechat_profile::SourceMessageId {
            database: "message/message_0.db".into(),
            table: table.clone(),
            local_id: (index + 1).to_string(),
        })
        .unwrap();
        let mut message = core.message("a", &id).unwrap();
        // A snapshot generation, not message content; consumers never compare it.
        message.as_object_mut().unwrap().remove("version");
        projected.insert((*name).into(), message);
    }
    let actual = serde_json::to_string_pretty(&Value::Object(projected)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("UPDATE_NATIVE_COMPAT_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert!(
        expected == actual,
        "Message JSON differs from {GOLDEN}. Consumers persist this JSON, so a change can stop \
         them dispatching messages stored by an earlier release. If the change is intended, \
         regenerate with UPDATE_NATIVE_COMPAT_FIXTURES=1 and keep the OpenClaw consumer \
         compatibility test passing without changing its frozen consumer file.\n{actual}"
    );
}
