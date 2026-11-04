---
"@agent-wechat/cli": patch
---

`wx` starts again on Linux machines without PulseAudio, such as servers and slim containers. It now loads `libpulse` only when a call opens this computer's microphone and speakers, and reports what to install if it is missing; installing the package no longer fails there.
