CREATE TABLE IF NOT EXISTS finder_message_sources (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    account_dir TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    local_id INTEGER NOT NULL,
    server_id INTEGER NOT NULL,
    object_id TEXT NOT NULL,
    object_nonce_id TEXT NOT NULL,
    raw_xml TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (session_id, account_dir, chat_id, local_id)
);

CREATE INDEX IF NOT EXISTS idx_finder_message_sources_identity
    ON finder_message_sources(session_id, account_dir, object_id, object_nonce_id);
