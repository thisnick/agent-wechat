-- Inspected Linux 4.1.13.26 ordinary schema; no rows or private identifiers.
CREATE TABLE MessageGroupTimeInfo(chatname_id INTEGER, group_id TEXT, create_time INTEGER, initial_sort_seq INTEGER, birth_time INTEGER);
CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY, is_session INTEGER);
CREATE TABLE TimeStamp(timestamp INTEGER);
CREATE TABLE wcdb_builtin_compression_record(tableName TEXT PRIMARY KEY, columns TEXT NOT NULL, rowid INTEGER) WITHOUT ROWID;
CREATE INDEX MessageGroupTimeInfoINDEX_CHAT_GROUP ON MessageGroupTimeInfo(chatname_id, group_id);
