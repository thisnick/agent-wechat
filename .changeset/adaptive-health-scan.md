---
"@agent-wechat/agent-server": patch
---

Split health monitoring into a cheap one-second process check and an expensive
UI observation that defaults to every 30 seconds. Add
`AGENT_WECHAT_HEALTH_UI_SCAN_INTERVAL_SECS` (any positive integer) to tune recovery
latency versus CPU usage. Add
`AGENT_WECHAT_HEALTH_UNRESPONSIVE_TIMEOUT_SECS` to keep the failure tolerance
independent from the sampling interval.
