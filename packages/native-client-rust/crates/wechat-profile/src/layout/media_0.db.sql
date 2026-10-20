-- Inspected Linux 4.1.13.26 ordinary schema; no rows or private identifiers.
CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY);
CREATE TABLE TimeStamp(timestamp INTEGER);
CREATE TABLE VoiceInfo(chat_name_id INTEGER, create_time INTEGER, local_id INTEGER, svr_id INTEGER, voice_data BLOB, data_index TEXT DEFAULT '0');
CREATE INDEX VoiceInfo_INDEX ON VoiceInfo(chat_name_id, svr_id);
CREATE UNIQUE INDEX VoiceInfo_UNIQUE_INDEX ON VoiceInfo(chat_name_id, create_time, local_id, data_index);
