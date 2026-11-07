---
"@agent-wechat/cli": patch
---

Publish `wechat-calls` as the fifth coordinated Rust crate, after its protocol
dependency. Include portable PCM/mixing and native Opus examples, feature and
integration documentation, MIT/codec licenses and package checks. The current
lower-level session/media APIs do not yet provide a high-level account-bound
CallClient/CallHandle service; native workers retain their 20-minute safety limit.

Keep optional internal dependencies synchronized when cutting versions. Use OIDC
for existing crates and the protected bootstrap token only for new crate names;
verify exact archive/source identity before resuming partial publications. npm
publication waits for all five crates.
