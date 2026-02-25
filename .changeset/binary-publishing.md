---
"@agent-wechat/cli": minor
---

Add binary artifact publishing and CLI update command

- Release workflow now publishes standalone `agent-server` binaries (amd64/arm64) as GitHub Release assets alongside Docker images
- New `wx update` command downloads the binary matching the CLI version and hot-swaps it into the running container via `docker cp` + process restart
