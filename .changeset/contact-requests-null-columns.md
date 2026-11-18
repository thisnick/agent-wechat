---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Fix listing friend requests failing with `profile_unavailable` (409) on profiles the official client wrote. Requests stored with empty (SQL NULL) columns, such as the remark and labels, couldn't be read, so `wx contacts requests list`, `GET contact-requests` and the OpenClaw `wechat_contact_request` tool failed for every request. Empty columns now read as empty text or zero, and accepting, updating or deleting a request keeps an empty column empty unless the change gives it a value, so the stored row keeps its form. New request rows are written as before.
