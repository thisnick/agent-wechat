---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": minor
---

Bound the growth of each account's `agent/control.sqlite`. Maintenance now reduces finished sync batches, text and media sends, group and contact changes, recalls and call actions to small receipts. Download records, which hold no payload, are kept. It removes finished metadata receipts, expired pagination views and old duplicate guards, and gives freed space back to the file system. Unfinished, blocked and still-uncertain work keeps its full records.

Uncertain (`needs_review`) records expire after 7 days, or once more than 100 newer ones exist in the same family. Expiry keeps the record and its unknown outcome but drops its sealed request and reply bytes; nothing is resent. Old receipts are removed after 90 days (text and media sends) or 365 days (group and contact changes and recalls). Their idempotency keys are kept as short digests, so reusing one returns `idempotency_key_expired` (409) instead of starting new work, and reading a removed send, media send job or operation returns `operation_expired` (410).

The voice fallback now reads the same newest 256 retained message batches as recall evidence, instead of up to 10,000. Add `wx store usage` to show a stopped account's sidecar size, its largest record families and the unfinished work that keeps records whole.
