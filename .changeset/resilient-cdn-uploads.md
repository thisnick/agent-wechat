---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
"@agent-wechat/rust-crates": patch
---

Make native CDN chunked uploads and downloads reuse a pooled HTTP/1.1 connection per transfer instead of creating a new client with `Connection: close` per chunk, retry transient transport drops and 5xx/408/429 responses with bounded backoff while honoring cancellation/logout fences, and fail over across authenticated CDN server IPs.
