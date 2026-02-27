import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import * as PUPPET from 'wechaty-puppet'
import type { Chat, Message } from '@agent-wechat/shared'
import {
  wechatTypeToMessageType,
  chatToContactPayload,
  chatToRoomPayload,
  messageToPayload,
} from './type-map.js'

describe('wechatTypeToMessageType', () => {
  it('maps text (1) to Text', () => {
    assert.equal(wechatTypeToMessageType(1), PUPPET.types.Message.Text)
  })

  it('maps image (3) to Image', () => {
    assert.equal(wechatTypeToMessageType(3), PUPPET.types.Message.Image)
  })

  it('maps voice (34) to Audio', () => {
    assert.equal(wechatTypeToMessageType(34), PUPPET.types.Message.Audio)
  })

  it('maps video (43) to Video', () => {
    assert.equal(wechatTypeToMessageType(43), PUPPET.types.Message.Video)
  })

  it('maps emoticon (47) to Emoticon', () => {
    assert.equal(wechatTypeToMessageType(47), PUPPET.types.Message.Emoticon)
  })

  it('maps location (48) to Location', () => {
    assert.equal(wechatTypeToMessageType(48), PUPPET.types.Message.Location)
  })

  it('maps app message (49) to Attachment', () => {
    assert.equal(wechatTypeToMessageType(49), PUPPET.types.Message.Attachment)
  })

  it('maps recalled (10002) to Recalled', () => {
    assert.equal(wechatTypeToMessageType(10002), PUPPET.types.Message.Recalled)
  })

  it('maps unknown type to Unknown', () => {
    assert.equal(wechatTypeToMessageType(999), PUPPET.types.Message.Unknown)
  })

  it('masks high bit for type comparison', () => {
    // WeChat sometimes sets bit 31 on type values
    assert.equal(wechatTypeToMessageType(1 | 0x80000000), PUPPET.types.Message.Text)
  })
})

describe('chatToContactPayload', () => {
  it('maps a regular chat to ContactPayload', () => {
    const chat: Chat = {
      id: 'wxid_abc123',
      username: 'wxid_abc123',
      name: 'Alice',
      remark: 'Alice Remark',
      unreadCount: 0,
      isGroup: false,
    }

    const payload = chatToContactPayload(chat)

    assert.equal(payload.id, 'wxid_abc123')
    assert.equal(payload.name, 'Alice')
    assert.equal(payload.alias, 'Alice Remark')
    assert.equal(payload.type, PUPPET.types.Contact.Individual)
    assert.equal(payload.gender, PUPPET.types.ContactGender.Unknown)
    assert.equal(payload.friend, true)
  })

  it('maps official account (gh_) to Official type', () => {
    const chat: Chat = {
      id: 'gh_abc123',
      username: 'gh_abc123',
      name: 'Official Account',
      unreadCount: 0,
      isGroup: false,
    }

    const payload = chatToContactPayload(chat)
    assert.equal(payload.type, PUPPET.types.Contact.Official)
  })

  it('handles missing remark', () => {
    const chat: Chat = {
      id: 'wxid_test',
      username: 'wxid_test',
      name: 'Test',
      unreadCount: 0,
      isGroup: false,
    }

    const payload = chatToContactPayload(chat)
    assert.equal(payload.alias, undefined)
  })
})

describe('chatToRoomPayload', () => {
  it('maps a group chat to RoomPayload', () => {
    const chat: Chat = {
      id: '12345@chatroom',
      username: '12345@chatroom',
      name: 'My Group',
      unreadCount: 3,
      isGroup: true,
    }

    const payload = chatToRoomPayload(chat)

    assert.equal(payload.id, '12345@chatroom')
    assert.equal(payload.topic, 'My Group')
    assert.deepEqual(payload.memberIdList, [])
    assert.deepEqual(payload.adminIdList, [])
  })
})

describe('messageToPayload', () => {
  const selfId = 'wxid_self'

  it('maps a direct text message', () => {
    const msg: Message = {
      localId: 42,
      serverId: 100,
      chatId: 'wxid_sender',
      sender: 'wxid_sender',
      type: 1,
      content: 'Hello world',
      timestamp: '2025-01-15T10:30:00Z',
    }

    const payload = messageToPayload(msg, selfId)

    assert.equal(payload.id, '42')
    assert.equal(payload.talkerId, 'wxid_sender')
    assert.equal(payload.text, 'Hello world')
    assert.equal(payload.type, PUPPET.types.Message.Text)
    assert.equal(payload.timestamp, Math.floor(new Date('2025-01-15T10:30:00Z').getTime() / 1000))
    // Direct message: has listenerId, no roomId
    assert.equal('listenerId' in payload && payload.listenerId, selfId)
    assert.equal('roomId' in payload, false)
  })

  it('maps a group text message', () => {
    const msg: Message = {
      localId: 99,
      serverId: 200,
      chatId: '12345@chatroom',
      sender: 'wxid_sender',
      type: 1,
      content: 'Group message',
      timestamp: '2025-01-15T10:30:00Z',
    }

    const payload = messageToPayload(msg, selfId)

    assert.equal(payload.id, '99')
    assert.equal(payload.talkerId, 'wxid_sender')
    // Group message: has roomId, no listenerId
    assert.equal('roomId' in payload && payload.roomId, '12345@chatroom')
    assert.equal('listenerId' in payload, false)
  })

  it('sets mentionIdList when isMentioned is true', () => {
    const msg: Message = {
      localId: 77,
      serverId: 300,
      chatId: '12345@chatroom',
      sender: 'wxid_sender',
      type: 1,
      content: '@self hello',
      timestamp: '2025-01-15T10:30:00Z',
      isMentioned: true,
    }

    const payload = messageToPayload(msg, selfId)
    assert.equal('mentionIdList' in payload, true)
    if ('mentionIdList' in payload) {
      assert.deepEqual(payload.mentionIdList, [selfId])
    }
  })

  it('uses chatId as talkerId when sender is missing', () => {
    const msg: Message = {
      localId: 55,
      serverId: 400,
      chatId: 'wxid_friend',
      type: 1,
      content: 'No sender field',
      timestamp: '2025-01-15T10:30:00Z',
    }

    const payload = messageToPayload(msg, selfId)
    assert.equal(payload.talkerId, 'wxid_friend')
  })

  it('handles empty content', () => {
    const msg: Message = {
      localId: 33,
      serverId: 500,
      chatId: 'wxid_friend',
      sender: 'wxid_friend',
      type: 3,
      content: '',
      timestamp: '2025-01-15T10:30:00Z',
    }

    const payload = messageToPayload(msg, selfId)
    assert.equal(payload.text, undefined)
    assert.equal(payload.type, PUPPET.types.Message.Image)
  })

  it('sets listenerId to chatId for self-sent messages', () => {
    const msg: Message = {
      localId: 88,
      serverId: 600,
      chatId: 'wxid_friend',
      sender: selfId,
      type: 1,
      content: 'I sent this',
      timestamp: '2025-01-15T10:30:00Z',
      isSelf: true,
    }

    const payload = messageToPayload(msg, selfId)
    assert.equal('listenerId' in payload && payload.listenerId, 'wxid_friend')
  })
})
