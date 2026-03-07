/**
 * Test gateway connectivity (remote, TLS via HTTPS proxy).
 *
 * Usage:
 *   pnpm gateway:test:remote --endpoint nick.agent-wx.app:8443 [--token TOKEN]
 *
 * Connects to a gateway behind an HTTPS-terminating proxy (e.g. Caddy, Cloudflare).
 * The proxy handles TLS; the gateway server itself runs plain gRPC behind it.
 *
 * Token is read from: --token flag, WECHATY_TOKEN env, or ~/.config/agent-wechat/token
 */

// Override wechaty's bundled self-signed CA cert with system CAs so gRPC can
// verify Let's Encrypt certificates from Caddy. Must be set before imports.
import { existsSync, readFileSync as readFileSyncEarly } from 'fs'
for (const p of ['/etc/ssl/cert.pem', '/etc/ssl/certs/ca-certificates.crt']) {
  if (existsSync(p)) {
    process.env['WECHATY_PUPPET_SERVICE_TLS_CA_CERT'] = readFileSyncEarly(p, 'utf-8')
    break
  }
}

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

const endpoint = getArg('endpoint')
if (!endpoint) {
  console.error('--endpoint is required (e.g. --endpoint nick-wechaty.agent-wx.app:443)')
  process.exit(1)
}

const host = endpoint.split(':')[0]!

const puppet = new PuppetService({
  token: token,
  endpoint,
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

console.log('Connecting to gateway (remote, TLS)...')
console.log(`  endpoint:   ${endpoint}`)
console.log(`  serverName: ${host}`)
console.log(`  token:      ${token === 'test' ? 'test' : '***'}`)
console.log(`  GRPC_DEFAULT_SSL_ROOTS_FILE_PATH: ${process.env['GRPC_DEFAULT_SSL_ROOTS_FILE_PATH']}`)
console.log()

await puppet.start()
console.log('Connected. Sending ding...')
puppet.ding('hello')
console.log('Waiting for events. Ctrl+C to stop.')
