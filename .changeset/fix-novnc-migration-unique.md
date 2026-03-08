---
"@agent-wechat/cli": patch
---

Fix SQLite migration error: use CREATE UNIQUE INDEX instead of UNIQUE column constraint in ALTER TABLE for novnc_port, since SQLite does not support adding UNIQUE columns via ALTER TABLE.
