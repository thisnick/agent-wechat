---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Make large-file uploads (over 25 MiB) follow the official client when the CDN reports an error, and say why an upload stopped:

- A CDN error that the reply marks as retryable (`isretry`), or a code the official client retries, is retried on the next CDN address with a new sequence number, waiting `retrysec` when given, up to 8 times per upload. Before, any CDN error stopped the upload.
- A successful reply's `isretry`, `retrysec`, `isoverload` and `isgetcdn` no longer stop the upload, as in the official client.
- An empty or unreadable CDN reply is retried like a dropped connection, resending the same request.
- A CDN rejection now sets `serviceStage: "large_file_upload"` and `serviceCode` on the job, with reason `media_service_rejected`, instead of `media_processing_failed` with no detail. Failed large-file jobs also include `result.cdnUploadFailure` with the request (`apply`, `uploadblock`, `fin` or `complete`), the block, the attempts made and a fixed cause.
