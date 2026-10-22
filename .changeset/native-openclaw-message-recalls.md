---
"@agent-wechat/agent-wechat": minor
---

Add the current-conversation `wechat_recall` tool for own sent messages using exact stored message and server IDs, stable idempotency keys and explicit confirmed, rejected, pending or uncertain operation receipts. Outbound delivery results now expose verified stored IDs, including per-note voice receipts, instead of send or job IDs.

Wait briefly for accepted media jobs to publish their stored message receipts before verification, using only reads of the same job. Missing receipts and partial or uncertain outcomes never trigger another send.

Consume durable native recall events independently of model work. Invalidate buffered originals, quotes, attachment references and pending handoff proofs, suppress late media/transcription and automatic replies, and retain tombstones across restart. History and media tools stop exposing recalled content. Existing runtime transcripts and host-retained attachment bytes follow their existing retention policy.

Bind custody revisions, host turns and reply keys to the exact source target so reused local message IDs admit the new message without accepting late callbacks or cached media from the old one.
