---
"@agent-wechat/agent-wechat": minor
---

Allow explicit `*` rules for incoming direct-call callers, conference inviters
and conference groups. Exact group rules take precedence over the wildcard,
including an auto-join opt-out. Wildcard group auto-join requires an inviter
allowlist, with `['*']` available to accept any inviter.

Keep outbound destinations, tool requesters and owner consultation grants under
the existing exact authorization checks. Recheck wildcard rules and current
invitation identity before joining, and retain actual group identities for
conversation context and separate consultation sessions.
