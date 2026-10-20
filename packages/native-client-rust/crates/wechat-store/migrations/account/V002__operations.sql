CREATE TABLE runtime(key TEXT PRIMARY KEY,value BLOB NOT NULL);
CREATE TABLE operations(id TEXT PRIMARY KEY,kind TEXT NOT NULL,state TEXT NOT NULL,result_json TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE TABLE events(sequence INTEGER PRIMARY KEY,kind TEXT NOT NULL,data_json TEXT NOT NULL,created_at TEXT NOT NULL);
CREATE TABLE outbox(id TEXT PRIMARY KEY,idempotency_key TEXT NOT NULL UNIQUE,payload_hash TEXT NOT NULL,destination TEXT NOT NULL,payload BLOB NOT NULL,state TEXT NOT NULL,client_id TEXT NOT NULL,server_id TEXT,error_code TEXT,created_at TEXT NOT NULL);
CREATE TABLE apply_intents(id TEXT PRIMARY KEY,payload BLOB NOT NULL,state TEXT NOT NULL,created_at TEXT NOT NULL);
CREATE TABLE consumer_offsets(consumer_id TEXT PRIMARY KEY,sequence INTEGER NOT NULL);
CREATE TABLE read_overrides(message_id TEXT PRIMARY KEY,is_read INTEGER NOT NULL,user_generation INTEGER NOT NULL DEFAULT 0);
PRAGMA user_version=2;
