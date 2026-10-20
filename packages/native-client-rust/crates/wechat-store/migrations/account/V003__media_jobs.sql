CREATE TABLE media_sources(id TEXT PRIMARY KEY,name TEXT NOT NULL,media_type TEXT NOT NULL,byte_length INTEGER NOT NULL,sha256 TEXT NOT NULL,payload BLOB NOT NULL,created_at TEXT NOT NULL);
CREATE TABLE media_jobs(id TEXT PRIMARY KEY,idempotency_key TEXT NOT NULL UNIQUE,request_hash TEXT NOT NULL,kind TEXT NOT NULL,state TEXT NOT NULL,phase TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,cancel_requested INTEGER NOT NULL DEFAULT 0,retryable INTEGER NOT NULL DEFAULT 0,error_code TEXT,data_json TEXT NOT NULL,payload BLOB NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE INDEX media_jobs_queue ON media_jobs(state,created_at,id);
CREATE TABLE media_assets(id TEXT PRIMARY KEY,message_id TEXT,variant TEXT NOT NULL,kind TEXT NOT NULL,metadata_json TEXT NOT NULL,plan BLOB NOT NULL,payload BLOB,updated_at TEXT NOT NULL);
CREATE INDEX media_assets_message ON media_assets(message_id,variant);
PRAGMA user_version=3;
