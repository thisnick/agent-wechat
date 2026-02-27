# @agent-wechat/wechaty-puppet

Wechaty Puppet for [agent-wechat](https://github.com/thisnick/agent-wechat). Bridges any Wechaty bot to WeChat via the agent-wechat REST/WebSocket server.

## Prerequisites

- **An agent-wechat server** running — set up via the CLI or from the [agent-wechat repo](https://github.com/thisnick/agent-wechat):
  ```bash
  npx @agent-wechat/cli up     # starts the Docker container
  npx @agent-wechat/cli login   # scan QR to log in
  ```
- **Node.js >= 22**

## Install

```bash
npm install @agent-wechat/wechaty-puppet wechaty wechaty-puppet
```

## Usage

```ts
import { WechatyBuilder } from 'wechaty'
import PuppetAgentWeChat from '@agent-wechat/wechaty-puppet'

const bot = WechatyBuilder.build({
  puppet: new PuppetAgentWeChat({
    serverUrl: 'http://localhost:6174',  // optional, this is the default
    token: 'your-token',                 // optional, reads from ~/.config/agent-wechat/token
  })
})

bot.on('scan', (qrcode, status) => {
  console.log(`Scan QR Code to login: ${status}\nhttps://wechaty.js.org/qrcode/${encodeURIComponent(qrcode)}`)
})

bot.on('login', user => console.log(`Logged in: ${user}`))

bot.on('message', async msg => {
  if (msg.text() === 'ding') {
    await msg.say('dong')
  }
})

await bot.start()
```

## Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `serverUrl` | string | `http://localhost:6174` | agent-wechat server URL |
| `token` | string | auto | Auth token. Falls back to `AGENT_WECHAT_TOKEN` env var, then `~/.config/agent-wechat/token` |
| `pollIntervalMs` | number | `2000` | Message polling interval in milliseconds |

## Supported Features

| Feature | Status | Notes |
|---------|--------|-------|
| QR Login | Yes | Via WebSocket `/api/ws/login` |
| Logout | Yes | |
| Receive Text | Yes | Polling-based |
| Receive Image | Yes | Via media endpoint |
| Receive Voice | Yes | Via media endpoint |
| Receive Video | Yes | Via media endpoint |
| Receive Emoticon | Yes | |
| Send Text | Yes | |
| Send Image | Yes | |
| Send File | Yes | |
| Contact List | Yes | Full address book via `/api/contacts` |
| Contact Alias | Read | From remark field |
| Room List | Partial | Recent group chats only |
| Room Topic | Read | From group name |
| Forward Text | Yes | |
| Send URL | Partial | Sent as plain text |
| Message Recall | No | |
| Room Create | No | |
| Room Add/Remove | No | |
| Friendship | No | |
| Tags | No | |
| Moments/Posts | No | |

## How It Works

The puppet connects to the agent-wechat REST API (Rust server running inside a Docker container alongside WeChat desktop). It does not interact with WeChat directly.

- **Login**: WebSocket subscription to `/api/ws/login` drives QR scan events
- **Messages**: Polls `GET /api/chats` for unreads, then `GET /api/messages/{chatId}` for new messages
- **Sending**: `POST /api/messages/send` with text, image, or file payloads
- **Media**: `GET /api/messages/{chatId}/media/{localId}` for image/voice/video downloads
- **Contacts**: Full address book via `GET /api/contacts`
- **Rooms**: Derived from `GET /api/chats` (group chats)
