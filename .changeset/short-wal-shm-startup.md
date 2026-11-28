---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

The daemon now starts when an interrupted open (for example after a crash, power loss, or full disk) leaves SQLite's 3-byte `-shm` file beside `registry.sqlite` or `control.sqlite`. It keeps a pre-open copy in `.sqlite-recovery-…`, verifies any `-wal` log standalone, and lets SQLite rebuild the `-shm` index. Unexpected 3-byte contents and other non-block `-shm` lengths are still refused with existing files preserved.
