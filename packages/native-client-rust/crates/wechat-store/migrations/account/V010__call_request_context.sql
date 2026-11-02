-- Preserve exact owned request bytes for local recovery; older intents have no
-- recoverable request. This does not authorize replay or infer remote completion.
ALTER TABLE call_actions ADD COLUMN encrypted_request BLOB;
PRAGMA user_version=10;
