CREATE TABLE pagination_views(id TEXT PRIMARY KEY,query_hash TEXT NOT NULL,event_cursor TEXT NOT NULL,expires_at INTEGER NOT NULL,payload BLOB NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX pagination_expiry ON pagination_views(expires_at,created_at);
PRAGMA user_version=4;
