use super::*;
use rusqlite::ffi;
use std::{
    ffi::{c_char, c_int, c_void},
    fs, ptr,
};

type Emit = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, c_int, c_int) -> c_int;

unsafe extern "C" fn fixture_create(
    _context: *mut c_void,
    _args: *mut *const c_char,
    _count: c_int,
    out: *mut *mut ffi::Fts5Tokenizer,
) -> c_int {
    *out = Box::into_raw(Box::new(0u8)).cast();
    ffi::SQLITE_OK
}

unsafe extern "C" fn fixture_delete(tokenizer: *mut ffi::Fts5Tokenizer) {
    drop(Box::from_raw(tokenizer.cast::<u8>()));
}

unsafe extern "C" fn fixture_tokens(
    _tokenizer: *mut ffi::Fts5Tokenizer,
    context: *mut c_void,
    flags: c_int,
    text: *const c_char,
    length: c_int,
    emit: Option<Emit>,
) -> c_int {
    let emit = emit.unwrap();
    let text = if length == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(text.cast::<u8>(), length as usize)
    };
    if flags & 1 != 0 {
        return emit(context, 0, text.as_ptr().cast(), length, 0, length);
    }
    if text == b"many positions" {
        for position in 0..130 {
            let result = emit(context, 0, c"shared".as_ptr(), 6, 0, length);
            if result != ffi::SQLITE_OK {
                return result;
            }
            if position == 64 {
                let result = emit(context, 1, "雪".as_ptr().cast(), 3, 0, length);
                if result != ffi::SQLITE_OK {
                    return result;
                }
            }
        }
        return ffi::SQLITE_OK;
    }
    // Independently chosen fixture terms deliberately differ from the source
    // text. The Chinese term and its Latin synonym occupy one position.
    let terms: &[(&[u8], i32)] = match text {
        b"target body" => &[
            ("雪".as_bytes(), 0),
            (b"xue", 1),
            (b"shared", 0),
            (b"shared", 0),
        ],
        b"keep body" => &[(b"shared", 0), (b"keep", 0)],
        b"" => &[],
        _ => return ffi::SQLITE_ERROR,
    };
    for (bytes, colocated) in terms {
        let result = emit(
            context,
            *colocated,
            bytes.as_ptr().cast(),
            bytes.len() as i32,
            0,
            length,
        );
        if result != ffi::SQLITE_OK {
            return result;
        }
    }
    ffi::SQLITE_OK
}

fn fixture_tokenizer(connection: &Connection) {
    unsafe {
        tokenizer::install(
            connection,
            ffi::fts5_tokenizer {
                xCreate: Some(fixture_create),
                xDelete: Some(fixture_delete),
                xTokenize: Some(fixture_tokens),
            },
            ptr::null_mut(),
            None,
        )
        .unwrap();
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    location: ProfileLocation,
    keys: KeyFile,
    options: WriteOptions,
    source: SourceMessageId,
}

impl Fixture {
    fn new(encrypted: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("account/db_storage/message");
        fs::create_dir_all(&storage).unwrap();
        let mut keys = KeyFile {
            account_binding: "account".into(),
            databases: BTreeMap::new(),
            image_dat: None,
        };
        let table = format!("Msg_{:x}", Md5::digest(b"peer"));
        for basename in ["message_0.db", DATABASE] {
            let path = storage.join(basename);
            let connection = Connection::open(&path).unwrap();
            if encrypted {
                connection
                    .execute_batch(&format!(
                        "PRAGMA key=\"x'{}'\"; PRAGMA cipher_compatibility=4;",
                        hex::encode([0x31u8; 32])
                    ))
                    .unwrap();
            }
            if basename == "message_0.db" {
                connection
                    .execute_batch(
                        &include_str!("../layout/conversation.sql")
                            .replace("__MESSAGE_TABLE__", &table),
                    )
                    .unwrap();
                connection.execute(&format!("INSERT INTO {table}(local_id,server_id,create_time,sort_seq,local_type,status) VALUES(7,99,100,100001,1,2)"), []).unwrap();
            } else {
                fixture_tokenizer(&connection);
                connection.execute_batch("CREATE TABLE name2id(username TEXT PRIMARY KEY); INSERT INTO name2id(rowid,username)VALUES(1,'peer'),(2,'other'); CREATE TABLE untouched(value TEXT); INSERT INTO untouched VALUES('keep');").unwrap();
                for partition in 0..4 {
                    connection
                        .execute_batch(&format!("{}; {};", main_sql(partition), aux_sql(partition)))
                        .unwrap();
                    let main = format!("message_fts_v4_{partition}");
                    let aux = format!("message_fts_v4_aux_{partition}");
                    for (id, body, local, sort, session, time) in [
                        (1, "target body", 7, 100001, 1, 100),
                        (2, "keep body", 8, 100002, 1, 100),
                        (3, "keep body", 7, 100001, 2, 100),
                        (4, "keep body", 7, 100003, 1, 101),
                    ] {
                        connection.execute(&format!("INSERT INTO {main}(rowid,acontent,message_local_id,sort_seq,local_type,session_id,sender_id,create_time) VALUES(?1,?2,?3,?4,1,?5,1,?6)"),params![id,body,local,sort,session,time]).unwrap();
                        connection.execute(&format!("INSERT INTO {aux}(message_local_id,sort_seq,session_id) VALUES(?1,?2,?3)"),params![local,sort,session]).unwrap();
                    }
                }
                connection.execute_batch("CREATE TABLE ImgFtsAux0V0(acontent TEXT); INSERT INTO ImgFtsAux0V0(rowid,acontent)VALUES(1,'keep body'); CREATE VIRTUAL TABLE ImgFts0V0 USING fts5(acontent,content='ImgFtsAux0V0',tokenize='MMFtsTokenizer disable_pinyin'); INSERT INTO ImgFts0V0(rowid,acontent)VALUES(1,'keep body');").unwrap();
            }
            drop(connection);
            if encrypted {
                let salt = fs::read(&path).unwrap()[..16].try_into().unwrap();
                let relative = format!("message/{basename}");
                keys.databases.insert(
                    relative.clone(),
                    DatabaseKey::new(relative, salt, [0x31; 32]).unwrap(),
                );
            }
        }
        Self {
            location: ProfileLocation::new(directory.path().join("account")).unwrap(),
            keys,
            options: WriteOptions {
                exclusive_ownership_confirmed: true,
                allow_plaintext: !encrypted,
                durable_intent_id: "synthetic-recall-search".into(),
            },
            source: SourceMessageId {
                database: "message/message_0.db".into(),
                table,
                local_id: "7".into(),
            },
            _directory: directory,
        }
    }

    fn search(&self) -> Connection {
        let connection = open_write(
            &self.location,
            "message/message_fts.db",
            &self.keys,
            self.options.allow_plaintext,
        )
        .unwrap();
        fixture_tokenizer(&connection);
        connection
    }

    fn apply(&self) -> Result<()> {
        remove_recalled_search(
            &self.location,
            &self.keys,
            "peer",
            "99",
            &self.source,
            100,
            &self.options,
        )
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(self.location.storage.join("message/message_fts.db")).unwrap()
    }
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn vocabulary(connection: &Connection, partition: usize) -> Vec<(Vec<u8>, i64, i64)> {
    connection.execute_batch(&format!("CREATE VIRTUAL TABLE temp.check_terms USING fts5vocab(main,message_fts_v4_{partition},instance)")).unwrap();
    let result = connection
        .prepare(
            "SELECT CAST(term AS BLOB),doc,offset FROM temp.check_terms ORDER BY term,doc,offset",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    connection
        .execute_batch("DROP TABLE temp.check_terms")
        .unwrap();
    result
}

#[test]
fn encrypted_and_plain_cleanup_removes_real_postings_and_preserves_other_documents() {
    for encrypted in [false, true] {
        let fixture = Fixture::new(encrypted);
        let before = fixture.search();
        let survivors = (0..4)
            .map(|partition| {
                vocabulary(&before, partition)
                    .into_iter()
                    .filter(|row| row.1 != 1)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        drop(before);
        fixture.apply().unwrap();
        let after = fixture.search();
        for (partition, expected) in survivors.iter().enumerate() {
            assert_eq!(&vocabulary(&after, partition), expected);
            assert_eq!(count(&after,&format!("SELECT count(*) FROM message_fts_v4_{partition} WHERE message_fts_v4_{partition} MATCH '雪'")),0);
            assert_eq!(count(&after,&format!("SELECT count(*) FROM message_fts_v4_{partition} WHERE message_fts_v4_{partition} MATCH 'xue'")),0);
            assert_eq!(count(&after,&format!("SELECT count(*) FROM message_fts_v4_{partition} WHERE message_fts_v4_{partition} MATCH 'shared'")),3);
            assert_eq!(
                count(
                    &after,
                    &format!("SELECT count(*) FROM message_fts_v4_aux_{partition}")
                ),
                3
            );
            after.execute_batch(&format!("INSERT INTO message_fts_v4_{partition}(message_fts_v4_{partition})VALUES('integrity-check')")).unwrap();
        }
        assert_eq!(
            count(
                &after,
                "SELECT count(*) FROM ImgFts0V0 WHERE ImgFts0V0 MATCH 'shared'"
            ),
            1
        );
        assert_eq!(
            count(&after, "SELECT count(*) FROM untouched WHERE value='keep'"),
            1
        );
        drop(after);
        let before_retry = fixture.bytes();
        fixture.apply().unwrap();
        assert_eq!(fixture.bytes(), before_retry);
    }
}

#[test]
fn bad_schema_or_cross_shard_identity_never_mutates_the_index() {
    for case in [
        "extra_aux_column",
        "unknown_partition",
        "trigger",
        "second_message_shard",
        "wrong_time",
        "wrong_docsize",
        "orphan_aux",
        "orphan_main",
        "aux_rowid_mismatch",
        "duplicate_document",
        "external_content_layout",
        "unknown_shard_name",
    ] {
        let fixture = Fixture::new(false);
        if matches!(case, "second_message_shard" | "unknown_shard_name") {
            let name = if case == "second_message_shard" {
                "message_1.db"
            } else {
                "message_future.db"
            };
            Connection::open(fixture.location.storage.join(format!("message/{name}")))
                .unwrap()
                .execute_batch("CREATE TABLE opaque(value TEXT);")
                .unwrap();
        } else {
            let connection = fixture.search();
            match case {
                "extra_aux_column" => connection.execute_batch("ALTER TABLE message_fts_v4_aux_0 ADD COLUMN extra TEXT;").unwrap(),
                "unknown_partition" => connection.execute_batch("CREATE TABLE message_fts_v5_0(opaque TEXT);").unwrap(),
                "trigger" => connection.execute_batch("CREATE TRIGGER unexpected_aux_delete AFTER DELETE ON message_fts_v4_aux_0 BEGIN DELETE FROM untouched; END;").unwrap(),
                "wrong_time" => {connection.execute("UPDATE message_fts_v4_3 SET create_time=101 WHERE rowid=1",[]).unwrap();},
                // Synthetic corruption must be rejected rather than repaired.
                "wrong_docsize" => {connection.execute("UPDATE message_fts_v4_0_docsize SET sz=X'02000000000000' WHERE id=1",[]).unwrap();},
                "orphan_aux" => {connection.execute("DELETE FROM message_fts_v4_0 WHERE rowid=1",[]).unwrap();},
                "orphan_main" => {connection.execute("DELETE FROM message_fts_v4_aux_0 WHERE rowid=1",[]).unwrap();},
                "aux_rowid_mismatch" => {connection.execute("UPDATE message_fts_v4_aux_0 SET rowid=55 WHERE rowid=1",[]).unwrap();},
                "duplicate_document" => {connection.execute("INSERT INTO message_fts_v4_0(rowid,acontent,message_local_id,sort_seq,local_type,session_id,sender_id,create_time) VALUES(55,'target body',7,100001,1,1,1,100)",[]).unwrap();},
                "external_content_layout" => connection.execute_batch("DROP TABLE message_fts_v4_0; DROP TABLE message_fts_v4_aux_0; CREATE TABLE message_fts_v4_aux_0(acontent TEXT,message_local_id INTEGER,sort_seq INTEGER,local_type INTEGER,session_id INTEGER,sender_id INTEGER,create_time INTEGER,CONSTRAINT sessionId_localId_createtime PRIMARY KEY(session_id,message_local_id,sort_seq)); CREATE VIRTUAL TABLE message_fts_v4_0 USING fts5(tokenize='MMFtsTokenizer disable_pinyin',content='message_fts_v4_aux_0',acontent,message_local_id UNINDEXED,sort_seq UNINDEXED,local_type UNINDEXED,session_id UNINDEXED,sender_id UNINDEXED,create_time UNINDEXED);").unwrap(),
                _ => unreachable!(),
            }
        }
        let before = fixture.bytes();
        assert!(fixture.apply().is_err(), "{case}");
        assert_eq!(fixture.bytes(), before, "{case}");
    }
}

#[test]
fn empty_indexed_content_is_deleted_without_inventing_tokens() {
    let fixture = Fixture::new(true);
    let connection = fixture.search();
    for partition in 0..4 {
        connection
            .execute(
                &format!("UPDATE message_fts_v4_{partition} SET acontent=NULL WHERE rowid=1"),
                [],
            )
            .unwrap();
    }
    drop(connection);
    fixture.apply().unwrap();
    let connection = fixture.search();
    for partition in 0..4 {
        assert_eq!(
            count(
                &connection,
                &format!("SELECT count(*) FROM message_fts_v4_{partition} WHERE rowid=1")
            ),
            0
        );
        connection.execute_batch(&format!("INSERT INTO message_fts_v4_{partition}(message_fts_v4_{partition})VALUES('integrity-check')")).unwrap();
    }
}

#[test]
fn multibyte_docsize_positions_survive_engine_delete_and_integrity_check() {
    let fixture = Fixture::new(true);
    let connection = fixture.search();
    for partition in 0..4 {
        connection
            .execute(
                &format!(
                    "UPDATE message_fts_v4_{partition} SET acontent='many positions' WHERE rowid=1"
                ),
                [],
            )
            .unwrap();
        let terms = vocabulary(&connection, partition);
        let target = terms.iter().filter(|term| term.1 == 1).collect::<Vec<_>>();
        assert_eq!(target.len(), 131);
        assert_eq!(target.iter().map(|term| term.2).max(), Some(129));
        let size: Vec<u8> = connection
            .query_row(
                &format!("SELECT sz FROM message_fts_v4_{partition}_docsize WHERE id=1"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Genuine FTS5 insertion encoded 130 positions, including a colocated
        // synonym, as a two-byte count followed by six unindexed-column zeros.
        assert_eq!(size, [0x81, 0x02, 0, 0, 0, 0, 0, 0]);
    }
    drop(connection);
    fixture.apply().unwrap();
    let connection = fixture.search();
    for partition in 0..4 {
        assert!(vocabulary(&connection, partition)
            .iter()
            .all(|term| term.1 != 1));
        assert_eq!(count(&connection, &format!("SELECT count(*) FROM message_fts_v4_{partition} WHERE message_fts_v4_{partition} MATCH 'shared'")), 3);
        connection.execute_batch(&format!("INSERT INTO message_fts_v4_{partition}(message_fts_v4_{partition})VALUES('integrity-check')")).unwrap();
    }
}

#[test]
fn absent_database_is_a_noop_and_wrong_target_is_rejected() {
    let fixture = Fixture::new(false);
    for server in ["98", "099"] {
        assert!(remove_recalled_search(
            &fixture.location,
            &fixture.keys,
            "peer",
            server,
            &fixture.source,
            100,
            &fixture.options
        )
        .is_err());
    }
    assert!(remove_recalled_search(
        &fixture.location,
        &fixture.keys,
        "peer",
        "99",
        &fixture.source,
        101,
        &fixture.options
    )
    .is_err());
    let before = fixture.bytes();
    let mut options = fixture.options.clone();
    options.exclusive_ownership_confirmed = false;
    assert!(remove_recalled_search(
        &fixture.location,
        &fixture.keys,
        "peer",
        "99",
        &fixture.source,
        100,
        &options
    )
    .is_err());
    assert_eq!(fixture.bytes(), before);
    fs::rename(
        fixture.location.storage.join("message/message_fts.db"),
        fixture
            .location
            .storage
            .join("message/search-preserved.bin"),
    )
    .unwrap();
    fixture.apply().unwrap();
}

#[test]
fn replay_gate_rejects_unarmed_or_wrong_content_and_transaction_rollback_recovers() {
    let fixture = Fixture::new(false);
    let mut connection = open_write(
        &fixture.location,
        "message/message_fts.db",
        &fixture.keys,
        true,
    )
    .unwrap();
    let gate = tokenizer::register(&connection).unwrap();
    assert!(connection
        .execute("DELETE FROM message_fts_v4_0 WHERE rowid=1", [])
        .is_err());
    gate.arm(b"different text".to_vec(), vec![]).unwrap();
    assert!(connection
        .execute("DELETE FROM message_fts_v4_0 WHERE rowid=1", [])
        .is_err());
    assert!(gate.disarm().is_err());
    {
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch("CREATE VIRTUAL TABLE temp.recall_terms USING fts5vocab(main,message_fts_v4_0,instance)").unwrap();
        let tokens = frozen_tokens(&transaction, "message_fts_v4_0", 1).unwrap();
        transaction
            .execute_batch("DROP TABLE temp.recall_terms")
            .unwrap();
        delete_document(
            &transaction,
            &gate,
            Document {
                table: "message_fts_v4_0".into(),
                rowid: 1,
                content: b"target body".to_vec(),
                tokens,
            },
        )
        .unwrap();
        // Dropping the uncommitted transaction simulates interrupted local work.
    }
    drop(connection);
    let check = fixture.search();
    assert_eq!(
        count(
            &check,
            "SELECT count(*) FROM message_fts_v4_0 WHERE message_fts_v4_0 MATCH '雪'"
        ),
        1
    );
    check
        .execute_batch("INSERT INTO message_fts_v4_0(message_fts_v4_0)VALUES('integrity-check')")
        .unwrap();
    drop(check);
    fixture.apply().unwrap();
}
