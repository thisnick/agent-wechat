---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Group member lists fill faster after setting up a new profile: background group refreshes now run in batches of 5 per pass, wake the account loop on enqueue and while due groups remain, yield the account writer lock around each metadata request, and back off individual failing groups so one failure does not block healthy ones.
