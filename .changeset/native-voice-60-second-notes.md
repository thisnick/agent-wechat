---
"@agent-wechat/cli": patch
"@agent-wechat/agent-wechat": patch
---

Send outgoing voice as notes of up to 60 seconds, WeChat's single-message limit, instead of 10-second notes. Longer audio splits into the fewest equal-length notes and keeps per-note IDs, partial results and `needs_review` on uncertainty. Notes above 32 KiB of SILK (about 15 seconds) are uploaded as consecutive offset ranges of one voice message; that upload path has offline fixtures only and still requires live verification.
