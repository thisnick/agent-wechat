-- Digests of idempotency keys and record IDs whose receipts were removed by
-- retention. A retired key can never admit a new mutation: requests that reuse
-- it are refused as expired. Only a 16-byte SHA-256 prefix is kept; no request,
-- reply or payload survives retirement.
CREATE TABLE retired_keys(
    family TEXT NOT NULL,
    digest BLOB NOT NULL CHECK(length(digest)=16),
    retired_at INTEGER NOT NULL,
    PRIMARY KEY(family,digest)
) WITHOUT ROWID;
PRAGMA user_version=15;
