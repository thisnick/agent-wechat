---
"@agent-wechat/rust-crates": patch
---

The Rust libraries now share one version with the CLI, OpenClaw extension and desktop app. Every release publishes all five crates under the release version, so `wechat-protocol` X.Y.Z comes from the same source as `wx` X.Y.Z. Library versions jump from 2.2.0 to the next application version; no API changes.
