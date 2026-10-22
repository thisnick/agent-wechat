-- A target claim survives every idempotency key and every process restart.
CREATE TABLE outgoing_recalls(
    operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    target_key TEXT NOT NULL UNIQUE,
    source_key TEXT NOT NULL UNIQUE,
    phase TEXT NOT NULL CHECK(phase IN ('queued','submitting','accepted','succeeded','failed','needs_review')),
    encrypted_request BLOB NOT NULL
);
CREATE TABLE recall_keys(
    key_hash TEXT PRIMARY KEY,
    request_hash TEXT NOT NULL,
    operation_id TEXT NOT NULL REFERENCES outgoing_recalls(operation_id)
);
PRAGMA user_version=6;
