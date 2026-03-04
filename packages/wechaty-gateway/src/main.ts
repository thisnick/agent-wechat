import { PuppetServer } from 'wechaty-puppet-service'
import { PuppetAgentWeChat } from '@agent-wechat/wechaty-puppet'

const port = process.env['WECHATY_PUPPET_SERVER_PORT'] || '8788'
const rawToken = process.env['WECHATY_TOKEN']

if (!rawToken) {
  console.error('WECHATY_TOKEN is required')
  process.exit(1)
}

// wechaty-puppet-service requires an SNI prefix in the token (since v0.30).
// When TLS is disabled (gateway behind Caddy), use "insecure_" prefix.
const token = rawToken.includes('_') ? rawToken : `insecure_${rawToken}`

const puppet = new PuppetAgentWeChat({
  serverUrl: process.env['AGENT_WECHAT_URL'] || 'http://localhost:6174',
  token: process.env['AGENT_WECHAT_TOKEN'],
})

const server = new PuppetServer({
  endpoint: `0.0.0.0:${port}`,
  puppet,
  token,
  tls: { disable: true },
})

await server.start()
console.log(`Wechaty gateway listening on port ${port}`)

// Graceful shutdown
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, async () => {
    console.log(`Received ${signal}, shutting down...`)
    await server.stop()
    process.exit(0)
  })
}
