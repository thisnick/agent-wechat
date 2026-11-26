---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Make large-file uploads (files and original videos over 25 MiB) handle CDN errors like the official client, and say why an upload stopped:

- A CDN error that the reply marks as retryable (`isretry`), or a code the official client retries, is retried with a new sequence number on the next CDN address. The retry waits `retrysec` when given, otherwise the official backoff. At most 8 server errors are retried per upload, waiting 15 minutes in total at most. Before, any CDN error stopped the upload.
- A successful reply's `isretry`, `retrysec`, `isoverload` and `isgetcdn` no longer stop the upload, as in the official client.
- An empty or unreadable CDN reply is retried like a dropped connection on every CDN transfer, uploads and downloads, resending the same request.
- A CDN rejection now sets `serviceStage: "large_file_upload"` and `serviceCode` on the job, with reason `media_service_rejected`, instead of `media_processing_failed` with no detail. A large-file job that stops during the transfer also includes `result.cdnUploadFailure` with the request (`apply`, `uploadblock`, `fin` or `complete`), the block, the attempts for that request, the server errors retried and a fixed cause.
