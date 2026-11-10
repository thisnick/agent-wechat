# Simplified Chinese glossary

Terms for translating the app (`packages/client-ui/locales/zh-CN.json`) and the docs
(`docs/src/content/docs/zh-cn/`). Where WeChat has its own wording, use the official
WeChat client's Simplified Chinese so the app reads like WeChat.

## WeChat features

| English | 简体中文 | Notes |
| --- | --- | --- |
| chat, conversation | 聊天, 会话 | 会话 for the list of conversations |
| direct message (DM) | 私聊 | |
| group chat | 群聊 | |
| contact | 联系人 | |
| Official Account | 公众号 | |
| service account | 服务号 | Never use 服务 alone for the hub |
| Moments | 朋友圈 | |
| mute notifications (a chat) | 消息免打扰 | 静音 only for a microphone |
| pin a chat | 置顶聊天 | |
| recall a message | 撤回 | |
| mention, @ | @ 提醒 | "mention everyone": @所有人 |
| sticker | 表情 | |
| voice message | 语音消息 | |
| File Transfer | 文件传输助手 | |
| group announcement | 群公告 | |
| my alias in this group | 我在本群的昵称 | |
| sign in, sign out | 登录, 退出登录 | |
| QR code | 二维码 | |

## agent-wechat terms

| English | 简体中文 | Notes |
| --- | --- | --- |
| hub, agent-wechat hub | Hub, agent-wechat Hub | Keep the English word |
| daemon | Daemon | Keep the English word |
| access key | 访问密钥 | |
| API token | API 令牌 | |
| AI agent | 智能体 | |
| desktop app | 桌面应用 | |
| simulator | 模拟器 | |
| profile (an account's WeChat data on the hub) | 账号数据 in the app; Profile in the docs | App text is for everyday users; the docs keep the technical term. 数据目录 for the folder itself. Not 配置 ("configuration") or 资料 (a WeChat personal info card) |
| profile name (WeChat nickname) | 个人昵称 | |

## Keep in English

`wx`, OpenClaw, CLI, API, SDK, HTTP, JSON, command names, flags, config keys, field
names and anything inside code.

## Style

- Full-width punctuation in Chinese text: ，。：；？！（）
- A space between Chinese and Latin words or numbers: `连接到 agent-wechat Hub`, `50 MB`
- Address the reader as 你, not 您.
