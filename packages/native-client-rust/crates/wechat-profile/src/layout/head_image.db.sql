-- Inspected Linux 4.1.13.26 ordinary schema; no rows or private identifiers.
CREATE TABLE head_image(username TEXT PRIMARY KEY, md5 TEXT, image_buffer BLOB, update_time INTEGER);
