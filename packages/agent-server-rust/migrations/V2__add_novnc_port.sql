ALTER TABLE sessions ADD COLUMN novnc_port INTEGER;
UPDATE sessions SET novnc_port = vnc_port + 180 WHERE novnc_port IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_novnc_port ON sessions(novnc_port);
