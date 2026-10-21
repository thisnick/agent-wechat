//! Whole-conversation local read projection at a caller's frozen timestamp.
use crate::{
    projection::{check_known_table, identity, ownership},
    writer::{exact_columns, find_database, open_write},
    *,
};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadProjection {
    pub session_present: bool,
    pub removed_unread_rows: usize,
    pub unread_count_before: i64,
    pub unread_count_after: i64,
    pub last_clear_unread_timestamp: i64,
    pub whole_thread: bool,
}

/// Clear this session through a frozen server timestamp, preserving later rows.
/// Unlike an exact-ID projection, this reconciles an older counter/list mismatch
/// when the entire known session is before the boundary. It performs no RPC.
pub fn mark_read_thread(
    location: &ProfileLocation,
    keys: &KeyFile,
    thread: &str,
    through_time: i64,
    options: &WriteOptions,
) -> Result<ThreadReadProjection> {
    ownership(location, keys, options)?;
    identity(thread)?;
    if !u32::try_from(through_time).is_ok_and(|time| time != 0) {
        return Err(ProfileError::InvalidQuery);
    }
    let relative = find_database(location, "session.db")?;
    let mut database = open_write(location, &relative, keys, options.allow_plaintext)?;
    check_known_table(&database, "SessionTable")?;
    check_known_table(&database, "SessionUnreadListTable_1")?;
    exact_columns(&database, &relative, "Name2Id", &["user_name"])?;
    let tx = database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let session = tx
        .query_row(
            "SELECT COALESCE(unread_count,0),COALESCE(last_timestamp,0),COALESCE(last_clear_unread_timestamp,0) FROM SessionTable WHERE username=?1",
            [thread],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        )
        .optional()?;
    let Some((before, last_time, old_clear)) = session else {
        // A status control can refer to a session absent from local history.
        // Do not manufacture a conversation merely to record an empty read.
        return Ok(ThreadReadProjection {
            session_present: false,
            removed_unread_rows: 0,
            unread_count_before: 0,
            unread_count_after: 0,
            last_clear_unread_timestamp: 0,
            whole_thread: true,
        });
    };
    if before < 0 || old_clear < 0 {
        return Err(ProfileError::WritesUnsupported);
    }
    let name_id = tx
        .query_row(
            "SELECT rowid FROM Name2Id WHERE user_name=?1",
            [thread],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let (removed, remaining, first) = if let Some(name_id) = name_id {
        let removed = tx.execute(
            "DELETE FROM SessionUnreadListTable_1 WHERE username_id=?1 AND create_time<=?2",
            params![name_id, through_time],
        )?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM SessionUnreadListTable_1 WHERE username_id=?1",
            [name_id],
            |row| row.get(0),
        )?;
        let first = tx.query_row(
            "SELECT server_id FROM SessionUnreadListTable_1 WHERE username_id=?1 ORDER BY create_time,local_id,server_id LIMIT 1",
            [name_id],
            |row| row.get::<_, i64>(0),
        ).optional()?.unwrap_or(0);
        (removed, remaining, first)
    } else {
        (0, 0, 0)
    };
    // Unknown counter contributions can belong to newer history. Preserve them
    // when the session summary is newer than this read boundary.
    let after = if last_time > through_time {
        before.saturating_sub(removed as i64).max(remaining)
    } else {
        remaining
    };
    let clear = old_clear.max(through_time);
    tx.execute(
        "UPDATE SessionTable SET unread_count=?1,unread_first_msg_srv_id=?2,last_clear_unread_timestamp=?3,unread_first_pat_msg_local_id=CASE WHEN ?1=0 THEN 0 ELSE unread_first_pat_msg_local_id END,unread_first_pat_msg_sort_seq=CASE WHEN ?1=0 THEN 0 ELSE unread_first_pat_msg_sort_seq END WHERE username=?4",
        params![after, first, clear, thread],
    )?;
    tx.commit()?;
    Ok(ThreadReadProjection {
        session_present: true,
        removed_unread_rows: removed,
        unread_count_before: before,
        unread_count_after: after,
        last_clear_unread_timestamp: clear,
        whole_thread: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::{collections::BTreeMap, fs};

    fn fixture(reordered: bool) -> (tempfile::TempDir, ProfileLocation, KeyFile, WriteOptions) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("account/db_storage/session/session.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let sql = include_str!("layout/session.db.sql");
        let sql = if reordered {
            sql.replace(
                "server_id INTEGER, local_id INTEGER, create_time INTEGER",
                "server_id INTEGER, create_time INTEGER, local_id INTEGER",
            )
        } else {
            sql.into()
        };
        let database = Connection::open(path).unwrap();
        database.execute_batch(&sql).unwrap();
        database.execute_batch("INSERT INTO Name2Id(rowid,user_name)VALUES(1,'peer'),(2,'other'); INSERT INTO SessionTable(username,unread_count,unread_first_msg_srv_id,unread_first_pat_msg_local_id,unread_first_pat_msg_sort_seq,last_timestamp,last_clear_unread_timestamp,draft,status)VALUES('peer',9,90,12,13,100,20,'keep draft',7),('other',1,500,0,0,100,0,'other',3); INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(1,90,1,90),(1,80,2,100),(2,500,1,100);").unwrap();
        drop(database);
        let location = ProfileLocation::new(directory.path().join("account")).unwrap();
        let keys = KeyFile {
            account_binding: "account".into(),
            databases: BTreeMap::new(),
            image_dat: None,
        };
        let options = WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: true,
            durable_intent_id: "synthetic-thread-read".into(),
        };
        (directory, location, keys, options)
    }

    #[test]
    fn whole_thread_clears_legacy_counter_and_advances_watermark_in_both_column_orders() {
        for reordered in [false, true] {
            let (_directory, location, keys, options) = fixture(reordered);
            let result = mark_read_thread(&location, &keys, "peer", 100, &options).unwrap();
            assert_eq!(
                (
                    result.removed_unread_rows,
                    result.unread_count_before,
                    result.unread_count_after
                ),
                (2, 9, 0)
            );
            assert_eq!(result.last_clear_unread_timestamp, 100);
            let database = Connection::open(location.storage.join("session/session.db")).unwrap();
            let row = database.query_row("SELECT unread_count,unread_first_msg_srv_id,last_clear_unread_timestamp,unread_first_pat_msg_local_id,unread_first_pat_msg_sort_seq,draft,status FROM SessionTable WHERE username='peer'", [], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?,row.get::<_,i64>(6)?))).unwrap();
            assert_eq!(row, (0, 0, 100, 0, 0, "keep draft".into(), 7));
            assert_eq!(
                database
                    .query_row(
                        "SELECT unread_count FROM SessionTable WHERE username='other'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            assert_eq!(
                database
                    .query_row(
                        "SELECT COUNT(*) FROM SessionUnreadListTable_1 WHERE username_id=2",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            drop(database);
            let again = mark_read_thread(&location, &keys, "peer", 90, &options).unwrap();
            assert_eq!(
                (
                    again.removed_unread_rows,
                    again.unread_count_after,
                    again.last_clear_unread_timestamp
                ),
                (0, 0, 100)
            );
        }
    }

    #[test]
    fn later_arrivals_and_unknown_newer_counter_contributions_survive() {
        let (_directory, location, keys, options) = fixture(false);
        let database = Connection::open(location.storage.join("session/session.db")).unwrap();
        database.execute_batch("UPDATE SessionTable SET last_timestamp=110 WHERE username='peer'; INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(1,3,3,110);").unwrap();
        drop(database);
        let result = mark_read_thread(&location, &keys, "peer", 100, &options).unwrap();
        assert_eq!(
            (result.removed_unread_rows, result.unread_count_after),
            (2, 7)
        );
        let database = Connection::open(location.storage.join("session/session.db")).unwrap();
        assert_eq!(
            database
                .query_row(
                    "SELECT unread_first_msg_srv_id FROM SessionTable WHERE username='peer'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        drop(database);
        let result = mark_read_thread(&location, &keys, "peer", 110, &options).unwrap();
        assert_eq!(result.unread_count_after, 0);
        assert_eq!(result.last_clear_unread_timestamp, 110);
    }

    #[test]
    fn unknown_session_is_empty_and_unsafe_writes_are_rejected() {
        let (_directory, location, keys, mut options) = fixture(false);
        assert!(
            !mark_read_thread(&location, &keys, "absent", 100, &options)
                .unwrap()
                .session_present
        );
        assert!(mark_read_thread(&location, &keys, "peer", 0, &options).is_err());
        assert!(mark_read_thread(&location, &keys, "peer\n", 100, &options).is_err());
        options.exclusive_ownership_confirmed = false;
        assert!(mark_read_thread(&location, &keys, "peer", 100, &options).is_err());
        options.exclusive_ownership_confirmed = true;
        let mut other_keys = keys.clone();
        other_keys.account_binding = "other".into();
        assert!(mark_read_thread(&location, &other_keys, "peer", 100, &options).is_err());
        let database = Connection::open(location.storage.join("session/session.db")).unwrap();
        database
            .execute_batch(
                "CREATE TRIGGER unexpected AFTER UPDATE ON SessionTable BEGIN SELECT 1; END;",
            )
            .unwrap();
        drop(database);
        assert!(mark_read_thread(&location, &keys, "peer", 100, &options).is_err());
    }
}
