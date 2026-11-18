---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Shrink two kinds of encrypted plans in each account's `agent/control.sqlite` that repeated bytes the account already keeps.

- **Voice notes.** A voice attachment's plan no longer keeps the note's encoded audio when the official profile holds the note; a download reads the profile's copy first and otherwise fetches by server ID as before. Storage maintenance rewrites existing plans the same way, one row at a time, only after the profile returns identical bytes. Media cache repair is unchanged: it uses only owned bytes and never starts a transfer.
- **Sticker jobs.** A finished pack acquisition among the newest five of its pack keeps only the pack, item IDs, captions and archive locator, which is what adding the pack again reads to reuse its owned archive. Older and failed acquisitions and finished pack, Saved and send jobs drop their plans once nothing reads them: after a day, and only when they are not waiting for a profile write. Their job records and idempotency keys stay. New sticker job plans store raw bytes as base64 text instead of JSON number arrays; earlier plans still read.

An earlier version can't read sticker job plans written or reduced by this one. After downgrading, an unfinished sticker job or one awaiting a profile write fails, and adding a pack again doesn't reuse an earlier download. Voice plans stay readable.
