BEGIN;
ALTER TABLE account_settings ADD COLUMN auto_download_max_bytes INTEGER NOT NULL DEFAULT 52428800 CHECK(auto_download_max_bytes BETWEEN 0 AND 536870912);
PRAGMA user_version=4;
COMMIT;
