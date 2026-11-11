---
"@agent-wechat/rust-crates": minor
---

`wechat-protocol` now reports why a request failed as a type, so embedders no longer match error text. A non-zero service code carries `ServiceRejected { service, code }`; `is_session_rejected` tells you the account needs a new login or renewal. A stop, transfer or renewal that ends a request or a notification wait carries `AccountInterrupted`, so a push loop can end quietly on `Stopped` and reconnect on `CredentialsSuperseded`. Error messages are unchanged.

Logout and shutdown no longer need a Tokio runtime: `PendingLogout::submit_blocking`, `PendingLogout::submit_guarded_blocking`, `NativeClient::logout_blocking` and `NativeClient::shutdown_blocking` run on the caller's own blocking worker, with the same at-most-once logout as the async methods.
