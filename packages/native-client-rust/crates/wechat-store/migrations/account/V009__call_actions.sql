-- Operational intents and uncertain receipts only. Official profiles own chat history.
CREATE TABLE call_sessions (
  call_id TEXT PRIMARY KEY,
  authority_generation TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','needs_review','ended')),
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX one_unresolved_call ON call_sessions((1)) WHERE state!='ended';
CREATE TABLE call_actions (
  call_id TEXT NOT NULL REFERENCES call_sessions(call_id),
  action_id TEXT NOT NULL,
  route INTEGER NOT NULL,
  request_digest TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('prepared','received','needs_review')),
  encrypted_reply BLOB,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(call_id, action_id)
);
PRAGMA user_version=9;
