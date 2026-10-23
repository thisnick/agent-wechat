-- Original author metadata is distinct from the actor who initiated a recall.
ALTER TABLE message_recalls ADD COLUMN original_sender_id TEXT;
ALTER TABLE message_recalls ADD COLUMN original_direction TEXT CHECK(original_direction IN ('incoming','outgoing'));
ALTER TABLE message_recalls ADD COLUMN replacement_text TEXT;
PRAGMA user_version=8;
