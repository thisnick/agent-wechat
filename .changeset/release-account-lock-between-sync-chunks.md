---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Let message and contact reads use the account lock between chunks of incoming message replay. Interrupted replay resumes from the saved batch, and the profile checkpoint still waits for pending work to finish.

A received recall hides the original message during catch-up. The recall event is sent when its command is replayed.
