//! Online views retain membership/order independently of mutable official files.
use crate::Core;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use wechat_profile::{ListQuery, MessageQuery, ProfileReader};

#[derive(Clone, Default)]
pub struct CatalogQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub search: Option<String>,
    pub thread_kind: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    view: String,
    query: String,
    last: String,
    expires: i64,
}
#[derive(Serialize, Deserialize)]
struct View {
    ids: Vec<String>,
    metadata: Value,
    // Account-wide message continuations retain each peer even when mutable
    // contact/name tables no longer identify it. Older scoped views omit this.
    #[serde(default)]
    message_threads: BTreeMap<String, String>,
}
const MAX_ITEMS: usize = 100_000;
// Large history exports need a bounded window longer than an interactive read.
const VIEW_SECONDS: i64 = 30 * 60;

impl Core {
    fn pagination_continuation(
        &self,
        account: &str,
        query: &str,
        encoded: &str,
    ) -> Result<(Cursor, View, String, usize)> {
        if encoded.len() > 8192 {
            bail!("invalid_cursor");
        }
        let decode = || -> Result<Cursor> {
            let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
            let plain = self
                .cursor_vault
                .open_record(account, "pagination-cursor", &bytes)?;
            Ok(serde_json::from_slice(&plain)?)
        };
        let cursor = decode().map_err(|_| anyhow::anyhow!("invalid_cursor"))?;
        if cursor.version != 1 || cursor.query != query {
            bail!("invalid_cursor");
        }
        if cursor.expires <= wechat_store::unix_seconds() {
            bail!("pagination_cursor_expired");
        }
        let retained = self.store.pagination_view(account, &cursor.view)?;
        if retained.query_hash != query || retained.expires_at != cursor.expires {
            bail!("invalid_cursor");
        }
        let plain = self.vault.open_record(
            account,
            &format!("pagination-view:{}", cursor.view),
            &retained.payload,
        )?;
        let view: View = serde_json::from_slice(&plain)?;
        let start = view
            .ids
            .iter()
            .position(|id| id == &cursor.last)
            .context("invalid_cursor")?
            + 1;
        Ok((cursor, view, retained.event_cursor, start))
    }

    pub fn catalog_item(&self, account: &str, kind: &str, id: &str) -> Result<Value> {
        if !matches!(kind, "threads" | "contacts" | "groups") || id.is_empty() || id.len() > 512 {
            bail!("invalid_request");
        }
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        self.catalog_rows(account, kind, None, &reader, own.as_deref())?
            .into_iter()
            .find(|r| r["id"] == id)
            .context("no rows")
    }
    /// The account actor must be held around the profile snapshot and boundary.
    pub fn catalog_page(
        &self,
        account: &str,
        kind: &str,
        subject: Option<&str>,
        q: &CatalogQuery,
    ) -> Result<Value> {
        if !matches!(
            kind,
            "threads" | "contacts" | "groups" | "members" | "messages"
        ) || q.search.as_ref().is_some_and(|s| s.len() > 256)
            || q.thread_kind
                .as_ref()
                .is_some_and(|k| !matches!(k.as_str(), "direct" | "group" | "system"))
        {
            bail!("invalid_request");
        }
        let query = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                kind,
                subject,
                &q.search,
                &q.thread_kind
            ))?)
        );
        let limit = q.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            bail!("invalid_request");
        }
        let selected = if kind == "messages" {
            q.cursor
                .as_deref()
                .map(|cursor| {
                    let (_, view, _, start) =
                        self.pagination_continuation(account, &query, cursor)?;
                    view.ids
                        .iter()
                        .skip(start)
                        .take(limit)
                        .map(|id| {
                            let peer = subject
                                .or_else(|| view.message_threads.get(id).map(String::as_str))
                                .context("invalid_cursor")?;
                            Ok((id.clone(), peer.to_owned()))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
        } else {
            None
        };
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        // A continuation resolves retained IDs against the current generation.
        // New arrivals and renames cannot change the original view membership.
        let rows = if let Some(ids) = selected {
            let mut peers = BTreeMap::new();
            let mut sources = Vec::with_capacity(ids.len());
            for (id, peer) in ids {
                let table = format!("Msg_{:x}", md5::Md5::digest(peer.as_bytes()));
                let source = crate::source_message_id(&id)?;
                if !source.table.eq_ignore_ascii_case(&table) {
                    bail!("invalid_cursor");
                }
                // A removed database/table/row is a tombstone. A present but
                // unreadable or incompatible source remains an explicit error.
                let Some(database) = reader
                    .inventory()
                    .iter()
                    .find(|d| d.relative_path == source.database)
                else {
                    continue;
                };
                if database.state == wechat_profile::DatabaseState::Readable
                    && !reader.schemas().iter().any(|s| {
                        s.database == source.database
                            && s.tables.iter().any(|t| t.name == source.table)
                    })
                {
                    continue;
                }
                peers.insert(id, peer);
                sources.push(source);
            }
            let mut rows = Vec::with_capacity(sources.len());
            let mut encoded_bytes = 0usize;
            for mut message in reader.messages_by_ids(&sources, own.as_deref())? {
                // The retained query and table hash establish the peer even
                // after contact/name metadata changes after page one.
                let id = crate::message_id(&message.id)?;
                message.peer = Some(peers.get(&id).context("invalid_cursor")?.clone());
                let value =
                    self.normalize_recalled(account, message, own.as_deref(), reader.generation())?;
                encoded_bytes = encoded_bytes.saturating_add(serde_json::to_vec(&value)?.len());
                if encoded_bytes > 32 * 1024 * 1024 {
                    bail!("pagination_capacity_exceeded");
                }
                rows.push(value);
            }
            rows
        } else {
            self.catalog_rows(account, kind, subject, &reader, own.as_deref())?
        };
        let metadata = json!({"sourceGeneration":reader.generation(),"coverage":reader.coverage(),"order":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()});
        self.paginate_values(account, &query, q, rows, metadata)
    }
    fn paginate_values(
        &self,
        account: &str,
        query: &str,
        q: &CatalogQuery,
        rows: Vec<Value>,
        metadata: Value,
    ) -> Result<Value> {
        let limit = q.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) {
            bail!("invalid_request");
        }
        let current: BTreeMap<String, Value> = rows
            .into_iter()
            .map(|item| {
                Ok((
                    item["id"]
                        .as_str()
                        .context("resource identity missing")?
                        .to_owned(),
                    item,
                ))
            })
            .collect::<Result<_>>()?;
        let (view_id, view, boundary, expires, start) = if let Some(encoded) = &q.cursor {
            let (cursor, view, boundary, start) =
                self.pagination_continuation(account, query, encoded)?;
            (cursor.view, view, boundary, cursor.expires, start)
        } else {
            // Preserve the source ordering explicitly; the current resource map
            // is used only for field resolution, never as the ordering contract.
            let order = metadata["order"]
                .as_array()
                .context("resource ordering missing")?;
            let mut ids = Vec::new();
            let mut seen = BTreeSet::new();
            for id in order {
                let id = id.as_str().context("resource identity missing")?;
                let Some(item) = current.get(id) else {
                    bail!("resource identity missing");
                };
                let search = q.search.as_ref().map(|s| s.to_lowercase());
                let matches_search = search.as_ref().is_none_or(|s| {
                    ["id", "displayName", "nickname", "remark", "alias", "body"]
                        .iter()
                        .any(|key| {
                            item[key]
                                .as_str()
                                .is_some_and(|v| v.to_lowercase().contains(s))
                        })
                });
                let matches_kind = q
                    .thread_kind
                    .as_ref()
                    .is_none_or(|k| item["kind"].as_str() == Some(k.as_str()));
                if matches_search && matches_kind && seen.insert(id.to_owned()) {
                    ids.push(id.to_owned());
                }
                if ids.len() > MAX_ITEMS {
                    bail!("pagination_capacity_exceeded");
                }
            }
            let mut clean_metadata = metadata;
            clean_metadata
                .as_object_mut()
                .context("invalid page metadata")?
                .remove("order");
            let view = View {
                message_threads: ids
                    .iter()
                    .filter_map(|id| {
                        current
                            .get(id)?
                            .get("threadId")?
                            .as_str()
                            .map(|peer| (id.clone(), peer.to_owned()))
                    })
                    .collect(),
                ids,
                metadata: clean_metadata,
            };
            let id = uuid::Uuid::new_v4().to_string();
            let boundary = self.store.event_boundary(account)?;
            let expires = wechat_store::unix_seconds() + VIEW_SECONDS;
            if view.ids.len() > limit {
                let payload = self.vault.seal(
                    account,
                    &format!("pagination-view:{id}"),
                    &serde_json::to_vec(&view)?,
                )?;
                self.store
                    .save_pagination_view(account, &id, query, &boundary, expires, &payload)?;
            }
            (id, view, boundary, expires, 0)
        };
        let end = start.saturating_add(limit).min(view.ids.len());
        let items: Vec<_> = view.ids[start..end]
            .iter()
            .map(|id| {
                current.get(id).cloned().unwrap_or_else(
                    || json!({"id":id,"deleted":true,"version":metadata_version(&view.metadata)}),
                )
            })
            .collect();
        let next = if end < view.ids.len() {
            let cursor = Cursor {
                version: 1,
                view: view_id,
                query: query.into(),
                last: view.ids[end - 1].clone(),
                expires,
            };
            Some(URL_SAFE_NO_PAD.encode(self.cursor_vault.seal(
                account,
                "pagination-cursor",
                &serde_json::to_vec(&cursor)?,
            )?))
        } else {
            None
        };
        let mut result = view.metadata;
        let object = result.as_object_mut().context("invalid page metadata")?;
        object.insert("items".into(), json!(items));
        object.insert("nextCursor".into(), json!(next));
        object.insert("asOfEventCursor".into(), json!(boundary));
        object.insert("viewExpiresAt".into(), json!(expires));
        Ok(result)
    }
    fn catalog_rows(
        &self,
        account: &str,
        kind: &str,
        subject: Option<&str>,
        reader: &ProfileReader,
        own: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut result = Vec::new();
        let mut cursor = None;
        let mut encoded_bytes = 0usize;
        loop {
            let q = ListQuery {
                limit: Some(200),
                cursor: cursor.clone(),
                search: None,
            };
            let (items, next): (Vec<Value>, Option<String>) = match kind {
                "threads" => {
                    let page = reader.threads(&q)?;
                    (page.items.into_iter().map(|t|{let id=t.username;let kind=if id.ends_with("@chatroom"){"group"}else if id=="filehelper"||id.starts_with("weixin"){"system"}else{"direct"};Ok(json!({"id":id,"peerId":id,"accountId":account,"kind":kind,"displayName":t.label,"unreadCount":t.unread_count,"readStateVersion":self.read_state_version(account,&id,reader.generation())?,"history":{"coverage":"partial","localCount":null},"version":reader.generation()}))}).collect::<Result<Vec<_>>>()?,page.next_cursor)
                }
                "contacts" => {
                    let page = reader.contacts(&q)?;
                    (page.items.into_iter().filter(|c|!c.is_group).map(|c|json!({"id":c.username,"username":c.username,"displayName":c.label,"nickname":c.nickname,"remark":c.remark,"alias":c.alias,"relation":"unknown","version":reader.generation()})).collect(),page.next_cursor)
                }
                "groups" => {
                    let page = reader.groups(&q)?;
                    (page.items.into_iter().map(|g|json!({"id":g.username,"roomId":g.username,"displayName":g.label,"ownerId":g.owner,"announcement":g.announcement,"version":reader.generation()})).collect(),page.next_cursor)
                }
                "members" => {
                    let page = reader.group_members(subject.context("room required")?, &q)?;
                    (page.items.into_iter().map(|m|json!({"id":m.username,"username":m.username,"displayName":m.label,"roomDisplayName":m.room_display_name,"version":reader.generation()})).collect(),page.next_cursor)
                }
                "messages" => {
                    let page = reader.messages(&MessageQuery {
                        chat: subject.map(str::to_owned),
                        own_username: own.map(str::to_owned),
                        limit: Some(200),
                        cursor: cursor.clone(),
                        ..Default::default()
                    })?;
                    (
                        page.items
                            .into_iter()
                            .map(|m| self.normalize_recalled(account, m, own, reader.generation()))
                            .collect::<Result<Vec<_>>>()?,
                        page.next_cursor,
                    )
                }
                _ => bail!("invalid_request"),
            };
            for item in items {
                encoded_bytes = encoded_bytes.saturating_add(serde_json::to_vec(&item)?.len());
                if encoded_bytes > 32 * 1024 * 1024 {
                    bail!("pagination_capacity_exceeded");
                }
                result.push(item);
            }
            if result.len() > MAX_ITEMS {
                bail!("pagination_capacity_exceeded");
            }
            match next {
                Some(next) if cursor.as_ref() != Some(&next) => cursor = Some(next),
                Some(_) => bail!("invalid profile continuation"),
                None => break,
            }
        }
        Ok(result)
    }
}
fn metadata_version(metadata: &Value) -> Value {
    metadata
        .get("sourceGeneration")
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn rows(ids: &[(&str, &str)]) -> Vec<Value> {
        ids.iter()
            .map(|(id, name)| json!({"id":id,"displayName":name}))
            .collect()
    }
    fn metadata(ids: &[&str]) -> Value {
        json!({"sourceGeneration":"test","coverage":"partial","order":ids})
    }
    #[test]
    fn paging_retains_membership_order_updates_and_tombstones_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store");
        let core = Core::new(Arc::new(wechat_store::Store::open(&path).unwrap()), None).unwrap();
        core.create_account("a").unwrap();
        let first = core
            .paginate_values(
                "a",
                "query",
                &CatalogQuery {
                    limit: Some(1),
                    search: Some("match".into()),
                    ..Default::default()
                },
                rows(&[("a", "match a"), ("b", "match b"), ("c", "match c")]),
                metadata(&["a", "b", "c"]),
            )
            .unwrap();
        let cursor = first["nextCursor"].as_str().unwrap().to_owned();
        drop(core);
        let core = Core::new(Arc::new(wechat_store::Store::open(&path).unwrap()), None).unwrap();
        let second = core
            .paginate_values(
                "a",
                "query",
                &CatalogQuery {
                    limit: Some(2),
                    cursor: Some(cursor.clone()),
                    search: Some("match".into()),
                    ..Default::default()
                },
                rows(&[("d", "match late old timestamp"), ("b", "renamed")]),
                metadata(&["d", "b"]),
            )
            .unwrap();
        assert_eq!(second["items"][0]["id"], "b");
        assert_eq!(second["items"][0]["displayName"], "renamed");
        assert_eq!(second["items"][1]["id"], "c");
        assert_eq!(second["items"][1]["deleted"], true);
        assert!(second["nextCursor"].is_null());
        assert!(core
            .paginate_values(
                "a",
                "other-query",
                &CatalogQuery {
                    cursor: Some(cursor),
                    ..Default::default()
                },
                rows(&[]),
                metadata(&[])
            )
            .unwrap_err()
            .to_string()
            .contains("invalid_cursor"));
    }
    #[test]
    fn account_message_search_retains_cross_thread_membership_and_peers() {
        use md5::Digest;
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = temp.path().join("fixture");
        let password = [42u8; 32];
        let (_, keys, _) = wechat_profile::initialize_profile(&profile, &password).unwrap();
        let database = profile.join("db_storage/message/message_0.db");
        let db = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
            0
        );
        db.execute_batch(
            "PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;",
        )
        .unwrap();
        for (index, peer) in [(1, "first-peer"), (2, "second@chatroom")] {
            let table = format!("Msg_{:x}", md5::Md5::digest(peer.as_bytes()));
            db.execute(
                "INSERT INTO Name2Id(rowid,user_name,is_session)VALUES(?1,?2,1)",
                rusqlite::params![index, peer],
            )
            .unwrap();
            db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER);")).unwrap();
            db.execute(
                &format!("INSERT INTO {table} VALUES(1,?1,1,?1,1,'match original','',0,0)"),
                [index * 2],
            )
            .unwrap();
        }
        let first_table = format!("Msg_{:x}", md5::Md5::digest(b"first-peer"));
        db.execute_batch(&format!(
            "INSERT INTO {first_table} VALUES(2,3,1,3,1,'match middle','',0,0);"
        ))
        .unwrap();
        let store = Arc::new(wechat_store::Store::open(&temp.path().join("data")).unwrap());
        store.attach("a", &profile, true).unwrap();
        let key_path = temp.path().join("keys.json");
        keys.write_new(&key_path).unwrap();
        store.set_keys_file("a", &key_path).unwrap();
        let core = Core::new(store, None).unwrap();
        core.create_account("b").unwrap();
        let first = core
            .catalog_page(
                "a",
                "messages",
                None,
                &CatalogQuery {
                    limit: Some(1),
                    search: Some("match".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first["items"][0]["threadId"], "second@chatroom");
        assert!(first.get("message_threads").is_none());
        let cursor = first["nextCursor"].as_str().unwrap().to_owned();
        // Retained peers survive name-map removal and a search-matching edit.
        db.execute_batch(&format!("UPDATE {first_table} SET message_content='edited' WHERE local_id=2;DELETE FROM {first_table} WHERE local_id=1;DELETE FROM Name2Id;INSERT INTO {first_table} VALUES(3,99,NULL,99,1,'late malformed row','',0,0);")).unwrap();
        let next = core
            .catalog_page(
                "a",
                "messages",
                None,
                &CatalogQuery {
                    limit: Some(2),
                    search: Some("match".into()),
                    cursor: Some(cursor.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(next["items"][0]["threadId"], "first-peer");
        assert_eq!(next["items"][0]["body"], "edited");
        assert_eq!(next["items"][1]["deleted"], true);
        assert!(next["nextCursor"].is_null());
        for (account, subject, search) in [
            ("a", Some("first-peer"), "match"),
            ("a", None, "changed"),
            ("b", None, "match"),
        ] {
            assert!(core
                .catalog_page(
                    account,
                    "messages",
                    subject,
                    &CatalogQuery {
                        cursor: Some(cursor.clone()),
                        search: Some(search.into()),
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .to_string()
                .contains("invalid_cursor"));
        }
    }

    #[test]
    fn message_continuation_only_resolves_retained_page_rows() {
        use md5::Digest;
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = temp.path().join("fixture");
        let password = [42u8; 32];
        let (_, keys, _) = wechat_profile::initialize_profile(&profile, &password).unwrap();
        let table = format!("Msg_{:x}", md5::Md5::digest(b"peer"));
        let database = profile.join("db_storage/message/message_0.db");
        let mut db = rusqlite::Connection::open(&database).unwrap();
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
            0
        );
        db.execute_batch(&format!("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;INSERT INTO Name2Id(user_name,is_session)VALUES('peer',1);CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER);")).unwrap();
        let tx = db.transaction().unwrap();
        for local in 1..=2048 {
            tx.execute(
                &format!("INSERT INTO {table} VALUES(?1,?1,1,?1,1,'original','',0,0)"),
                [local],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let store =
            std::sync::Arc::new(wechat_store::Store::open(&temp.path().join("data")).unwrap());
        store.attach("a", &profile, true).unwrap();
        let key_path = temp.path().join("keys.json");
        keys.write_new(&key_path).unwrap();
        store.set_keys_file("a", &key_path).unwrap();
        let core = Core::new(store, None).unwrap();
        let first = core
            .catalog_page(
                "a",
                "messages",
                Some("peer"),
                &CatalogQuery {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        let cursor = first["nextCursor"].as_str().unwrap().to_owned();
        // Edits and deletions in the original membership must be reflected;
        // unrelated late rows must never be decoded for this continuation.
        db.execute_batch(&format!("UPDATE {table} SET message_content='edited' WHERE local_id=2047;DELETE FROM {table} WHERE local_id=2046;INSERT INTO {table} VALUES(3000,3000,NULL,0,1,'late malformed row','',0,0);")).unwrap();
        let next = core
            .catalog_page(
                "a",
                "messages",
                Some("peer"),
                &CatalogQuery {
                    limit: Some(2),
                    cursor: Some(cursor.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(next["items"][0]["body"], "edited");
        assert_eq!(next["items"][0]["threadId"], "peer");
        assert_eq!(next["items"][1]["deleted"], true);
        let missing = crate::message_id(&wechat_profile::SourceMessageId {
            database: "message/message_0.db".into(),
            table: table.clone(),
            local_id: "2046".into(),
        })
        .unwrap();
        assert_eq!(next["items"][1]["id"], missing);
        assert_eq!(next["items"].as_array().unwrap().len(), 2);
        assert!(core
            .catalog_page(
                "a",
                "messages",
                Some("another_peer"),
                &CatalogQuery {
                    cursor: Some(cursor),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .to_string()
            .contains("invalid_cursor"));
        db.execute("DELETE FROM Name2Id WHERE user_name='peer'", [])
            .unwrap();
        let later = core
            .catalog_page(
                "a",
                "messages",
                Some("peer"),
                &CatalogQuery {
                    limit: Some(1),
                    cursor: Some(next["nextCursor"].as_str().unwrap().into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(later["items"][0]["threadId"], "peer");
        let mut unavailable = keys.clone();
        let relative = "message/message_0.db";
        let salt = unavailable.databases[relative].salt;
        unavailable.databases.insert(
            relative.into(),
            wechat_profile::DatabaseKey::new(relative.into(), salt, [7; 32]).unwrap(),
        );
        let unavailable_path = temp.path().join("unavailable-keys.json");
        unavailable.write_new(&unavailable_path).unwrap();
        core.store.set_keys_file("a", &unavailable_path).unwrap();
        let error = core
            .catalog_page(
                "a",
                "messages",
                Some("peer"),
                &CatalogQuery {
                    cursor: Some(next["nextCursor"].as_str().unwrap().into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<wechat_profile::ProfileError>(),
            Some(wechat_profile::ProfileError::KeyMismatch(_))
        ));
    }

    #[test]
    fn cursor_rejects_tampering_and_cross_account_use() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::new(
            Arc::new(wechat_store::Store::open(&temp.path().join("store")).unwrap()),
            None,
        )
        .unwrap();
        core.create_account("a").unwrap();
        core.create_account("b").unwrap();
        let page = core
            .paginate_values(
                "a",
                "query",
                &CatalogQuery {
                    limit: Some(1),
                    ..Default::default()
                },
                rows(&[("a", "a"), ("b", "b")]),
                metadata(&["a", "b"]),
            )
            .unwrap();
        let cursor = page["nextCursor"].as_str().unwrap().to_owned();
        assert!(core
            .paginate_values(
                "b",
                "query",
                &CatalogQuery {
                    cursor: Some(cursor.clone()),
                    ..Default::default()
                },
                rows(&[]),
                metadata(&[])
            )
            .is_err());
        let mut bytes = URL_SAFE_NO_PAD.decode(cursor).unwrap();
        bytes[5] ^= 1;
        assert!(core
            .paginate_values(
                "a",
                "query",
                &CatalogQuery {
                    cursor: Some(URL_SAFE_NO_PAD.encode(bytes)),
                    ..Default::default()
                },
                rows(&[]),
                metadata(&[])
            )
            .is_err());
    }
}
