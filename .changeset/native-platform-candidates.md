---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
---

Add macOS ARM64 and native Windows x64/ARM64 release targets, portable private-file and daemon ownership handling, and ownership-bound graceful shutdown. Extend verified npm provisioning and OpenClaw managed hosting to these targets. Build candidate artifacts on native CI workers; live account and official-app profile compatibility require separate platform acceptance and are not established by the offline smoke tests.

Apply the same private directory/file protections to WXGF decoder and media encoder scratch data, including owner-only Windows ACLs.
