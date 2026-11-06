---
"@agent-wechat/cli": patch
---

The daemon starts again after a sudden stop of the computer, VM or WSL leaves an out-of-date `-shm` index beside one of its databases. It checks the log by itself, keeps a copy of the original files in a `.sqlite-recovery-…` folder and lets SQLite rebuild the index. If the index records changes that the log is missing, it still refuses to start. The error now names the database and links to recovery steps.
