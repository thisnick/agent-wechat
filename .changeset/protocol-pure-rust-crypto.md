---
"@agent-wechat/rust-crates": minor
---

Add an opt-in `pure-rust-crypto` Cargo feature to `wechat-protocol` alongside the default-enabled `openssl-crypto` backend so consumers can build without vendored OpenSSL (`default-features = false, features = ["pure-rust-crypto"]`). Add portable `crypto::Curve`, `crypto::Ecdh::generate_for`, `crypto::Ecdh::from_private_for`, `crypto::cbc_encrypt`, and `crypto::cbc_decrypt` APIs; OpenSSL-typed `Ecdh::generate`, `Ecdh::from_private`, and `crypto::cbc` remain available when `openssl-crypto` is enabled. Callers setting `default-features = false` must select at least one crypto backend (`openssl-crypto` or `pure-rust-crypto`).
