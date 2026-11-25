---
"@agent-wechat/rust-crates": minor
---

Gate the bundled C SILK voice encoder and decoder (`wechat_protocol::silk::encode` and `wechat_protocol::silk::decode`, plus the `cc` build step) behind a default-enabled `voice-silk` Cargo feature on `wechat-protocol`. Building `wechat-protocol` with `default-features = false` skips compiling C SILK while keeping SILK framing validation, WAV formatting, voice size/duration constants, and all protocol codecs. Callers using `default-features = false` must explicitly enable `voice-silk` to call `silk::encode` or `silk::decode`.
