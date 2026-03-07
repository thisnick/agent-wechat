import { PuppetServer } from 'wechaty-puppet-service'
import { PuppetAgentWeChat } from '@agent-wechat/wechaty-puppet'
import WebSocket from 'ws'

const port = process.env['WECHATY_PUPPET_SERVER_PORT'] || '8788'
const rawToken = process.env['AGENT_WECHAT_TOKEN']
const host = process.env['WECHATY_HOST']

if (!rawToken) {
  console.error('AGENT_WECHAT_TOKEN is required')
  process.exit(1)
}

// wechaty-puppet-service requires an SNI prefix in the token (since v0.30).
// When behind Caddy, use the public hostname so clients can verify the TLS cert.
// Falls back to "insecure_" for local dev.
const token = host ? `${host}_${rawToken}` : `insecure_${rawToken}`

const puppet = new PuppetAgentWeChat({
  serverUrl: process.env['AGENT_WECHAT_URL'] || 'http://localhost:6174',
  token: rawToken,
})

const server = new PuppetServer({
  endpoint: `0.0.0.0:${port}`,
  puppet,
  token,
  tls: { disable: true },
})

await server.start()
console.log(`Wechaty gateway listening on port ${port}`)

// Register with chatie.io service discovery if we have a public hostname.
if (host) {
  const publicPort = parseInt(process.env['WECHATY_PUPPET_PUBLIC_PORT'] || '8443', 10)
  registerWithChatie(token, host, publicPort)
}

// Graceful shutdown
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, async () => {
    console.log(`Received ${signal}, shutting down...`)
    await server.stop()
    process.exit(0)
  })
}

function registerWithChatie(registryToken: string, serviceHost: string, grpcPort: number) {
  const endpoint = 'wss://api.chatie.io/v0/websocket'
  // Protocol format: io|{version}|{id}|{serviceHost}|{servicePort}
  const protocol = `io|0.0.1|agent-wechat|${serviceHost}|${grpcPort}`

  let reconnectTimer: ReturnType<typeof setTimeout> | undefined

  function connect() {
    const ws = new WebSocket(endpoint, protocol, {
      headers: { Authorization: `Token ${registryToken}` },
    })

    ws.on('open', () => {
      console.log('[registry] registered with chatie.io (%s:%d)', serviceHost, grpcPort)
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
