---
"@agent-wechat/cli": minor
"@agent-wechat/wechat": minor
---

Prepare npm + Docker release foundation:

- publishable package metadata for CLI and OpenClaw extension
- bundled CLI build for standalone npm install
- OpenClaw extension package name/id alignment to remove id mismatch warnings
- tag-based GitHub Actions release workflows (npm trusted publishing + multi-arch Docker)
