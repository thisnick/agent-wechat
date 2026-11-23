---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": major
---

Fix VoIP relay check-in failing on IPv6 addresses and add dual-stack IPv4/IPv6 relay support across direct audio, direct video, and conference calls (#662).

- Accept IPv4, IPv6, or empty strings (up to 45 bytes) in `RelayS2CCheckinResp` `clientIpStr` (field 12) and `svrIpStr` (field 13), preserving the learned text verbatim and setting the numeric IPv4 fields to `0` for IPv6/empty values matching the official client while keeping UDP pinned to the actual relay endpoint.
- Add end-to-end dual-stack IPv4/IPv6 UDP relay selection and transport (`SelectedRelay::endpoint`, `TwoRttRelay::new`, `TwoRttRelay::receive` now take/expose `std::net::SocketAddr`), strategy candidate enumeration with local route probing and legacy `Strategy.udp_addrs` fallback, and 1-RTT `merge_mode` (`is_merged_response`) support with 2-RTT fallback.
- Classify relay handshake errors as `relay_setup_failed` and record `call-media-diagnostic` for incoming direct calls as well as outgoing direct calls.
- Breaking Rust crate change in `wechat-protocol`: `SelectedRelay::endpoint`, `TwoRttRelay::new`, and `TwoRttRelay::receive` now use `std::net::SocketAddr` instead of `std::net::SocketAddrV4`.
