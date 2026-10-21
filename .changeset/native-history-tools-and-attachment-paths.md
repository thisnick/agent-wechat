---
"@agent-wechat/agent-wechat": minor
---

Add read-only chat discovery, stored message history, keyword search and message lookup tools. Group turns stay within their current conversation; cross-conversation lookup requires an owner turn outside a group. History reads do not mark messages read or download media.

Restore original attachment paths and attachment metadata in direct and group inputs. Retain bounded sender-admitted group context across restarts until the next mention, with explicit pending states, verified media and one prepared image preview. Preserve original mention facts and distinguish historical context from the current trigger.
