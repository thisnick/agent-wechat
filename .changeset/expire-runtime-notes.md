---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Expire operational notes in `control.sqlite` that previously grew forever with traffic, and retire permanently superseded metadata receipts (#730).

- Contact phase markers (`contact-notifications:`, `contact-pinning:`, `contact-pinning-time:`, `contact-privacy-observation:`), non-sync `friend-attention-stage:` markers, and finished or held group refresh markers (`incoming-group-done:`, `incoming-group-held:`) are now removed after 30 days once their owning sync batch, metadata receipt or contact change has finished. Held group refreshes keep their sealed diagnostic response bodies under `incoming-group-receipt:` for 7 days.
- Login QR images (`qr:`) and orphaned `metadata-hydrated:` checkpoints are removed once their login or metadata request finishes. Login idempotency keys (`login-key:`), superseded saved-login journals (`saved-login-superseded:`) and raw bytes of applied sync controls (`sync-control:` not paired with `skipped-control:` or referenced by `recall-control:`) are removed after 7 days.
- Skipped WAL conversion event receipts (`event:profile-wal-conversion-skipped:`) now hash the error text to bound key size and expire after 30 days.
- Permanently superseded or unrequested metadata receipts are retired from `metadata-pending:` instead of looping on every recovery pass and blocking profile exports.
