---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
"@agent-wechat/desktop": patch
---

Send `.opus`, `.oga`, `.caf` and `.amr` audio as voice notes. OpenClaw text-to-speech replies, which are written as `.opus`, no longer fail with "Voice sending requires an explicit audio attachment", and a remote file served with a generic content type is classified by its name. The daemon now also converts CAF and AMR sources. An unsupported local voice source is now rejected before OpenClaw records the send as dispatched.
