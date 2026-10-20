CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE accounts(id TEXT PRIMARY KEY,storage_mode TEXT NOT NULL,profile_path TEXT NOT NULL UNIQUE,schema_compatibility TEXT NOT NULL);
PRAGMA user_version=1;
