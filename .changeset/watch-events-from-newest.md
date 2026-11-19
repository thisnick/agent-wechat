---
"@agent-wechat/cli": patch
---

`wx events watch`, `wx events --watch` and `wx messages watch` now start from the newest event when you don't pass `--after`. Before, they started at cursor 0, and on an account with more than 10,000 events, which is more than the daemon keeps, they failed immediately with `event_cursor_expired`. A cursor file still takes priority. A new cursor file is now written as soon as the watch connects, so the next run resumes from that point even if no event arrived.

`wx events list` and `wx events` without `--after` now show the most recent events instead of the oldest ones.

An explicit `--after` or a saved cursor that is older than the kept events still fails, now with a message that says so and how to start from the newest event.

The daemon's `GET /v1/accounts/{account}/events` and `/events/stream` accept `newest=true`: a page holds the most recent events, and the stream sends only events added after it connects. It cannot be combined with `after`.
