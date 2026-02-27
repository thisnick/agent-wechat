# @agent-wechat/wechaty-puppet

## 0.8.0

### Minor Changes

- [#59](https://github.com/thisnick/agent-wechat/pull/59) [`eb95ac6`](https://github.com/thisnick/agent-wechat/commit/eb95ac6f6ac0bc072450a12f636ee19544201ae2) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty puppet package and contacts API

  - New `@agent-wechat/wechaty-puppet` package: bridges Wechaty bots to WeChat via the agent-wechat server
  - New `GET /api/contacts` endpoint: queries contact.db for full address book
  - New CLI commands: `contacts list` and `contacts find`
