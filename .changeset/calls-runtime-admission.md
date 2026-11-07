---
"@agent-wechat/cli": patch
---

Move the shared per-account call admission state machine into wechat-calls.
The daemon uses the same implementation and retains its account permissions,
operation journals, stale-attempt checks and uncertain-outcome handling.
