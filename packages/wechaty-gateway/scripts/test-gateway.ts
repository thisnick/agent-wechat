/**
 * Local gateway connectivity test.
 *
 * Step 1 — Start the gateway (in one terminal):
 *   AGENT_WECHAT_URL=http://localhost:6174 \
 *   AGENT_WECHAT_TOKEN=test \
 *   WECHATY_TOKEN=test \
 *   pnpm --filter @agent-wechat/wechaty-gateway exec -- node --import tsx src/main.ts
 *
 * Step 2 — Run this client (in another terminal):
 *   pnpm --filter @agent-wechat/wechaty-gateway exec -- node --import tsx scripts/test-gateway.ts
 */
import { readFileSync } from 'fs'
import { join } from 'path'
import { homedir } from 'os'
import { PuppetService } from 'wechaty-puppet-service'

const args = process.argv.slice(2)
function getArg(name: string): string | undefined {
  const idx = args.indexOf(`--${name}`)
  return idx >= 0 ? args[idx + 1] : undefined
}

function loadLocalToken(): string | undefined {
  try {
    return readFileSync(join(homedir(), '.config', 'agent-wechat', 'token'), 'utf-8').trim()
  } catch {
    return undefined
  }
}

const token = getArg('token') ?? process.env['WECHATY_TOKEN'] ?? loadLocalToken()
if (!token) {
  console.error('No token found. Set WECHATY_TOKEN or create ~/.config/agent-wechat/token')
  process.exit(1)
}
const endpoint = getArg('endpoint') ?? '127.0.0.1:8788'

// insecure_ prefix is required as SNI identifier; tls.disable actually disables TLS on client
const insecureToken = token.startsWith('insecure_') ? token : `insecure_${token}`

const puppet = new PuppetService({
  token: insecureToken,
  endpoint,
  tls: { disable: true },
})

puppet.on('scan', (payload) => {
  console.log(`[scan] status=${payload.status}`)
  if (payload.qrcode) {
    console.log(`  QR: https://wechaty.js.org/qrcode/${encodeURIComponent(payload.qrcode)}`)
  }
})

puppet.on('login', (payload) => {
  console.log(`[login] ${payload.contactId}`)
})

puppet.on('message', async (payload) => {
  try {
    const msg = await puppet.messagePayload(payload.messageId)
    const text = msg.text ?? ''
    const conversationId = msg.roomId || msg.talkerId || ''
    console.log(`[message] id=${payload.messageId} from=${msg.talkerId} room=${msg.roomId} conv=${conversationId} text=${text.slice(0, 100)}`)

    if (/\bding\b/i.test(text)) {
      console.log('  -> sending dong to %s', conversationId)
      try {
        await puppet.messageSendText(conversationId, 'dong')
        console.log('  -> dong sent')
      } catch (sendErr) {
        console.log('  -> send failed:', sendErr)
      }
    }
  } catch (err) {
    console.log(`[message] id=${payload.messageId} (failed to load: ${err})`)
  }
})

puppet.on('logout', (payload) => {
  console.log(`[logout] ${payload.contactId} reason=${payload.data}`)
})

puppet.on('error', (payload) => {
  console.log(`[error]`, JSON.stringify(payload))
})

puppet.on('dong', (payload) => {
  console.log(`[dong] ${payload.data}`)
})

console.log('Connecting to gateway via gRPC...')
console.log(`  endpoint: ${endpoint}`)
console.log(`  token: ${token === 'test' ? 'test' : '***'}`)
console.log()

await puppet.start()
console.log('Connected. Sending ding...')
puppet.ding('hello')
console.log('Waiting for events. Ctrl+C to stop.')
