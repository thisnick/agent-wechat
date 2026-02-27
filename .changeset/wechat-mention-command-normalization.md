---
"@agent-wechat/wechat": patch
---

Restore reliable group command handling for mention-prefixed commands such as `@agent /compact`.

- Normalize WeChat command bodies so leading group mention tokens are stripped before command detection/authorization.
- Use command-aware detection (`isControlCommandMessage`) in monitor gating paths.
- Add a WeChat mention adapter so downstream command parsing also sees normalized command text.
- Add tests covering mention-prefixed command normalization behavior.
