---
"@agent-wechat/cli": minor
"@agent-wechat/desktop": minor
"@agent-wechat/client-ui": minor
---

Add a separate AgentWechat desktop package with the existing chat UI, a bundled native hub and CLI, system-keyring connection storage, optional HTTPS network access, and native file saving. Keep standalone CLI credentials file based and support private-pipe credentials for client commands.

Build desktop installers through the prerelease version-package flow. Sign Windows binaries before packaging and sign the final installer, and require Apple notarization, stapling and Gatekeeper verification before publishing macOS downloads.
