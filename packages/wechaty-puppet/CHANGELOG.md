# @agent-wechat/wechaty-puppet

## 0.8.2

## 0.8.1

### Patch Changes

- [#63](https://github.com/thisnick/agent-wechat/pull/63) [`67f92fe`](https://github.com/thisnick/agent-wechat/commit/67f92fee2a4e1f1440b7f6982f6962e6652e3dd5) Thanks [@thisnick](https://github.com/thisnick)! - Fix login event handling — QR scan events were not emitted due to incorrect event discriminator

## 0.8.0

### Minor Changes

- [#59](https://github.com/thisnick/agent-wechat/pull/59) [`eb95ac6`](https://github.com/thisnick/agent-wechat/commit/eb95ac6f6ac0bc072450a12f636ee19544201ae2) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty puppet package and contacts API

  - New `@agent-wechat/wechaty-puppet` package: bridges Wechaty bots to WeChat via the agent-wechat server
  - New `GET /api/contacts` endpoint: queries contact.db for full address book
  - New CLI commands: `contacts list` and `contacts find`
