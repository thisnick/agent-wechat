---
"@agent-wechat/cli": minor
"@agent-wechat/desktop": minor
"@agent-wechat/client-ui": minor
---

Add a separate AgentWechat desktop package with the existing chat UI, a bundled native hub and CLI, system-keyring connection storage, optional HTTPS network access, and native file saving. Keep standalone CLI credentials file based and support private-pipe credentials for client commands.

Build desktop installers through the prerelease version-package flow. Sign Windows binaries before packaging and sign the final installer, and require Apple notarization, stapling and Gatekeeper verification before publishing macOS downloads.

Handle empty profile directories during fresh initialization on macOS and Windows while preserving existing contents and refusing conflicting writers.

Keep group admission and execution under the same account lock so a heartbeat cannot invalidate an already admitted operation before it starts.

Create CLI settings directories with private Windows ownership and protect existing owned directories without reopening them.
