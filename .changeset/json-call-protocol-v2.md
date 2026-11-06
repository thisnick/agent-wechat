---
"@agent-wechat/cli": major
"@agent-wechat/agent-wechat": major
"@agent-wechat/shared": major
---

Move call WebSockets to protocol version 2. Clients announce supported versions
before issuing commands or attaching media; omitted versions mean version 1 and
are rejected. Audio now uses JSON messages containing a track ID, generation and
base64 PCM, and media setup returns a receive-track array. Upgrade daemon, CLI
and OpenClaw plugin together. Raw binary call media is no longer accepted.

Conference participant snapshots report microphone state when supplied by the
server, keeping mute state separate from room membership.

Fix intermittent secure-channel setup failures caused by truncating a
zero-prefixed shared secret. Authentication and call polling use the corrected
calculation without the previous handshake-retry workaround.
