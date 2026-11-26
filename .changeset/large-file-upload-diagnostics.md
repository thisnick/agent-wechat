---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Report why a large file (over 25 MiB) stopped uploading. A CDN rejection now sets `serviceStage: "large_file_upload"` and `serviceCode` on the job, with reason `media_service_rejected`, instead of `media_processing_failed` with no detail. Failed large-file jobs also include `result.cdnUploadFailure` with the request (`apply`, `uploadblock`, `fin` or `complete`), the block, the attempts made and a fixed cause. An empty or unreadable CDN reply is now retried like a dropped connection, resending the same request.
