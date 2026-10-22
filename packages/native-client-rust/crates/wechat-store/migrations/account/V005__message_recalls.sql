-- Account-sidecar markers never retain original message bodies or media.
CREATE TABLE message_recalls(
    thread_id TEXT NOT NULL,
    server_id TEXT NOT NULL,
    message_id TEXT,
    source_created_at INTEGER,
    observed_at TEXT NOT NULL,
    recalled_at INTEGER,
    origin TEXT NOT NULL,
    projection_state TEXT NOT NULL DEFAULT 'pending',
    PRIMARY KEY(thread_id,server_id)
);
CREATE INDEX message_recalls_source ON message_recalls(message_id,source_created_at);
PRAGMA user_version=5;
