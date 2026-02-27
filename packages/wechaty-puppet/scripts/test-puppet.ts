/**
 * Test the puppet directly (without Wechaty).
 * Verifies connection, login, and message events.
 *
 * Usage:
 *   node --import tsx scripts/test-puppet.ts
 *   node --import tsx scripts/test-puppet.ts --url http://remote:6174 --token abc123
 */
import { PuppetAgentWeChat } from '../src/mod.js'

const args = process.argv.slice(2)
function getArg(name: string): string | undefined {
  const idx = args.indexOf(`--${name}`)
  return idx >= 0 ? args[idx + 1] : undefined
}

const serverUrl = getArg('url') ?? process.env.AGENT_WECHAT_URL
const token = getArg('token') ?? process.env.AGENT_WECHAT_TOKEN
const timeoutSec = Number(getArg('timeout') ?? '60')

const puppet = new PuppetAgentWeChat({
  ...(serverUrl ? { serverUrl } : {}),
  ...(token ? { token } : {}),
  pollIntervalMs: 2000,
})

puppet.on('scan', (payload) => {
  console.log(`[scan] status=${payload.status}`)
  if (payload.qrcode) {
    console.log(`Scan QR Code to login: https://wechaty.js.org/qrcode/${encodeURIComponent(payload.qrcode)}`)
  }
})

puppet.on('login', (payload) => {
  console.log(`[login] contactId=${payload.contactId}`)
})

puppet.on('logout', (payload) => {
  console.log(`[logout] contactId=${payload.contactId} data=${payload.data}`)
})

puppet.on('message', (payload) => {
  console.log(`[message] messageId=${payload.messageId}`)
  puppet.messageRawPayload(payload.messageId).then(msg => {
    console.log(`  talkerId=${msg.talkerId} type=${msg.type} text=${msg.text?.slice(0, 80)}`)
    if ('roomId' in msg) console.log(`  roomId=${msg.roomId}`)
  }).catch(() => {})
})

puppet.on('dong', (payload) => {
  console.log(`[dong] data=${payload.data}`)
})

puppet.on('error', (payload) => {
  console.log(`[error] ${payload.data}`)
})

puppet.on('ready', () => {
  console.log('[ready]')
  // Test ding/dong
  puppet.ding('test')
})

console.log(`Starting puppet... (timeout=${timeoutSec}s)`)
console.log(`  serverUrl: ${serverUrl ?? 'http://localhost:6174 (default)'}`)
console.log(`  token: ${token ? '***' : '(auto from file)'}`)
console.log()

await puppet.start()

setTimeout(async () => {
  console.log('\nTimeout reached, stopping...')
  await puppet.stop()
  process.exit(0)
}, timeoutSec * 1000)
