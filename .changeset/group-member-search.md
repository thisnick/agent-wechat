---
"@agent-wechat/cli": patch
"@agent-wechat/desktop": patch
---

Add `wx groups members list --search` to find members of one group by ID, WeChat ID, nickname, remark or name in the group. Member search now also matches a member's nickname, WeChat ID and name in the group when you have given them a remark, so mention suggestions find them too. Group member rows now include `nickname` and `alias`. Correct the help text for `wx contacts lookup` and `wx groups lookup`: both ask WeChat directly and don't save the result, and contact lookup accepts one or two IDs.
