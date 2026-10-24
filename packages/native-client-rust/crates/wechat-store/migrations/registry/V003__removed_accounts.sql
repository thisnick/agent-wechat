BEGIN;
ALTER TABLE accounts ADD COLUMN removed_at INTEGER;
PRAGMA user_version=3;
COMMIT;
