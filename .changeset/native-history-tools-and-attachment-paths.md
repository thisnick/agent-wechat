---
"@agent-wechat/agent-wechat": minor
---

Add default stored message history, keyword search and exact message lookup tools, restricted to the trusted current WeChat conversation for every requester, including owners. These default tools cannot select another thread or enumerate chats. History reads do not mark messages read or download media.

Add current-thread attachment retrieval using verified cached bytes or bounded, server-owned downloads. Return managed file paths and prepared image content, with cache-only and strict quality options and explicit pending, expired and unavailable outcomes.

Restore original attachment paths and attachment metadata in direct and group inputs. Retain bounded sender-admitted group context across restarts until the next mention, with explicit pending states, verified media and one prepared image preview. Preserve original mention facts and distinguish historical context from the current trigger.
