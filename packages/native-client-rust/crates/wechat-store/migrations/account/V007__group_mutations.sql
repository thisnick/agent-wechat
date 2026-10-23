-- One daemon-owned intent. Raw requests/replies are separately sealed by Core.
CREATE TABLE group_mutations(
    operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    key_hash TEXT NOT NULL UNIQUE,
    request_hash TEXT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('queued','submitting','received','projecting','succeeded','failed','cancelled','needs_review')),
    step INTEGER NOT NULL DEFAULT 0 CHECK(step>=0),
    encrypted_request BLOB NOT NULL,
    encrypted_receipts BLOB NOT NULL
);
PRAGMA user_version=7;
