---
"@agent-wechat/agent-wechat": patch
"@agent-wechat/cli": patch
---

Report confirmed conference departures through participant updates and call rosters. Expose known participant signaling statuses and preserve unrecognized numeric codes in snapshots, events, SDK types and CLI displays. Keep microphone changes and invitation outcomes separate from membership. Acknowledge a voice consultation once, then wait for the result without triggering repeated progress speech; callers can still ask for status while the lookup runs.
