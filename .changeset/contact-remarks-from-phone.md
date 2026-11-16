---
"@agent-wechat/rust-crates": patch
"@agent-wechat/agent-wechat": patch
---

Renaming a contact on the phone now updates their remark in profiles the daemon created (#483). The daemon no longer drops remarks that WeChat sends with a version marker: when a remark change arrives it looks the contact up again and saves the current remark, and `wx contacts refresh` saves it too. Remark changes now also arrive while the daemon is connected instead of only after it reconnects. To fill in remarks that are missing from before this update, run `wx contacts sync` or refresh the contact.
