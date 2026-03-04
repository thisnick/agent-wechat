import * as PUPPET from 'wechaty-puppet'
import { FileBox } from 'file-box'
import { readFileSync } from 'fs'
import { join } from 'path'
import { homedir } from 'os'
import { createRequire } from 'module'

import { WeChatClient } from '@agent-wechat/shared'
import type { Chat, Contact, Message, LoginSubscriptionEvent } from '@agent-wechat/shared'

const require = createRequire(import.meta.url)
const { version: VERSION } = require('../package.json')
import {
  chatToContactPayload,
  contactToContactPayload,
  chatToRoomPayload,
  messageToPayload,
} from './type-map.js'

const { log } = PUPPET

export interface PuppetAgentWeChatOptions extends PUPPET.PuppetOptions {
  serverUrl?: string
  token?: string
  pollIntervalMs?: number
}

export class PuppetAgentWeChat extends PUPPET.Puppet {

  static override readonly VERSION = VERSION

  private client!: WeChatClient
  private pollTimer?: ReturnType<typeof setInterval>
  private loginHandle?: { close: () => void }
  private loginTerminalSeen = false
  private loginFailureEmitted = false
  private lastSeenId = new Map<string, number>()

  // In-memory stores
  private contactStore = new Map<string, PUPPET.payloads.Contact>()
  private roomStore = new Map<string, PUPPET.payloads.Room>()
  private messageStore = new Map<string, PUPPET.payloads.Message>()
  // Keep raw messages for media lookup (need chatId + localId)
  private rawMessageStore = new Map<string, Message>()

  private get serverUrl(): string {
    return (this.options as PuppetAgentWeChatOptions).serverUrl
      ?? process.env['AGENT_WECHAT_URL']
      ?? 'http://localhost:6174'
  }

  private get token(): string | undefined {
    const optToken = (this.options as PuppetAgentWeChatOptions).token
    if (optToken) return optToken

    const envToken = process.env['AGENT_WECHAT_TOKEN']
    if (envToken) return envToken

    try {
      return readFileSync(join(homedir(), '.config', 'agent-wechat', 'token'), 'utf-8').trim()
    } catch {
      return undefined
    }
  }

  private get pollIntervalMs(): number {
    return (this.options as PuppetAgentWeChatOptions).pollIntervalMs ?? 2000
  }

  constructor(options: PuppetAgentWeChatOptions = {}) {
    super(options)
    log.verbose('PuppetAgentWeChat', 'constructor(%s)', JSON.stringify(options))
  }

  override name() { return '@agent-wechat/wechaty-puppet' }
  override version() { return VERSION }

  // ==================
  // Lifecycle
  // ==================

  override async onStart(): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'onStart()')

    this.client = new WeChatClient({
      baseUrl: this.serverUrl,
      token: this.token,
    })

    // Check if already logged in on the agent-wechat server
    try {
      const auth = await this.client.authStatus()
      if (auth.status === 'logged_in' && auth.loggedInUser) {
        log.info('PuppetAgentWeChat', 'Already logged in as %s', auth.loggedInUser)
        await this.loadContacts()
        // Guard: PuppetServer may restart the puppet on client reconnect
        // while currentUserId is still set from the previous session
        if (!this.isLoggedIn) {
          await super.login(auth.loggedInUser)
        }
        await this.snapshotBaseline()
        this.startPolling()
        this.emit('ready', { data: 'ready' })
        return
      }
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'Auth check failed: %s', err)
    }

    // Not logged in — start login subscription
    this.startLoginSubscription()
  }

  override async onStop(): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'onStop()')

    this.stopPolling()

    if (this.loginHandle) {
      this.loginHandle.close()
      this.loginHandle = undefined
    }

    this.contactStore.clear()
    this.roomStore.clear()
    this.messageStore.clear()
    this.rawMessageStore.clear()
    this.lastSeenId.clear()
  }

  // ==================
  // Login
  // ==================

  private startLoginSubscription(): void {
    log.verbose('PuppetAgentWeChat', 'startLoginSubscription()')
    this.loginTerminalSeen = false
    this.loginFailureEmitted = false

    this.loginHandle = this.client.loginSubscribe({
      timeoutMs: 300_000,
      onEvent: (event: LoginSubscriptionEvent) => {
        this.handleLoginEvent(event)
      },
      onError: (err: Error) => {
        const message = err.message?.trim() || 'WebSocket error'
        const settled = this.loginTerminalSeen || Boolean(this.currentUserId)
        if (settled) {
          log.verbose('PuppetAgentWeChat', 'Ignoring login WS error after terminal state: %s', message)
          return
        }
        log.error('PuppetAgentWeChat', 'Login WS error: %s', message)
        this.emitLoginFailure(message)
        this.loginTerminalSeen = true
      },
      onClose: () => {
        log.verbose('PuppetAgentWeChat', 'Login WS closed')
        this.loginHandle = undefined
        const settled = this.loginTerminalSeen || Boolean(this.currentUserId)
        if (!settled) {
          const message = 'Login connection closed before completion'
          log.error('PuppetAgentWeChat', message)
          this.emitLoginFailure(message)
          this.loginTerminalSeen = true
        }
      },
    })
  }

  private emitLoginFailure(message: string): void {
    if (this.loginFailureEmitted) return
    this.loginFailureEmitted = true
    this.emit('error', { data: message })
  }

  private handleLoginEvent(event: LoginSubscriptionEvent): void {
    switch (event.type) {
      case 'qr': {
        const qrcode = event.qrData ?? event.qrDataUrl ?? ''
        this.emit('scan', {
          qrcode,
          status: PUPPET.types.ScanStatus.Waiting,
        })
        break
      }
      case 'phone_confirm':
        this.emit('scan', {
          qrcode: '',
          status: PUPPET.types.ScanStatus.Scanned,
        })
        break
      case 'login_success':
        this.loginTerminalSeen = true
        if (this.loginHandle) {
          this.loginHandle.close()
          this.loginHandle = undefined
        }
        void this.onLoginSuccess(event.userId ?? 'unknown')
        break
      case 'login_timeout':
        this.loginTerminalSeen = true
        this.emitLoginFailure('Login timed out')
        break
      case 'error':
        this.loginTerminalSeen = true
        this.emitLoginFailure(event.message ?? 'Login error')
        break
      case 'status':
        log.verbose('PuppetAgentWeChat', 'Login status: %s', event.message)
        break
    }
  }

  private async onLoginSuccess(userId: string): Promise<void> {
    log.info('PuppetAgentWeChat', 'Login success: %s', userId)
    await this.loadContacts()
    await super.login(userId)
    await this.snapshotBaseline()
    this.startPolling()
    this.emit('ready', { data: 'ready' })
  }

  // ==================
  // Polling
  // ==================

  private async snapshotBaseline(): Promise<void> {
    try {
      const chats = await this.client.listChats(200)
      for (const chat of chats) {
        if (chat.lastMsgLocalId) {
          this.lastSeenId.set(chat.username, chat.lastMsgLocalId)
        }
      }
      log.info('PuppetAgentWeChat', 'Baseline snapshot: %d chats', this.lastSeenId.size)
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'snapshotBaseline error: %s', err)
    }
  }

  private startPolling(): void {
    if (this.pollTimer) return
    log.verbose('PuppetAgentWeChat', 'startPolling(%dms)', this.pollIntervalMs)

    this.pollTimer = setInterval(() => {
      void this.pollMessages()
    }, this.pollIntervalMs)
  }

  private stopPolling(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer)
      this.pollTimer = undefined
    }
  }

  private async pollMessages(): Promise<void> {
    try {
      const auth = await this.client.authStatus()
      if (auth.status !== 'logged_in' || !auth.loggedInUser) {
        log.warn('PuppetAgentWeChat', 'Account logged out, stopping poll')
        this.stopPolling()
        await this.logout('WeChat logged out')
        return
      }
      if (auth.loggedInUser !== this.currentUserId) {
        log.warn('PuppetAgentWeChat', 'User changed from %s to %s', this.currentUserId, auth.loggedInUser)
        this.stopPolling()
        await this.logout('User changed')
        return
      }

      const chats = await this.client.listChats(50)

      // Update contact/room stores
      for (const chat of chats) {
        if (chat.isGroup) {
          this.roomStore.set(chat.username, chatToRoomPayload(chat))
        } else {
          this.contactStore.set(chat.username, chatToContactPayload(chat))
        }
      }

      // Find chats with unreads
      const unreadChats = chats.filter(
        c => c.unreadCount > 0 && !c.username.startsWith('gh_'),
      )

      for (const chat of unreadChats) {
        await this.processUnreadChat(chat)
      }

      // Catch-up: check tracked chats where lastMsgLocalId advanced
      for (const chat of chats) {
        if (chat.username.startsWith('gh_')) continue
        const prevSeen = this.lastSeenId.get(chat.username)
        if (prevSeen === undefined) continue
        if (unreadChats.some(c => c.username === chat.username)) continue
        if (!chat.lastMsgLocalId || chat.lastMsgLocalId <= prevSeen) continue

        await this.processUnreadChat(chat)
      }
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'pollMessages error: %s', err)
    }

    // Feed the watchdog so gRPC clients don't timeout
    this.emit('heartbeat', { data: 'poll' })
  }

  private async processUnreadChat(chat: Chat): Promise<void> {
    const chatId = chat.username
    const firstPoll = !this.lastSeenId.has(chatId)
    const prevLastSeen = this.lastSeenId.get(chatId) ?? 0
    const fetchLimit = Math.max(chat.unreadCount, 20)

    let messages: Message[]
    try {
      messages = await this.client.listMessages(chatId, fetchLimit)
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'Failed to list messages for %s: %s', chatId, err)
      return
    }

    if (messages.length === 0) return

    let newMessages: Message[]
    if (firstPoll) {
      messages.sort((a, b) => a.localId - b.localId)
      const unread = chat.unreadCount ?? 0
      if (unread > 0 && unread < messages.length) {
        newMessages = messages.slice(-unread)
        const seenMax = messages[messages.length - unread - 1].localId
        this.lastSeenId.set(chatId, seenMax)
      } else if (unread >= messages.length) {
        newMessages = messages
      } else {
        const maxId = messages[messages.length - 1].localId
        this.lastSeenId.set(chatId, maxId)
        return
      }
    } else {
      newMessages = messages.filter(m => m.localId > prevLastSeen)
      if (newMessages.length === 0) return
      newMessages.sort((a, b) => a.localId - b.localId)
    }

    const selfId = this.currentUserId

    for (const msg of newMessages) {
      // Skip self-sent messages
      if (msg.isSelf) continue

      const payload = messageToPayload(msg, selfId)
      const messageId = payload.id
      this.messageStore.set(messageId, payload)
      this.rawMessageStore.set(messageId, msg)
      this.emit('message', { messageId })
    }

    const maxId = Math.max(...newMessages.map(m => m.localId))
    this.lastSeenId.set(chatId, maxId)

    // Clear unreads after processing
    try {
      await this.client.openChat(chatId, true)
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'Failed to clear unreads for %s: %s', chatId, err)
    }
  }

  private async loadContacts(): Promise<void> {
    // Load full contacts from address book
    try {
      const contacts = await this.client.listContacts(5000)
      for (const contact of contacts) {
        this.contactStore.set(contact.username, contactToContactPayload(contact))
      }
      log.info('PuppetAgentWeChat', 'Loaded %d contacts from address book', contacts.length)
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'listContacts not available, falling back to chats: %s', err)
    }

    // Load rooms from recent chats (rooms aren't in contacts endpoint)
    try {
      const chats = await this.client.listChats(200)
      for (const chat of chats) {
        if (chat.isGroup) {
          this.roomStore.set(chat.username, chatToRoomPayload(chat))
        } else if (!this.contactStore.has(chat.username)) {
          this.contactStore.set(chat.username, chatToContactPayload(chat))
        }
      }
    } catch (err) {
      log.warn('PuppetAgentWeChat', 'loadContacts chats error: %s', err)
    }
  }

  // ==================
  // Misc
  // ==================

  override ding(data?: string): void {
    log.verbose('PuppetAgentWeChat', 'ding(%s)', data ?? '')
    void this.client.status()
      .then(() => this.emit('dong', { data: data ?? '' }))
      .catch(err => this.emit('error', { data: String(err) }))
  }

  // ==================
  // Contact
  // ==================

  override async contactList(): Promise<string[]> {
    return [...this.contactStore.keys()]
  }

  override async contactRawPayload(contactId: string): Promise<PUPPET.payloads.Contact> {
    let payload = this.contactStore.get(contactId)
    if (payload) return payload

    // Try fetching from server
    try {
      const chat = await this.client.getChat(contactId)
      if (chat && !chat.isGroup) {
        payload = chatToContactPayload(chat)
        this.contactStore.set(contactId, payload)
        return payload
      }
    } catch { /* fall through */ }

    return {
      id: contactId,
      name: contactId,
      gender: PUPPET.types.ContactGender.Unknown,
      type: PUPPET.types.Contact.Individual,
      avatar: '',
      phone: [],
    }
  }

  override async contactRawPayloadParser(rawPayload: PUPPET.payloads.Contact): Promise<PUPPET.payloads.Contact> {
    return rawPayload
  }

  override async contactAlias(contactId: string): Promise<string>
  override async contactAlias(contactId: string, alias: string | null): Promise<void>
  override async contactAlias(contactId: string, alias?: string | null): Promise<string | void> {
    if (alias !== undefined) {
      log.verbose('PuppetAgentWeChat', 'contactAlias(%s, %s) not supported (read-only)', contactId, alias)
      return
    }
    const payload = await this.contactRawPayload(contactId)
    return payload.alias ?? ''
  }

  override async contactAvatar(contactId: string): Promise<FileBox>
  override async contactAvatar(contactId: string, file: FileBox): Promise<void>
  override async contactAvatar(contactId: string, file?: FileBox): Promise<FileBox | void> {
    if (file) {
      log.verbose('PuppetAgentWeChat', 'contactAvatar(%s, file) not supported', contactId)
      return
    }
    return FileBox.fromUrl('https://via.placeholder.com/100', { name: `${contactId}.png` })
  }

  override async contactPhone(contactId: string, phoneList: string[]): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'contactPhone(%s) not supported', contactId)
  }

  override async contactCorporationRemark(contactId: string, corporationRemark: string | null): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'contactCorporationRemark(%s) not supported', contactId)
  }

  override async contactDescription(contactId: string, description: string | null): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'contactDescription(%s) not supported', contactId)
  }

  override async contactSelfQRCode(): Promise<string> {
    return ''
  }

  override async contactSelfName(name: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'contactSelfName(%s) not supported', name)
  }

  override async contactSelfSignature(signature: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'contactSelfSignature(%s) not supported', signature)
  }

  // ==================
  // Message
  // ==================

  override async messageRawPayload(messageId: string): Promise<PUPPET.payloads.Message> {
    const payload = this.messageStore.get(messageId)
    if (!payload) throw new Error(`Message not found: ${messageId}`)
    return payload
  }

  override async messageRawPayloadParser(rawPayload: PUPPET.payloads.Message): Promise<PUPPET.payloads.Message> {
    return rawPayload
  }

  override async messageSendText(
    conversationId: string,
    text: string,
    _mentionIdList?: string[],
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendText(%s, %s)', conversationId, text.slice(0, 50))
    const result = await this.client.sendMessage({ chatId: conversationId, text })
    if (!result.success) {
      throw new Error(result.error ?? 'Send failed')
    }
  }

  override async messageSendFile(
    conversationId: string,
    file: FileBox,
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendFile(%s, %s)', conversationId, file.name)

    const buffer = await file.toBuffer()
    const base64 = buffer.toString('base64')
    const mimeType = file.mediaType || 'application/octet-stream'
    const isImage = mimeType.startsWith('image/')

    const result = isImage
      ? await this.client.sendMessage({
          chatId: conversationId,
          image: { data: base64, mimeType },
        })
      : await this.client.sendMessage({
          chatId: conversationId,
          file: { data: base64, filename: file.name },
        })

    if (!result.success) {
      throw new Error(result.error ?? 'Send file failed')
    }
  }

  override async messageImage(
    messageId: string,
    imageType: PUPPET.types.Image,
  ): Promise<FileBox> {
    return this.messageMediaToFileBox(messageId)
  }

  override async messageFile(messageId: string): Promise<FileBox> {
    return this.messageMediaToFileBox(messageId)
  }

  private async messageMediaToFileBox(messageId: string): Promise<FileBox> {
    const raw = this.rawMessageStore.get(messageId)
    if (!raw) throw new Error(`Raw message not found: ${messageId}`)

    const result = await this.client.getMedia(raw.chatId, raw.localId)
    if (!result.data || result.type === 'unsupported') {
      throw new Error(`Media not available for message ${messageId}`)
    }

    const buffer = Buffer.from(result.data, 'base64')
    return FileBox.fromBuffer(buffer, result.filename || `${messageId}.${result.format}`)
  }

  override async messageContact(messageId: string): Promise<string> {
    log.verbose('PuppetAgentWeChat', 'messageContact(%s) not supported', messageId)
    throw PUPPET.throwUnsupportedError()
  }

  override async messageMiniProgram(messageId: string): Promise<PUPPET.payloads.MiniProgram> {
    log.verbose('PuppetAgentWeChat', 'messageMiniProgram(%s) not supported', messageId)
    throw PUPPET.throwUnsupportedError()
  }

  override async messageUrl(messageId: string): Promise<PUPPET.payloads.UrlLink> {
    log.verbose('PuppetAgentWeChat', 'messageUrl(%s) not supported', messageId)
    throw PUPPET.throwUnsupportedError()
  }

  override async messageLocation(messageId: string): Promise<PUPPET.payloads.Location> {
    log.verbose('PuppetAgentWeChat', 'messageLocation(%s) not supported', messageId)
    throw PUPPET.throwUnsupportedError()
  }

  override async messageForward(
    conversationId: string,
    messageId: string,
  ): Promise<void | string> {
    const payload = this.messageStore.get(messageId)
    if (payload?.text) {
      return this.messageSendText(conversationId, payload.text)
    }
    log.verbose('PuppetAgentWeChat', 'messageForward(%s) non-text not supported', messageId)
  }

  override async messageRecall(messageId: string): Promise<boolean> {
    log.verbose('PuppetAgentWeChat', 'messageRecall(%s) not supported', messageId)
    return false
  }

  override async messageSendContact(
    conversationId: string,
    contactId: string,
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendContact() not supported')
  }

  override async messageSendUrl(
    conversationId: string,
    urlLinkPayload: PUPPET.payloads.UrlLink,
  ): Promise<void | string> {
    // Send the URL as plain text
    return this.messageSendText(conversationId, urlLinkPayload.url)
  }

  override async messageSendMiniProgram(
    conversationId: string,
    miniProgramPayload: PUPPET.payloads.MiniProgram,
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendMiniProgram() not supported')
  }

  override async messageSendLocation(
    conversationId: string,
    locationPayload: PUPPET.payloads.Location,
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendLocation() not supported')
  }

  override async messageSendPost(
    conversationId: string,
    postPayload: PUPPET.payloads.Post,
  ): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'messageSendPost() not supported')
  }

  override async conversationReadMark(
    conversationId: string,
    hasRead?: boolean,
  ): Promise<void | boolean> {
    log.verbose('PuppetAgentWeChat', 'conversationReadMark(%s) not supported', conversationId)
  }

  // ==================
  // Room
  // ==================

  override async roomList(): Promise<string[]> {
    return [...this.roomStore.keys()]
  }

  override async roomRawPayload(roomId: string): Promise<PUPPET.payloads.Room> {
    let payload = this.roomStore.get(roomId)
    if (payload) return payload

    try {
      const chat = await this.client.getChat(roomId)
      if (chat && chat.isGroup) {
        payload = chatToRoomPayload(chat)
        this.roomStore.set(roomId, payload)
        return payload
      }
    } catch { /* fall through */ }

    return {
      id: roomId,
      topic: roomId,
      avatar: '',
      memberIdList: [],
      adminIdList: [],
    }
  }

  override async roomRawPayloadParser(rawPayload: PUPPET.payloads.Room): Promise<PUPPET.payloads.Room> {
    return rawPayload
  }

  override async roomAdd(roomId: string, contactId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'roomAdd(%s, %s) not supported', roomId, contactId)
  }

  override async roomDel(roomId: string, contactId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'roomDel(%s, %s) not supported', roomId, contactId)
  }

  override async roomCreate(contactIdList: string[], topic?: string): Promise<string> {
    log.verbose('PuppetAgentWeChat', 'roomCreate() not supported')
    throw PUPPET.throwUnsupportedError()
  }

  override async roomQuit(roomId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'roomQuit(%s) not supported', roomId)
  }

  override async roomQRCode(roomId: string): Promise<string> {
    log.verbose('PuppetAgentWeChat', 'roomQRCode(%s) not supported', roomId)
    throw PUPPET.throwUnsupportedError()
  }

  override async roomTopic(roomId: string): Promise<string>
  override async roomTopic(roomId: string, topic: string): Promise<void>
  override async roomTopic(roomId: string, topic?: string): Promise<string | void> {
    if (topic !== undefined) {
      log.verbose('PuppetAgentWeChat', 'roomTopic(%s, %s) not supported (read-only)', roomId, topic)
      return
    }
    const payload = await this.roomRawPayload(roomId)
    return payload.topic
  }

  override async roomAnnounce(roomId: string): Promise<string>
  override async roomAnnounce(roomId: string, text: string): Promise<void>
  override async roomAnnounce(roomId: string, text?: string): Promise<string | void> {
    log.verbose('PuppetAgentWeChat', 'roomAnnounce(%s) not supported', roomId)
    if (text === undefined) return ''
  }

  override async roomAvatar(roomId: string): Promise<FileBox> {
    return FileBox.fromUrl('https://via.placeholder.com/100', { name: `${roomId}.png` })
  }

  // ==================
  // Room Member
  // ==================

  override async roomMemberList(roomId: string): Promise<string[]> {
    const payload = this.roomStore.get(roomId)
    return payload?.memberIdList ?? []
  }

  override async roomMemberRawPayload(roomId: string, contactId: string): Promise<PUPPET.payloads.RoomMember> {
    const contact = await this.contactRawPayload(contactId)
    return {
      id: contactId,
      avatar: contact.avatar ?? '',
      name: contact.name,
      roomAlias: contact.alias,
    }
  }

  override async roomMemberRawPayloadParser(rawPayload: PUPPET.payloads.RoomMember): Promise<PUPPET.payloads.RoomMember> {
    return rawPayload
  }

  // ==================
  // Room Invitation
  // ==================

  override async roomInvitationAccept(roomInvitationId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'roomInvitationAccept(%s) not supported', roomInvitationId)
  }

  override async roomInvitationRawPayload(roomInvitationId: string): Promise<PUPPET.payloads.RoomInvitation> {
    log.verbose('PuppetAgentWeChat', 'roomInvitationRawPayload(%s) not supported', roomInvitationId)
    return {} as PUPPET.payloads.RoomInvitation
  }

  override async roomInvitationRawPayloadParser(rawPayload: PUPPET.payloads.RoomInvitation): Promise<PUPPET.payloads.RoomInvitation> {
    return rawPayload
  }

  // ==================
  // Friendship
  // ==================

  override async friendshipAccept(friendshipId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'friendshipAccept(%s) not supported', friendshipId)
  }

  override async friendshipAdd(contactId: string, option?: PUPPET.types.FriendshipAddOptions): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'friendshipAdd(%s) not supported', contactId)
  }

  override async friendshipSearchPhone(phone: string): Promise<null | string> {
    return null
  }

  override async friendshipSearchHandle(handle: string): Promise<null | string> {
    return null
  }

  override async friendshipRawPayload(friendshipId: string): Promise<PUPPET.payloads.Friendship> {
    return { id: friendshipId } as PUPPET.payloads.Friendship
  }

  override async friendshipRawPayloadParser(rawPayload: PUPPET.payloads.Friendship): Promise<PUPPET.payloads.Friendship> {
    return rawPayload
  }

  // ==================
  // Tag
  // ==================

  override async tagContactAdd(tagId: string, contactId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'tagContactAdd(%s, %s) not supported', tagId, contactId)
  }

  override async tagContactRemove(tagId: string, contactId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'tagContactRemove(%s, %s) not supported', tagId, contactId)
  }

  override async tagContactDelete(tagId: string): Promise<void> {
    log.verbose('PuppetAgentWeChat', 'tagContactDelete(%s) not supported', tagId)
  }

  override async tagContactList(contactId: string): Promise<string[]>
  override async tagContactList(): Promise<string[]>
  override async tagContactList(contactId?: string): Promise<string[]> {
    return []
  }

  // ==================
  // Post
  // ==================

  override async postPublish(payload: PUPPET.payloads.Post): Promise<void | string> {
    log.verbose('PuppetAgentWeChat', 'postPublish() not supported')
  }

  override async postSearch(
    filter: PUPPET.filters.Post,
    pagination?: PUPPET.filters.PaginationRequest,
  ): Promise<PUPPET.filters.PaginationResponse<string[]>> {
    return { response: [] } as any
  }

  override async postRawPayload(postId: string): Promise<PUPPET.payloads.Post> {
    throw PUPPET.throwUnsupportedError()
  }

  override async postRawPayloadParser(rawPayload: PUPPET.payloads.Post): Promise<PUPPET.payloads.Post> {
    return rawPayload
  }

  // ==================
  // Tap
  // ==================

  override async tap(
    postId: string,
    type?: PUPPET.types.Tap,
    tap?: boolean,
  ): Promise<void | boolean> {
    log.verbose('PuppetAgentWeChat', 'tap(%s) not supported', postId)
  }

  override async tapSearch(
    postId: string,
    query?: PUPPET.filters.Tap,
    pagination?: PUPPET.filters.PaginationRequest,
  ): Promise<PUPPET.filters.PaginationResponse<PUPPET.payloads.Tap>> {
    return { response: [] } as any
  }
}
