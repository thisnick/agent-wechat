"@agent-wechat/wechat": patch
---

Harden WeChat inbound policy and command handling to align with OpenClaw channel security patterns.

- Add centralized access-control logic for DM/group policy resolution and inbound decisions.
- Normalize WeChat IDs/allowlists (including wildcard support) before authorization checks.
- Compute and pass `CommandAuthorized` in inbound context and block unauthorized group control commands.
- Apply mention gating with authorized command bypass behavior and fix segment-level mention handling.
- Disable NO_REPLY command-window batching by isolating command-bearing messages into per-message dispatch.
- Add group override support (`enabled`, `groupPolicy`, `allowFrom`) and align onboarding/docs semantics for `groupAllowFrom`.
- Add unit tests for policy resolution, authorization, and mention/command gating behavior.
