use crate::{CatalogQuery, Core, Store};
use rusqlite::{types::Value as SqlValue, Connection};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Arc};

struct Fixture {
    directory: tempfile::TempDir,
    core: Arc<Core>,
}

fn database(profile: &Path, relative: &str) -> Connection {
    let connection = Connection::open(profile.join("db_storage").join(relative)).unwrap();
    let password = [42u8; 32];
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(connection.handle(), password.as_ptr().cast(), 32) },
        0
    );
    connection
        .execute_batch(
            "PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;",
        )
        .unwrap();
    connection
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = directory.path().join("account");
    let (_, keys, _) = wechat_profile::initialize_profile(&profile, &[42; 32]).unwrap();
    database(&profile, "contact/contact.db").execute_batch(
        "INSERT INTO name2id(rowid,username) VALUES(1,'filehelper'),(2,'gh_synthetic'),(3,'work@openim'),(4,'member'),(5,'room@chatroom'),(6,'empty@chatroom'),(7,'support@kefu.openim');
         INSERT INTO contact(id,username,nick_name,flag,local_type,delete_flag,verify_flag,is_in_chat_room,extra_buffer) VALUES
         (1,'filehelper','File Transfer',1,1,0,8,0,X'6801'),
         (2,'gh_synthetic','Business',1,1,0,8,0,X'6800'),
         (3,'work@openim','Work',1,5,0,0,0,X'0A0161120177'),
         (4,'member','Member',1,1,0,0,0,X''),
         (5,'room@chatroom','Group',2,2,0,0,1,X''),
         (6,'empty@chatroom','Empty Group',2,2,0,0,1,X''),
         (7,'support@kefu.openim','Support',1,7,0,0,0,X'0A0161120177');
         INSERT INTO biz_info(id,username,type,accept_type,child_type) VALUES(1,'filehelper',1,0,0),(2,'gh_synthetic',0,0,0);
         INSERT INTO chat_room(id,username,owner,ext_buffer) VALUES(5,'room@chatroom','member',X''),(6,'empty@chatroom','member',X'');
         INSERT INTO chatroom_member(room_id,member_id) VALUES(5,2),(5,3),(5,4),(5,7);
         INSERT INTO openim_wording(lang_id,app_id,wording_id,wording) VALUES(1,'a','w','Company'),(2,'a','w','公司');
         INSERT INTO openim_appid(lang_id,app_id,acct_type_id) VALUES(1,'a','type'),(2,'a','type');
         INSERT INTO openim_acct_type(lang_id,acc_type_id) VALUES(1,'type');"
    ).unwrap();
    database(&profile, "session/session.db").execute_batch(
        "INSERT INTO SessionTable(username,unread_count,is_hidden,sort_timestamp,last_timestamp) VALUES('filehelper',0,0,100,100),('gh_synthetic',0,0,101,101),('work@openim',0,0,102,102),('member',0,0,103,103),('room@chatroom',0,0,104,104),('support@kefu.openim',0,0,105,105);"
    ).unwrap();
    let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
    store.attach("one", &profile, true).unwrap();
    let key_path = directory.path().join("keys.json");
    keys.write_new(&key_path).unwrap();
    store.set_keys_file("one", &key_path).unwrap();
    let core = Core::new(store, None).unwrap();
    assert!(core.owns_profile("one").unwrap());
    Fixture { directory, core }
}

fn classifications(page: &Value) -> BTreeMap<String, Value> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["id"].as_str().unwrap().to_owned(),
                item["classification"].clone(),
            )
        })
        .collect()
}

fn saved_rows(connection: &Connection, table: &str) -> Vec<Vec<SqlValue>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn encrypted_catalog_and_library_reads_expose_the_same_classification_without_writing_state() {
    let Fixture { directory, core } = fixture();
    let profile = directory.path().join("account");
    let contact = database(&profile, "contact/contact.db");
    let session = database(&profile, "session/session.db");
    let contacts_before = saved_rows(&contact, "contact");
    let business_before = saved_rows(&contact, "biz_info");
    let sessions_before = saved_rows(&session, "SessionTable");
    let list_query = wechat_profile::ListQuery {
        limit: Some(200),
        ..Default::default()
    };
    let catalog_query = CatalogQuery {
        limit: Some(200),
        ..Default::default()
    };
    let threads = classifications(
        &core
            .catalog_page("one", "threads", None, &catalog_query)
            .unwrap(),
    );
    let contacts = classifications(
        &core
            .catalog_page("one", "contacts", None, &catalog_query)
            .unwrap(),
    );
    let groups = classifications(
        &core
            .catalog_page("one", "groups", None, &catalog_query)
            .unwrap(),
    );
    let members = classifications(
        &core
            .catalog_page("one", "members", Some("room@chatroom"), &catalog_query)
            .unwrap(),
    );
    assert_eq!(
        threads,
        classifications(&core.threads("one", &list_query).unwrap())
    );
    assert_eq!(
        contacts,
        classifications(&core.contacts("one", &list_query).unwrap())
    );
    assert_eq!(
        groups,
        classifications(&core.groups("one", &list_query).unwrap())
    );
    assert_eq!(
        members,
        classifications(
            &core
                .group_members("one", "room@chatroom", &list_query)
                .unwrap()
        )
    );
    for (kind, items) in [
        ("threads", &threads),
        ("contacts", &contacts),
        ("groups", &groups),
    ] {
        for (id, classification) in items {
            assert_eq!(
                &core.catalog_item("one", kind, id).unwrap()["classification"],
                classification
            );
        }
    }
    for (id, classification) in &members {
        assert_eq!(
            &core.group_member("one", "room@chatroom", id).unwrap()["member"]["classification"],
            classification
        );
        assert_eq!(&contacts[id], classification);
        assert_eq!(&threads[id], classification);
    }
    assert_eq!(threads["filehelper"]["category"], "system");
    assert_eq!(threads["gh_synthetic"]["category"], "business");
    assert_eq!(threads["gh_synthetic"]["businessType"], 0);
    assert_eq!(threads["room@chatroom"]["category"], "group");
    assert_eq!(threads["member"]["category"], "unknown");
    assert_eq!(threads["work@openim"]["category"], "openim");
    assert_eq!(
        threads["support@kefu.openim"]["category"],
        "openim_customer_service"
    );
    assert_eq!(
        threads["work@openim"]["openIm"]["wordings"],
        json!([
            {"languageId":1,"text":"Company"},{"languageId":2,"text":"公司"}
        ])
    );
    assert_eq!(
        threads["work@openim"]["openIm"]["applications"],
        json!([
            {"languageId":1,"accountTypeId":"type","accountTypeCached":true},
            {"languageId":2,"accountTypeId":"type","accountTypeCached":false}
        ])
    );
    assert_eq!(groups["empty@chatroom"]["category"], "group");
    assert!(!threads.contains_key("empty@chatroom"));
    assert!(core
        .catalog_item("one", "threads", "empty@chatroom")
        .is_err());
    assert!(core
        .group_members("one", "empty@chatroom", &list_query)
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(saved_rows(&contact, "contact"), contacts_before);
    assert_eq!(saved_rows(&contact, "biz_info"), business_before);
    assert_eq!(saved_rows(&session, "SessionTable"), sessions_before);
}

#[test]
fn current_metadata_changes_are_visible_and_continuation_tombstones_omit_classification() {
    let Fixture { directory, core } = fixture();
    let profile = directory.path().join("account");
    let contact = database(&profile, "contact/contact.db");
    let before = core.catalog_item("one", "threads", "gh_synthetic").unwrap();
    assert_eq!(before["classification"]["businessType"], 0);
    assert_eq!(
        core.catalog_item("one", "contacts", "work@openim").unwrap()["classification"]["openIm"]
            ["wordings"][0]["text"],
        "Company"
    );
    contact.execute_batch("UPDATE contact SET extra_buffer=X'6801' WHERE username='gh_synthetic'; UPDATE biz_info SET type=1 WHERE username='gh_synthetic'; UPDATE openim_wording SET wording='Updated company' WHERE app_id='a' AND wording_id='w' AND lang_id=1;").unwrap();
    let saved_contact = saved_rows(&contact, "contact");
    let saved_business = saved_rows(&contact, "biz_info");
    for kind in ["threads", "contacts"] {
        assert_eq!(
            core.catalog_item("one", kind, "gh_synthetic").unwrap()["classification"]
                ["businessType"],
            1
        );
    }
    assert_eq!(
        classifications(&core.threads("one", &Default::default()).unwrap())["gh_synthetic"]
            ["businessType"],
        1
    );
    assert_eq!(
        classifications(&core.contacts("one", &Default::default()).unwrap())["work@openim"]
            ["openIm"]["wordings"][0]["text"],
        "Updated company"
    );
    assert_eq!(saved_rows(&contact, "contact"), saved_contact);
    assert_eq!(saved_rows(&contact, "biz_info"), saved_business);
    let first = core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(first["items"][0]["id"], "filehelper");
    let cursor = first["nextCursor"].as_str().unwrap().to_owned();
    contact.execute_batch("DELETE FROM contact WHERE username='gh_synthetic'; DELETE FROM biz_info WHERE username='gh_synthetic'").unwrap();
    let second = core
        .catalog_page(
            "one",
            "contacts",
            None,
            &CatalogQuery {
                limit: Some(1),
                cursor: Some(cursor),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(second["items"][0]["id"], "gh_synthetic");
    assert_eq!(second["items"][0]["deleted"], true);
    assert!(second["items"][0].get("classification").is_none());
    assert_eq!(
        core.catalog_item("one", "threads", "gh_synthetic").unwrap()["classification"]["category"],
        "unknown"
    );
}
