-- Media payloads move out of the database into private files under the
-- account's agent/media directory. payload_file names that file. profile_key
-- marks an attachment whose bytes are read from the official profile instead of
-- a copy kept here. Earlier rows keep their payload BLOB until it is moved.
ALTER TABLE media_assets ADD COLUMN payload_file TEXT;
ALTER TABLE media_assets ADD COLUMN profile_key TEXT;
ALTER TABLE media_sources ADD COLUMN payload_file TEXT;
PRAGMA user_version=13;
