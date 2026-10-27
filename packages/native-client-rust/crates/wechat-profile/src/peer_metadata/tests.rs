use super::*;
use crate::{KeyFile, ProfileLocation, ReadOptions};
use rusqlite::params;
use std::cell::Cell;

thread_local! {
    pub(super) static QUERIES: Cell<usize> = const { Cell::new(0) };
}

fn fixture() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("account/db_storage/contact/contact.db");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let database = Connection::open(path).unwrap();
    database
        .execute_batch(include_str!("../layout/contact.db.sql"))
        .unwrap();
    (directory, database)
}

fn reader(directory: &tempfile::TempDir) -> ProfileReader {
    ProfileReader::open(
        ProfileLocation::new(directory.path().join("account")).unwrap(),
        KeyFile::empty("account"),
        ReadOptions {
            allow_plaintext: true,
            stopped_or_consistent_snapshot: true,
            ..ReadOptions::default()
        },
    )
    .unwrap()
}

fn insert_contact(database: &Connection, name: &str, local_type: i64, extra: Option<&[u8]>) {
    database.execute("INSERT INTO contact(username,local_type,flag,delete_flag,verify_flag,extra_buffer) VALUES(?1,?2,1,0,0,?3)", params![name,local_type,extra]).unwrap();
}

#[test]
fn service_type_preserves_absent_zero_and_signed_values() {
    // Ordinary contact extras carry field 13 as int32, including its presence.
    assert_eq!(decode_service_type(&[]).unwrap(), None);
    assert_eq!(decode_service_type(&[0x68, 0]).unwrap(), Some(0));
    assert_eq!(decode_service_type(&[0x68, 1]).unwrap(), Some(1));
    assert_eq!(
        decode_service_type(&[0x68, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1])
            .unwrap(),
        Some(-1)
    );
    assert_eq!(decode_service_type(&[0x68, 0, 0x68, 2]).unwrap(), Some(2));
    assert!(decode_service_type(&[0x6a, 0]).is_err());
    assert!(decode_service_type(&[0x68, 0x80]).is_err());
}

#[test]
fn unknown_contact_extra_families_are_not_decoded_as_ordinary_business_data() {
    let (directory, database) = fixture();
    insert_contact(&database, "unknown", 8, Some(&[0x68, 0]));
    let metadata = reader(&directory)
        .peer_metadata(&["unknown".into()])
        .unwrap();
    assert_eq!(metadata["unknown"].contact_service_type, None);
    assert!(metadata["unknown"].openim.is_none());
}

#[test]
fn known_fields_validate_wire_type_and_unknown_fields_are_ignored() {
    // Unknown varint, fixed64, length-delimited, group and fixed32 fields do not
    // replace the two selected strings or the ordinary scalar.
    let bytes = [
        0x08, 0x01, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0x1a, 2, b'x', b'y', 0x23, 0x08, 1, 0x24, 0x2d,
        0, 0, 0, 0, 0x68, 1,
    ];
    assert_eq!(decode_service_type(&bytes).unwrap(), Some(1));
    let item = decode_openim(&[0x0a, 3, b'a', b'p', b'p', 0x12, 2, b'i', b'd', 0x18, 9]).unwrap();
    assert_eq!(item.app_id.as_deref(), Some("app"));
    assert_eq!(item.wording_id.as_deref(), Some("id"));
    assert!(decode_openim(&[0x08, 1]).is_err());
    assert!(decode_openim(&[0x0a, 1, 0xff]).is_err());
    assert!(decode_openim(&[0x0a, 2, b'a']).is_err());
    for invalid in [
        vec![0],
        vec![0x0b],
        vec![0x0c],
        vec![0x0b, 0x14],
        vec![0x0f],
        vec![0x80; 10],
    ] {
        assert!(decode_service_type(&invalid).is_err());
    }
}

#[test]
fn malformed_extra_is_isolated_and_business_data_does_not_default_to_zero() {
    let (directory, database) = fixture();
    insert_contact(&database, "ordinary", 1, None);
    insert_contact(&database, "business", 1, Some(&[0x68, 0]));
    insert_contact(&database, "bad@openim", 5, Some(&[0x0a, 5, b'a']));
    insert_contact(
        &database,
        "good@openim",
        6,
        Some(&[0x0a, 1, b'a', 0x12, 1, b'w']),
    );
    database.execute("INSERT INTO biz_info(username,type,accept_type,child_type) VALUES('business',0,7,NULL)", []).unwrap();
    let result = reader(&directory)
        .peer_metadata(&[
            "ordinary".into(),
            "business".into(),
            "bad@openim".into(),
            "good@openim".into(),
            "missing".into(),
        ])
        .unwrap();
    assert_eq!(result.len(), 4);
    assert!(result["ordinary"].business.is_none());
    assert_eq!(result["ordinary"].contact_service_type, None);
    assert_eq!(result["business"].contact_service_type, Some(0));
    let business = result["business"].business.as_ref().unwrap();
    assert_eq!(business.service_type, Some(0));
    assert_eq!(business.accept_type, Some(7));
    assert_eq!(business.child_type, None);
    assert!(result["bad@openim"].openim.is_none());
    assert_eq!(
        result["good@openim"]
            .openim
            .as_ref()
            .unwrap()
            .wording_id
            .as_deref(),
        Some("w")
    );
}

#[test]
fn catalog_joins_preserve_languages_and_app_identity() {
    let (directory, database) = fixture();
    insert_contact(
        &database,
        "work@openim",
        5,
        Some(&[0x0a, 1, b'a', 0x12, 1, b'w']),
    );
    insert_contact(
        &database,
        "support@kefu.openim",
        7,
        Some(&[0x0a, 1, b'b', 0x12, 1, b'w']),
    );
    database.execute_batch("INSERT INTO openim_wording(lang_id,app_id,wording_id,wording) VALUES(1,'a','w','Company'),(2,'a','w','公司'),(1,'b','w','Support'),(1,'a','unselected','Other'); INSERT INTO openim_appid(lang_id,app_id,acct_type_id) VALUES(1,'a','type'),(2,'a','type'),(1,'b','different'); INSERT INTO openim_acct_type(lang_id,acc_type_id) VALUES(1,'type'),(2,'different');").unwrap();
    let result = reader(&directory)
        .peer_metadata(&["work@openim".into(), "support@kefu.openim".into()])
        .unwrap();
    let work = result["work@openim"].openim.as_ref().unwrap();
    assert_eq!(
        work.wordings,
        vec![
            PeerOpenImWording {
                language_id: 1,
                wording: "Company".into()
            },
            PeerOpenImWording {
                language_id: 2,
                wording: "公司".into()
            },
        ]
    );
    assert_eq!(
        work.applications,
        vec![
            PeerOpenImApplication {
                language_id: 1,
                account_type_id: Some("type".into()),
                account_type_cached: true
            },
            PeerOpenImApplication {
                language_id: 2,
                account_type_id: Some("type".into()),
                account_type_cached: false
            },
        ]
    );
    let support = result["support@kefu.openim"].openim.as_ref().unwrap();
    assert_eq!(support.wordings[0].wording, "Support");
    assert!(!support.applications[0].account_type_cached);
}

#[test]
fn one_full_page_uses_a_fixed_number_of_queries() {
    let (directory, database) = fixture();
    let peers: Vec<_> = (0..200)
        .map(|index| format!("peer_{index}@openim"))
        .collect();
    for peer in &peers {
        insert_contact(&database, peer, 5, Some(&[0x0a, 1, b'a', 0x12, 1, b'w']));
    }
    database.execute_batch("INSERT INTO openim_wording(lang_id,app_id,wording_id,wording) VALUES(1,'a','w','Company'); INSERT INTO openim_appid(lang_id,app_id,acct_type_id) VALUES(1,'a','type'); INSERT INTO openim_acct_type(lang_id,acc_type_id) VALUES(1,'type');").unwrap();
    let reader = reader(&directory);
    QUERIES.with(|count| count.set(0));
    let result = reader.peer_metadata(&peers).unwrap();
    assert_eq!(result.len(), 200);
    assert!(result
        .values()
        .all(|item| item.openim.as_ref().unwrap().wordings.len() == 1));
    QUERIES.with(|count| assert_eq!(count.get(), 4));
    assert!(reader.peer_metadata(&[]).unwrap().is_empty());
    assert!(matches!(
        reader.peer_metadata(&vec!["peer".into(); 201]),
        Err(ProfileError::InvalidQuery)
    ));
    assert!(matches!(
        reader.peer_metadata(&[String::new()]),
        Err(ProfileError::InvalidQuery)
    ));
}

#[test]
fn optional_catalogs_can_be_missing_but_incompatible_tables_are_errors() {
    let (directory, database) = fixture();
    insert_contact(
        &database,
        "work@openim",
        5,
        Some(&[0x0a, 1, b'a', 0x12, 1, b'w']),
    );
    database.execute_batch("DROP TABLE biz_info; DROP TABLE openim_wording; DROP TABLE openim_appid; DROP TABLE openim_acct_type;").unwrap();
    let result = reader(&directory)
        .peer_metadata(&["work@openim".into()])
        .unwrap();
    assert!(result["work@openim"].business.is_none());
    assert!(result["work@openim"]
        .openim
        .as_ref()
        .unwrap()
        .wordings
        .is_empty());
    database
        .execute_batch("CREATE TABLE openim_wording(app_id TEXT)")
        .unwrap();
    assert!(matches!(
        reader(&directory).peer_metadata(&["work@openim".into()]),
        Err(ProfileError::UnsupportedSchema(_))
    ));
}

#[test]
fn duplicate_contact_or_business_records_do_not_choose_an_arbitrary_row() {
    let (directory, database) = fixture();
    insert_contact(&database, "duplicate", 1, None);
    insert_contact(&database, "duplicate", 5, None);
    insert_contact(&database, "business", 1, None);
    database.execute_batch("INSERT INTO biz_info(username,type) VALUES('business',0),('business',1),('business',2)").unwrap();
    let result = reader(&directory)
        .peer_metadata(&["duplicate".into(), "business".into()])
        .unwrap();
    assert!(!result.contains_key("duplicate"));
    assert!(result["business"].business.is_none());
}

#[test]
fn ambiguous_catalog_languages_do_not_choose_an_arbitrary_label() {
    let (directory, database) = fixture();
    insert_contact(
        &database,
        "work@openim",
        5,
        Some(&[0x0a, 1, b'a', 0x12, 1, b'w']),
    );
    database.execute_batch("DROP TABLE openim_wording; CREATE TABLE openim_wording(app_id TEXT,wording_id TEXT,lang_id INTEGER,wording TEXT); INSERT INTO openim_wording VALUES('a','w',1,'One'),('a','w',1,'Other'),('a','w',2,'Unique'); DROP TABLE openim_appid; CREATE TABLE openim_appid(app_id TEXT,lang_id INTEGER,acct_type_id TEXT); INSERT INTO openim_appid VALUES('a',1,'one'),('a',1,'other'),('a',2,'unique');").unwrap();
    let result = reader(&directory)
        .peer_metadata(&["work@openim".into()])
        .unwrap();
    let item = result["work@openim"].openim.as_ref().unwrap();
    assert_eq!(item.wordings.len(), 1);
    assert_eq!(item.wordings[0].language_id, 2);
    assert_eq!(item.wordings[0].wording, "Unique");
    assert_eq!(item.applications.len(), 1);
    assert_eq!(item.applications[0].language_id, 2);
}

#[test]
fn catalog_results_have_a_total_row_bound() {
    let (directory, database) = fixture();
    insert_contact(
        &database,
        "work@openim",
        5,
        Some(&[0x0a, 1, b'a', 0x12, 1, b'w']),
    );
    database.execute_batch("BEGIN").unwrap();
    let mut insert = database.prepare("INSERT INTO openim_wording(lang_id,app_id,wording_id,wording) VALUES(?1,'a','w','Label')").unwrap();
    for language in 0..=MAX_CATALOG_ROWS {
        insert.execute([language]).unwrap();
    }
    drop(insert);
    database.execute_batch("COMMIT").unwrap();
    assert!(matches!(
        reader(&directory).peer_metadata(&["work@openim".into()]),
        Err(ProfileError::ResourceLimit)
    ));
}

#[test]
fn malformed_scalar_and_oversized_extra_stay_unknown() {
    let (directory, database) = fixture();
    let extra = [0x68, 0].repeat(2049);
    insert_contact(&database, "peer", 1, Some(&extra));
    database
        .execute(
            "UPDATE contact SET flag='invalid' WHERE username='peer'",
            [],
        )
        .unwrap();
    let reader = ProfileReader::open(
        ProfileLocation::new(directory.path().join("account")).unwrap(),
        KeyFile::empty("account"),
        ReadOptions {
            allow_plaintext: true,
            stopped_or_consistent_snapshot: true,
            max_blob_bytes: 4096,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    let result = reader.peer_metadata(&["peer".into()]).unwrap();
    assert_eq!(result["peer"].flag, None);
    assert_eq!(result["peer"].contact_service_type, None);
}

#[test]
fn copied_reader_joins_the_captured_metadata_generation() {
    let (directory, database) = fixture();
    insert_contact(&database, "peer", 1, Some(&[0x68, 0]));
    database
        .execute("INSERT INTO biz_info(username,type) VALUES('peer',0)", [])
        .unwrap();
    let captured = reader(&directory);
    database
        .execute_batch("UPDATE contact SET extra_buffer=X'6801'; UPDATE biz_info SET type=1")
        .unwrap();
    let before = captured.peer_metadata(&["peer".into()]).unwrap();
    assert_eq!(before["peer"].contact_service_type, Some(0));
    assert_eq!(
        before["peer"].business.as_ref().unwrap().service_type,
        Some(0)
    );
    let after = reader(&directory).peer_metadata(&["peer".into()]).unwrap();
    assert_eq!(after["peer"].contact_service_type, Some(1));
    assert_eq!(
        after["peer"].business.as_ref().unwrap().service_type,
        Some(1)
    );
}

#[test]
fn owned_reader_reuses_its_existing_read_transaction() {
    let (directory, database) = fixture();
    insert_contact(&database, "peer", 1, Some(&[0x68, 0]));
    drop(database);
    let reader = crate::OwnedReaderCache::default()
        .read(
            ProfileLocation::new(directory.path().join("account")).unwrap(),
            KeyFile::empty("account"),
            ReadOptions {
                allow_plaintext: true,
                stopped_or_consistent_snapshot: true,
                ..ReadOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        reader.peer_metadata(&["peer".into()]).unwrap()["peer"].contact_service_type,
        Some(0)
    );
    assert_eq!(
        reader.peer_metadata(&["peer".into()]).unwrap()["peer"].contact_service_type,
        Some(0)
    );
}
