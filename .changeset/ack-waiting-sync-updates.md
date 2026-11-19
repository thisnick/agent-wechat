---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": minor
---

Acknowledge sync updates whose follow-up is local work, and finish them in the background, so they no longer hold back the sync position.

- **Group changes** are acknowledged as soon as their fields are saved. The group refresh that fetches the group's members and announcement runs separately and writes them when it succeeds. A refresh that has given up no longer holds the update forever.
- **Contact changes** with fields whose order the daemon can't establish, or with field controls it can't read, are acknowledged. One exact lookup of the contact is queued to fetch its current values. Uncertain mute and pin state still keeps notifications suppressed until a newer known update arrives.
- **An older pinned-chat change** that a newer one superseded is acknowledged without applying it, and the contact is looked up again.
- **Sticker sync notices** are acknowledged at once. Sticker reconciliation still runs after the sync.
