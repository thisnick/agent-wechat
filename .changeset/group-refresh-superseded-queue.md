---
"@agent-wechat/rust-crates": patch
---

Stop the incoming group refresh worker from running retained superseded refresh records as queued jobs. Previously, after a group refresh was superseded by a newer local change, a later worker pass could read the group metadata again, reapply it and replace the retained record. Records already stored are left in place and are no longer picked up.
