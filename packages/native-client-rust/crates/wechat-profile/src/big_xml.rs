//! Narrow, journal-owned in-place expansion for subtype-40 ("BigXml") app messages.
use crate::{
    projection::{check_known_table, ownership},
    schema::quote_identifier,
    writer::{find_database, open_write},
    *,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, OptionalExtension};

const BIG_XML_LOCAL_TYPE: i64 = (40i64 << 32) | 49;

/// Replacement payload or terminal failure status for an unexpanded subtype-40 app message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppMessageExpansion {
    /// Replace the subtype-40 stub with its downloaded inner application-message XML.
    Expanded {
        /// Positive inner application-message subtype (must not be `40`).
        subtype: u32,
        /// Complete official `message_content` XML, including `<sender>:\n` for incoming group rows.
        content: String,
        /// Summary text written to `SessionTable` when this row is still the thread's latest message.
        summary: String,
    },
    /// Record terminal download failure (`download_status = 3`) while keeping the stub intact.
    Failed,
}

/// Update an existing subtype-40 message row in place after BigXml download completes or fails.
///
/// Touches only the matching row in `message_0.db` (`local_type`, `download_status`,
/// `message_content`, `WCDB_CT_message_content`), and updates `session.db` (`SessionTable.summary`
/// and `last_msg_sub_type`) when an expanded row is still the thread's latest message.
///
/// Returns `Ok(None)` without modifying anything if the row does not exist or is no longer a
/// subtype-40 stub (for example if it was already expanded or recalled). Because `message_0.db`
/// and `session.db` commit in separate SQLite transactions, callers must hold exclusive profile
/// ownership and retain a durable operational receipt for replay.
pub fn apply_app_message_expansion(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server_id: &str,
    update: &AppMessageExpansion,
    options: &WriteOptions,
) -> Result<Option<SourceMessageId>> {
    ownership(location, keys, options)?;
    if let AppMessageExpansion::Expanded {
        subtype,
        content,
        summary,
    } = update
    {
        if *subtype == 0
            || *subtype == 40
            || *subtype > i32::MAX as u32
            || content.is_empty()
            || content.len() > 1024 * 1024
            || content.contains('\0')
            || summary.len() > 1024 * 1024
            || summary.contains('\0')
        {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let Some(source) = find_projected_message(location, keys, peer, server_id, options)? else {
        return Ok(None);
    };
    let sid = server_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let local = source
        .local_id
        .parse::<i64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let mut messages = open_write(location, &source.database, keys, options.allow_plaintext)?;
    crate::writer::exact_columns(
        &messages,
        &source.database,
        &source.table,
        crate::writer::MESSAGE_COLUMNS,
    )?;
    let table = quote_identifier(&source.table);
    let existing: Option<(Option<i64>, Option<i64>)> = messages
        .query_row(
            &format!(
                "SELECT local_type,create_time FROM {table} WHERE local_id=?1 AND server_id=?2"
            ),
            params![local, sid as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(existing_type), created)) = existing else {
        return Ok(None);
    };
    match update {
        AppMessageExpansion::Expanded {
            subtype,
            content,
            summary,
        } => {
            let new_local_type = ((i64::from(*subtype)) << 32) | 49;
            if existing_type != BIG_XML_LOCAL_TYPE && existing_type != new_local_type {
                return Ok(None);
            }
            let session_db = find_database(location, "session.db")?;
            let mut sessions = open_write(location, &session_db, keys, options.allow_plaintext)?;
            check_known_table(&sessions, "SessionTable")?;
            let message_updated = if existing_type == BIG_XML_LOCAL_TYPE {
                let compressed = match compressed_profile_field(content.as_bytes())? {
                    SqlValue::Blob(encoded) => STANDARD
                        .decode(encoded)
                        .map_err(|_| ProfileError::InvalidMetadata)?,
                    _ => return Err(ProfileError::InvalidMetadata),
                };
                let tx = messages.transaction()?;
                let updated = tx.execute(
                    &format!(
                        "UPDATE {table} SET local_type=?3,download_status=2,message_content=?4,WCDB_CT_message_content=4 WHERE local_id=?1 AND server_id=?2 AND local_type=?5"
                    ),
                    params![local, sid as i64, new_local_type, compressed, BIG_XML_LOCAL_TYPE],
                )?;
                tx.commit()?;
                updated > 0
            } else {
                false
            };
            let session_updated = if let Some(created) = created {
                let tx = sessions.transaction()?;
                let updated = tx.execute(
                    "UPDATE SessionTable SET summary=?4,last_msg_sub_type=?5 WHERE username=?1 AND last_msg_locald_id=?2 AND last_timestamp=?3 AND last_msg_type=49 AND last_msg_sub_type=40",
                    params![peer, local, created, summary, i64::from(*subtype)],
                )?;
                tx.commit()?;
                updated > 0
            } else {
                false
            };
            Ok((message_updated || session_updated).then_some(source))
        }
        AppMessageExpansion::Failed => {
            if existing_type != BIG_XML_LOCAL_TYPE {
                return Ok(None);
            }
            let tx = messages.transaction()?;
            let updated = tx.execute(
                &format!(
                    "UPDATE {table} SET download_status=3 WHERE local_id=?1 AND server_id=?2 AND local_type=?3"
                ),
                params![local, sid as i64, BIG_XML_LOCAL_TYPE],
            )?;
            tx.commit()?;
            Ok((updated > 0).then_some(source))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, ProfileLocation, KeyFile, WriteOptions) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profile");
        let (location, keys, _) = initialize_profile(&root, &[7; 32]).unwrap();
        let options = WriteOptions {
            allow_plaintext: false,
            durable_intent_id: "intent-bigxml-test".into(),
            exclusive_ownership_confirmed: true,
        };
        (dir, location, keys, options)
    }

    fn insert_stub(
        location: &ProfileLocation,
        keys: &KeyFile,
        options: &WriteOptions,
        peer: &str,
        server_id: &str,
        stub_xml: &str,
    ) -> SourceMessageId {
        let mut fields = RawFields::new();
        for (k, v) in [
            ("local_type", BIG_XML_LOCAL_TYPE),
            ("sort_seq", 1700000001000),
            ("create_time", 1700000001),
            ("status", 3),
            ("upload_status", 0),
            ("download_status", 0),
            ("server_seq", 10),
            ("origin_source", 2),
            ("WCDB_CT_message_content", 4),
            ("WCDB_CT_source", 4),
        ] {
            fields.insert(k.into(), SqlValue::Integer(v.to_string()));
        }
        fields.insert(
            "message_content".into(),
            compressed_profile_field(stub_xml.as_bytes()).unwrap(),
        );
        fields.insert(
            "source".into(),
            compressed_profile_field(b"<msgsource/>").unwrap(),
        );
        fields.insert("compress_content".into(), SqlValue::Text(String::new()));
        let applied = apply_message_projection(
            location,
            keys,
            &MessageProjection {
                peer: peer.into(),
                sender: "wxid_sender".into(),
                from: peer.into(),
                to: "wxid_self".into(),
                server_message_id: server_id.into(),
                fields,
                packed_other_fields: vec![88, 0],
            },
            options,
        )
        .unwrap();
        apply_message_session_projection(
            location,
            keys,
            &MessageSessionProjection {
                source: applied.source_id.clone(),
                peer: peer.into(),
                summary: "Chat History".into(),
                outgoing: false,
                unread: true,
                sender: Some("wxid_sender".into()),
                sender_display: None,
                created_by_this_intent: true,
            },
            options,
        )
        .unwrap();
        applied.source_id
    }

    #[test]
    fn expands_subtype_40_row_and_session_and_is_idempotent() {
        let (_dir, location, keys, options) = setup();
        let peer = "room@chatroom";
        let stub = "wxid_sender:\n<msg><appmsg><title>Chat History</title><type>40</type><realinnertype>19</realinnertype></appmsg></msg>";
        let source = insert_stub(&location, &keys, &options, peer, "9001", stub);
        let full = "wxid_sender:\n<msg><appmsg><title>Chat History</title><type>19</type><recorditem>&lt;recordinfo&gt;&lt;datalist&gt;&lt;dataitem datatype='1'&gt;&lt;datadesc&gt;hello&lt;/datadesc&gt;&lt;/dataitem&gt;&lt;/datalist&gt;&lt;/recordinfo&gt;</recorditem></appmsg></msg>";

        let applied = apply_app_message_expansion(
            &location,
            &keys,
            peer,
            "9001",
            &AppMessageExpansion::Expanded {
                subtype: 19,
                content: full.into(),
                summary: "Chat History".into(),
            },
            &options,
        )
        .unwrap();
        assert_eq!(applied, Some(source.clone()));

        // Replay against an already-expanded row is a clean no-op.
        let again = apply_app_message_expansion(
            &location,
            &keys,
            peer,
            "9001",
            &AppMessageExpansion::Expanded {
                subtype: 19,
                content: full.into(),
                summary: "Chat History".into(),
            },
            &options,
        )
        .unwrap();
        assert_eq!(again, None);

        let reader = ProfileReader::open(
            location.clone(),
            keys.clone(),
            ReadOptions {
                allow_plaintext: false,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        let message = reader.message(&source, Some("wxid_self")).unwrap();
        assert_eq!(message.local_type, ((19u64 << 32) | 49).to_string());
        assert_eq!(message.content.text.as_deref(), Some(full));
        assert_eq!(
            message.raw_fields.get("download_status"),
            Some(&SqlValue::Integer("2".into()))
        );
        let thread = reader.threads(&ListQuery::default()).unwrap().items;
        let session = thread.iter().find(|t| t.username == peer).unwrap();
        assert_eq!(
            session.raw_fields.get("last_msg_sub_type"),
            Some(&SqlValue::Integer("19".into()))
        );
    }

    #[test]
    fn marks_failed_status_and_never_overwrites_recalled_row() {
        let (_dir, location, keys, options) = setup();
        let peer = "wxid_peer";
        let stub = "<msg><appmsg><title>Chat History</title><type>40</type><realinnertype>19</realinnertype></appmsg></msg>";
        let source = insert_stub(&location, &keys, &options, peer, "9002", stub);

        assert_eq!(
            apply_app_message_expansion(
                &location,
                &keys,
                peer,
                "9002",
                &AppMessageExpansion::Failed,
                &options,
            )
            .unwrap(),
            Some(source.clone())
        );

        apply_recall_projection(&location, &keys, peer, "9002", Some(1700000050), &options)
            .unwrap();
        let after_recall = apply_app_message_expansion(
            &location,
            &keys,
            peer,
            "9002",
            &AppMessageExpansion::Expanded {
                subtype: 19,
                content: "<msg><appmsg><type>19</type></appmsg></msg>".into(),
                summary: "Chat History".into(),
            },
            &options,
        )
        .unwrap();
        assert_eq!(after_recall, None);

        let reader = ProfileReader::open(
            location.clone(),
            keys.clone(),
            ReadOptions {
                allow_plaintext: false,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        let message = reader.message(&source, Some("wxid_self")).unwrap();
        assert_eq!(message.local_type, "10000");
    }

    #[test]
    fn repairs_session_sub_type_after_partial_message_only_expansion() {
        let (_dir, location, keys, options) = setup();
        let peer = "wxid_peer_partial";
        let stub = "<msg><appmsg><title>Stub Title</title><type>40</type><realinnertype>19</realinnertype></appmsg></msg>";
        let source = insert_stub(&location, &keys, &options, peer, "9003", stub);
        let full =
            "<msg><appmsg><title>Expanded Title</title><type>19</type><recorditem/></appmsg></msg>";

        // Simulate crash between message_0.db commit and session.db commit by updating only Msg_*.
        let messages =
            open_write(&location, &source.database, &keys, options.allow_plaintext).unwrap();
        let table = quote_identifier(&source.table);
        let new_local_type = (19i64 << 32) | 49;
        messages
            .execute(
                &format!("UPDATE {table} SET local_type=?1,download_status=2 WHERE server_id=9003"),
                params![new_local_type],
            )
            .unwrap();
        drop(messages);

        let repaired = apply_app_message_expansion(
            &location,
            &keys,
            peer,
            "9003",
            &AppMessageExpansion::Expanded {
                subtype: 19,
                content: full.into(),
                summary: "Expanded Title".into(),
            },
            &options,
        )
        .unwrap();
        assert_eq!(repaired, Some(source));

        let reader = ProfileReader::open(
            location,
            keys,
            ReadOptions {
                allow_plaintext: false,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        let threads = reader.threads(&ListQuery::default()).unwrap().items;
        let session = threads.iter().find(|t| t.username == peer).unwrap();
        assert_eq!(
            session.raw_fields.get("last_msg_sub_type"),
            Some(&SqlValue::Integer("19".into()))
        );
        assert_eq!(
            session.raw_fields.get("summary"),
            Some(&SqlValue::Text("Expanded Title".into()))
        );
    }
}
