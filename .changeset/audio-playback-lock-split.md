---
"@agent-wechat/desktop": patch
"@agent-wechat/cli": patch
---

Split `PlaybackHandle`'s producer resampler lock from the audio output queue lock so real-time audio drain never blocks on producer resampling, while preserving stale-token checks and resampling capacity preflight.
