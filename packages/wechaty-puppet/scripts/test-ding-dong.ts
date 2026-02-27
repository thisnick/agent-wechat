/**
 * Ding-dong bot via Wechaty.
 * Replies "dong" to any message containing "ding".
 *
 * Usage:
 *   node --import tsx scripts/test-ding-dong.ts
 *   node --import tsx scripts/test-ding-dong.ts --url http://remote:6174 --token abc123
 *
 * Requires: npm install wechaty (peer dep)
 */
import { WechatyBuilder } from 'wechaty'
import { PuppetAgentWeChat } from '../src/mod.js'

const args = process.argv.slice(2)
function getArg(name: string): string | undefined {
  const idx = args.indexOf(`--${name}`)
  return idx >= 0 ? args[idx + 1] : undefined
}

const serverUrl = getArg('url') ?? process.env.AGENT_WECHAT_URL
const token = getArg('token') ?? process.env.AGENT_WECHAT_TOKEN

const bot = WechatyBuilder.build({
  puppet: new PuppetAgentWeChat({
    ...(serverUrl ? { serverUrl } : {}),
    ...(token ? { token } : {}),
  }),
})

bot.on('scan', (qrcode, status) => {
  console.log(`[scan] status=${status}`)
  if (qrcode) {
    console.log(`Scan QR Code to login: https://wechaty.js.org/qrcode/${encodeURIComponent(qrcode)}`)
  }
})

bot.on('login', (user) => {
  console.log(`[login] ${user.name()} (${user.id})`)
})

bot.on('logout', (user) => {
  console.log(`[logout] ${user.name()}`)
})

bot.on('message', async (msg) => {
  const talker = msg.talker()
  const room = msg.room()
  const text = msg.text()

  const from = room
    ? `${await room.topic()} / ${talker.name()}`
    : talker.name()

  console.log(`[message] ${from}: ${text?.slice(0, 100)}`)

  if (text && /\bding\b/i.test(text)) {
    console.log(`  -> sending dong`)
    await msg.say('dong')
  }
})

bot.on('error', (error) => {
  console.log(`[error] ${error.message}`)
})

console.log('Starting ding-dong bot...')
console.log(`  serverUrl: ${serverUrl ?? 'http://localhost:6174 (default)'}`)
console.log(`  token: ${token ? '***' : '(auto from file)'}`)
console.log()

await bot.start()
console.log('Bot running. Send "ding" to get "dong". Ctrl+C to stop.')
