---
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/cli": patch
"@agent-wechat/rust-crates": patch
---

Improve native video calling across desktop UI, `wx calls`, and daemon transcoding:

- Render local self-capture directly inside the local participant's conference grid tile (with local speaking, mute, and camera indicators), hide the redundant corner self-view overlay when the self tile is visible, avoid duplicating the local participant in the conference roster drawer, and raise desktop self-view resolution to 640x480 without `-fflags nobuffer` IDR drops.
- Fix incoming transcoded MJPEG video stalling on `Waiting for video...` under FFmpeg 7.1/8.0 (such as Homebrew FFmpeg on macOS) by explicitly converting MJPEG encoder frames to full-range YUV (`out_range=full`, `-strict unofficial -pix_fmt yuv420p -color_range pc`), forwarding control-socket track events into `wx calls` video reception, and using per-track latest-frame decoding. Also normalize WebSocket close teardown errors so `wx calls` does not print raw `Sending after closing is not allowed` errors.
- Reduce desktop video lag, choppiness, and macroblocking by dropping unreferenced P-frames after packet/queue loss until the next IDR keyframe (including carrying eviction drop flags across multi-packet RSM reassembly), replacing the 16-frame FIFO JPEG transcoder buffer with a latest-frame slot and AUD frame flushing, keeping receive queues shallow for independent JPEG frames (2) while preserving deeper bursts for inter-frame H.264/H.265 tracks (16), skipping SQLite `spawn_blocking` grant lookups for owner bearer tokens on media frames, pacing desktop WebView JPEG IPC with `requestAnimationFrame` acknowledgements, and deduplicating steady-state audio level updates.
