-- A proven local non-submission is distinct from an unknown remote result.
-- Preserve original order, sealed evidence and legacy missing request context.
CREATE TABLE call_actions_v11 (
  call_id TEXT NOT NULL REFERENCES call_sessions(call_id),
  action_id TEXT NOT NULL,
  route INTEGER NOT NULL,
  request_digest TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK(outcome IN ('prepared','received','needs_review','not_submitted')),
  encrypted_reply BLOB,
  updated_at INTEGER NOT NULL,
  encrypted_request BLOB,
  PRIMARY KEY(call_id, action_id)
);
INSERT INTO call_actions_v11(rowid,call_id,action_id,route,request_digest,outcome,encrypted_reply,updated_at,encrypted_request)
  SELECT rowid,call_id,action_id,route,request_digest,outcome,encrypted_reply,updated_at,encrypted_request FROM call_actions;
DROP TABLE call_actions;
ALTER TABLE call_actions_v11 RENAME TO call_actions;
PRAGMA user_version=11;
