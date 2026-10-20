CREATE TABLE IF NOT EXISTS account_settings(account_id TEXT PRIMARY KEY REFERENCES accounts(id),keys_file TEXT,alias TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS access_tokens(id TEXT PRIMARY KEY,name TEXT NOT NULL,digest TEXT NOT NULL,accounts_json TEXT NOT NULL,scopes_json TEXT NOT NULL,expires_at INTEGER,revoked INTEGER NOT NULL DEFAULT 0);
PRAGMA user_version=2;
