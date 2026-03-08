ALTER TABLE sessions ADD COLUMN novnc_port INTEGER UNIQUE;
UPDATE sessions SET novnc_port = vnc_port + 180 WHERE novnc_port IS NULL;
