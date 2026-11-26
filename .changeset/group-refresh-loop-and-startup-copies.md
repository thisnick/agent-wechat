---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Stop a group refresh from repeating forever and stop interrupted starts from keeping database copies on a full disk (#722).

- When a group changed again while its information was being refreshed, the daemon kept fetching that group's details from WeChat on every pass and wrote ever-longer rows into `control.sqlite`. It now fetches once more and stops. Rows earlier versions left behind are removed by the regular cleanup after seven days, and the database shrinks once there is room to compact it.
- Copies that an interrupted start left in `.sqlite-admission-*` folders are now removed before the daemon copies a database again, not only after a start succeeds. A start that failed for lack of disk space used to keep them, so it could never free the space it needed. A copy left while the daemon was already opening the database is still kept until a start succeeds. When the disk is full, the error now says how much free space the check needs.
