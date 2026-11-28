---
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Stop setting `RLIMIT_AS` on the `ffmpeg` subprocess used for WXGF still previews so dynamically linked `ffmpeg` builds with many shared libraries and pipeline threads do not fail in `pthread_create()` while resident memory stays small. Clamp the remaining Unix `setrlimit` caps (`RLIMIT_CPU`, `RLIMIT_FSIZE`, `RLIMIT_NOFILE`) against inherited limits via `getrlimit`, and pass `-max_pixels 16777216` to enforce the pixel bound inside `libavcodec`.
