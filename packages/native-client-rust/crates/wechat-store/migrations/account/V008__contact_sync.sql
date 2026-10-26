ALTER TABLE apply_intents ADD COLUMN stream TEXT NOT NULL DEFAULT 'message';
CREATE INDEX apply_intents_stream ON apply_intents(stream,state,created_at,id);
PRAGMA user_version=8;
