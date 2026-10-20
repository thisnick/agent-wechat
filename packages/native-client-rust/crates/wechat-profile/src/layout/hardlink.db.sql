-- Inspected Linux 4.1.13.26 ordinary schema; no rows or private identifiers.
CREATE TABLE db_info(Key TEXT PRIMARY KEY, ValueInt64 INTEGER, ValueDouble REAL, ValueStdStr TEXT, ValueBlob BLOB);
CREATE TABLE dir2id(username TEXT PRIMARY KEY);
CREATE TABLE file_checkpoint_v4(month_id INTEGER PRIMARY KEY ASC);
CREATE TABLE file_hardlink_info_v4(md5_hash INTEGER, md5 TEXT, type INTEGER, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER, _rowid_ INTEGER PRIMARY KEY ASC, extra_buffer BLOB);
CREATE TABLE image_hardlink_info_v4(md5_hash INTEGER, md5 TEXT, type INTEGER, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER, _rowid_ INTEGER PRIMARY KEY ASC, extra_buffer BLOB);
CREATE TABLE talker_checkpoint_v4(talker_id INTEGER PRIMARY KEY ASC, month_id INTEGER);
CREATE TABLE video_checkpoint_v4(month_id INTEGER PRIMARY KEY ASC);
CREATE TABLE video_hardlink_info_v4(md5_hash INTEGER, md5 TEXT, type INTEGER, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER, _rowid_ INTEGER PRIMARY KEY ASC, extra_buffer BLOB);
CREATE INDEX file_hardlink_info_v4_DIR1 ON file_hardlink_info_v4(dir1);
CREATE INDEX file_hardlink_info_v4_MD5_HASH ON file_hardlink_info_v4(md5_hash);
CREATE INDEX file_hardlink_info_v4_MODIFY_TIME ON file_hardlink_info_v4(modify_time);
CREATE INDEX image_hardlink_info_v4_DIR1 ON image_hardlink_info_v4(dir1);
CREATE INDEX image_hardlink_info_v4_MD5_HASH ON image_hardlink_info_v4(md5_hash);
CREATE INDEX image_hardlink_info_v4_MODIFY_TIME ON image_hardlink_info_v4(modify_time);
CREATE INDEX talker_checkpoint_v4_MONTH_ID ON talker_checkpoint_v4(month_id);
CREATE INDEX video_hardlink_info_v4_DIR1 ON video_hardlink_info_v4(dir1);
CREATE INDEX video_hardlink_info_v4_MD5_HASH ON video_hardlink_info_v4(md5_hash);
CREATE INDEX video_hardlink_info_v4_MODIFY_TIME ON video_hardlink_info_v4(modify_time);
