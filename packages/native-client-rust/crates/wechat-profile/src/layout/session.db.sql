-- Inspected Linux 4.1.13.26 ordinary schema; no rows or private identifiers.
CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY);
CREATE TABLE SessionDeleteTable(username TEXT PRIMARY KEY, delete_time INTEGER);
CREATE TABLE SessionDraft(username TEXT, window_id TEXT, timestamp INTEGER, draft_data BLOB);
CREATE TABLE SessionNoContactInfoTable(username TEXT PRIMARY KEY, session_title TEXT);
CREATE TABLE SessionTable(username TEXT PRIMARY KEY, type INTEGER, unread_count INTEGER, unread_first_msg_srv_id INTEGER, unread_first_pat_msg_local_id INTEGER, unread_first_pat_msg_sort_seq INTEGER, is_hidden INTEGER, summary TEXT, draft TEXT, status INTEGER, last_timestamp INTEGER, sort_timestamp INTEGER, last_clear_unread_timestamp INTEGER, last_msg_locald_id INTEGER, last_msg_type INTEGER, last_msg_sub_type INTEGER, last_msg_sender TEXT, last_sender_display_name TEXT, last_msg_ext_type INTEGER);
CREATE TABLE SessionUnreadListTable_1(username_id INTEGER, server_id INTEGER, local_id INTEGER, create_time INTEGER, CONSTRAINT _NameId_ServerId PRIMARY KEY(username_id, server_id));
CREATE TABLE SessionUnreadStatTable_1(username_id INTEGER, unread_stat INTEGER, CONSTRAINT _NameId PRIMARY KEY(username_id));
CREATE UNIQUE INDEX SessionDraft_SESSION_DRAFT ON SessionDraft(username, window_id);
CREATE INDEX SessionTable_LSENDER ON SessionTable(last_msg_sender);
CREATE INDEX SessionTable_TYPE ON SessionTable(type);
CREATE INDEX SessionUnreadListTable_1_NameId_CreateTime ON SessionUnreadListTable_1(username_id ASC, create_time ASC);
