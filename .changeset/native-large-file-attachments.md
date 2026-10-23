---
"@agent-wechat/cli": minor
"@agent-wechat/agent-wechat": minor
---

Add native ordinary-file transfers up to 512 MiB, including large-file routing, upload completion receipts, verified download readback, and persisted file-message reservations. Apply the file limit consistently in the daemon, CLI, SDK, and OpenClaw while retaining the existing image, video, and voice limits. Stream large file retrievals in the CLI and OpenClaw without base64 conversion. Keep ordinary-file transfers from blocking account heartbeats and incoming sync. Interrupted or uncertain message reservations require review before another send.

Keep the submitted OpenClaw job ID and idempotency key available when status polling times out or fails, without repeating the send.
