CREATE TABLE IF NOT EXISTS file_download_operations (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    chat_id TEXT NOT NULL,
    local_id INTEGER NOT NULL,
    server_id TEXT,
    expected_filename TEXT NOT NULL,
    sent_at TEXT,
    state TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    error_code TEXT,
    output_path TEXT,
    size_bytes INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_file_download_operations_state
    ON file_download_operations(state, updated_at);
