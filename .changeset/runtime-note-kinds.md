---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Retention of the daemon's internal `control.sqlite` notes now comes from one list of every kind of note, and keeps every note for as long as before. The general way the daemon saves such a note now refuses keys over 16 KiB, which only a bug can produce (#722 grew keys to 86 KB).
