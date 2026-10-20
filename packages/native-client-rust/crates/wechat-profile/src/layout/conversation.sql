-- Dynamic conversation table name is a verified MD5 identifier.
CREATE TABLE __MESSAGE_TABLE__(local_id INTEGER PRIMARY KEY AUTOINCREMENT, server_id INTEGER, local_type INTEGER, sort_seq INTEGER, real_sender_id INTEGER, create_time INTEGER, status INTEGER, upload_status INTEGER, download_status INTEGER, server_seq INTEGER, origin_source INTEGER, source TEXT, message_content TEXT, compress_content TEXT, packed_info_data BLOB, WCDB_CT_message_content INTEGER DEFAULT NULL, WCDB_CT_source INTEGER DEFAULT NULL);
CREATE INDEX __MESSAGE_TABLE___SENDERID ON __MESSAGE_TABLE__(real_sender_id);
CREATE INDEX __MESSAGE_TABLE___SERVERID ON __MESSAGE_TABLE__(server_id);
CREATE INDEX __MESSAGE_TABLE___SORTSEQ ON __MESSAGE_TABLE__(sort_seq);
CREATE INDEX __MESSAGE_TABLE___TYPE_SEQ ON __MESSAGE_TABLE__(local_type, sort_seq);
