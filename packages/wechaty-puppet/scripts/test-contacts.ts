/**
 * List contacts and rooms from the puppet.
 * Useful for verifying the puppet can read data from the server.
 *
 * Usage:
 *   node --import tsx scripts/test-contacts.ts
 *   node --import tsx scripts/test-contacts.ts --url http://remote:6174 --token abc123
 */
import { PuppetAgentWeChat } from '../src/mod.js'

const args = process.argv.slice(2)
function getArg(name: string): string | undefined {
  const idx = args.indexOf(`--${name}`)
  return idx >= 0 ? args[idx + 1] : undefined
}

const serverUrl = getArg('url') ?? process.env.AGENT_WECHAT_URL
const token = getArg('token') ?? process.env.AGENT_WECHAT_TOKEN

const puppet = new PuppetAgentWeChat({
  ...(serverUrl ? { serverUrl } : {}),
  ...(token ? { token } : {}),
})

puppet.on('login', async (payload) => {
  console.log(`Logged in as: ${payload.contactId}\n`)

  // List contacts
  const contactIds = await puppet.contactList()
  console.log(`Contacts (${contactIds.length}):`)
  for (const id of contactIds.slice(0, 20)) {
    const contact = await puppet.contactRawPayload(id)
    const alias = contact.alias ? ` (${contact.alias})` : ''
    console.log(`  ${contact.name}${alias}  [${id}]`)
  }
  if (contactIds.length > 20) {
    console.log(`  ... and ${contactIds.length - 20} more`)
  }

  console.log()

  // List rooms
  const roomIds = await puppet.roomList()
  console.log(`Rooms (${roomIds.length}):`)
  for (const id of roomIds.slice(0, 20)) {
    const room = await puppet.roomRawPayload(id)
    console.log(`  ${room.topic}  [${id}]`)
  }
  if (roomIds.length > 20) {
    console.log(`  ... and ${roomIds.length - 20} more`)
  }

  console.log('\nDone.')
  await puppet.stop()
  process.exit(0)
})

puppet.on('error', (payload) => {
  console.error(`Error: ${payload.data}`)
})

puppet.on('scan', (payload) => {
  console.log(`Not logged in (scan status=${payload.status}). Log in first via: pnpm cli auth login`)
  process.exit(1)
})

await puppet.start()

// Timeout if login doesn't happen
setTimeout(async () => {
  console.error('Timed out waiting for login')
  await puppet.stop()
  process.exit(1)
}, 15000)
