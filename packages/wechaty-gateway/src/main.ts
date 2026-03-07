import { PuppetServer } from 'wechaty-puppet-service'
import { PuppetAgentWeChat } from '@agent-wechat/wechaty-puppet'
import WebSocket from 'ws'

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

// Register with chatie.io service discovery so clients can resolve token → endpoint.
// The public port (e.g. 8443) is what Caddy exposes; the registry returns our public IP + this port.
const publicPort = parseInt(process.env['WECHATY_PUPPET_PUBLIC_PORT'] || '8443', 10)
registerWithChatie(token, publicPort)

// Graceful shutdown
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, async () => {
    console.log(`Received ${signal}, shutting down...`)
    await server.stop()
    process.exit(0)
  })
}

function registerWithChatie(registryToken: string, grpcPort: number) {
  const endpoint = 'wss://api.chatie.io/v0/websocket'
  const protocol = `io|agent-wechat|0.0.0.0|${grpcPort}`

  let reconnectTimer: ReturnType<typeof setTimeout> | undefined

  function connect() {
    const ws = new WebSocket(endpoint, protocol, {
      headers: { Authorization: `Token ${registryToken}` },
    })

    ws.on('open', () => {
      console.log('[registry] registered with chatie.io (port %d)', grpcPort)
    })

    ws.on('message', (data) => {
      try {
        const msg = JSON.parse(data.toString())

        // Direct JSON-RPC request
        if (msg.method === 'getHostieGrpcPort' && msg.id !== undefined) {
          ws.send(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: grpcPort }))
          return
        }

        // Wrapped io event with jsonrpc payload
        if (msg.name === 'jsonrpc' && msg.payload?.method === 'getHostieGrpcPort' && msg.payload?.id !== undefined) {
          ws.send(JSON.stringify({
            name: 'jsonrpc',
            payload: { jsonrpc: '2.0', id: msg.payload.id, result: grpcPort },
          }))
        }
      } catch {
        // not JSON, ignore
      }
    })

    ws.on('close', () => {
      console.log('[registry] disconnected, reconnecting in 10s...')
      reconnectTimer = setTimeout(connect, 10_000)
    })

    ws.on('error', (err) => {
      console.error('[registry] error:', err.message)
    })

    // Keep-alive ping every 30s
    const pingInterval = setInterval(() => {
      if (ws.readyState === WebSocket.OPEN) ws.ping()
    }, 30_000)

    ws.on('close', () => clearInterval(pingInterval))

    // Clean up on process exit
    const cleanup = () => {
      clearInterval(pingInterval)
      clearTimeout(reconnectTimer)
      ws.close()
    }
    process.on('SIGINT', cleanup)
    process.on('SIGTERM', cleanup)
  }

  connect()
}
