---
"@agent-wechat/client-ui": minor
"@agent-wechat/shared": minor
"@agent-wechat/cli": patch
---

Show latest-message previews and group member avatar collages in the conversation list. Prefix outgoing previews with “You” and incoming group previews with the saved sender name, while using compact labels for media and preserving recall suppression.

Add optional `lastMessage` and `avatarMembers` summaries to thread list/detail responses using existing profile history and roster data. Resolve only the returned page in the same snapshot, preserve thread pagination, distinguish empty/unavailable history, and keep existing fields compatible without database migrations or upstream refreshes.
