-- A closed local call is distinct from a confirmed remote termination. Keep
-- unknown action receipts unchanged; a dead controller cannot own a new call.
ALTER TABLE call_sessions ADD COLUMN local_end_reason TEXT;
ALTER TABLE call_sessions ADD COLUMN remote_terminal_confirmed INTEGER NOT NULL DEFAULT 0 CHECK(remote_terminal_confirmed IN (0,1));
PRAGMA user_version=12;
CREATE INDEX demo_operations_state ON operations(state);
