import * as PUPPET from 'wechaty-puppet'
import type { Chat, Contact, Message } from '@agent-wechat/shared'

/**
 * Map WeChat message type numbers to Wechaty MessageType.
 *
 * WeChat types: 1=Text, 3=Image, 34=Voice, 42=ShareCard, 43=Video,
 *   47=Emoticon, 48=Location, 49=App(varies), 62=MicroVideo, 10000=Sys
 */
export function wechatTypeToMessageType(wechatType: number): PUPPET.types.Message {
  const baseType = wechatType & 0x7fffffff
  switch (baseType) {
    case 1:     return PUPPET.types.Message.Text
    case 3:     return PUPPET.types.Message.Image
    case 34:    return PUPPET.types.Message.Audio
    case 42:    return PUPPET.types.Message.Contact
    case 43:    return PUPPET.types.Message.Video
    case 47:    return PUPPET.types.Message.Emoticon
    case 48:    return PUPPET.types.Message.Location
    case 49:    return PUPPET.types.Message.Attachment  // App messages — could be URL, file, etc.
    case 62:    return PUPPET.types.Message.Video       // MicroVideo
    case 10000: return PUPPET.types.Message.Unknown     // System messages
    case 10002: return PUPPET.types.Message.Recalled
    default:    return PUPPET.types.Message.Unknown
  }
}

/**
 * Convert an agent-wechat Chat (non-group) to a Wechaty ContactPayload.
 */
export function chatToContactPayload(chat: Chat): PUPPET.payloads.Contact {
  return {
    id:     chat.username,
    name:   chat.name,
    alias:  chat.remark,
    gender: PUPPET.types.ContactGender.Unknown,
    type:   chat.username.startsWith('gh_')
      ? PUPPET.types.Contact.Official
      : PUPPET.types.Contact.Individual,
    avatar: '',
    phone:  [],
    friend: true,
  }
}

/**
 * Convert a server Contact to a Wechaty ContactPayload.
 */
export function contactToContactPayload(contact: Contact): PUPPET.payloads.Contact {
  return {
    id:     contact.username,
    name:   contact.nickName,
    alias:  contact.remark,
    gender: PUPPET.types.ContactGender.Unknown,
    type:   contact.contactType === 'official'
      ? PUPPET.types.Contact.Official
      : PUPPET.types.Contact.Individual,
    avatar: contact.smallHeadUrl ?? '',
    phone:  [],
    friend: true,
  }
}

/**
 * Convert an agent-wechat Chat (group) to a Wechaty RoomPayload.
 */
export function chatToRoomPayload(chat: Chat): PUPPET.payloads.Room {
  return {
    id:           chat.username,
    topic:        chat.name,
    avatar:       '',
    memberIdList: [],
    adminIdList:  [],
  }
}

/**
 * Convert an agent-wechat Message to a Wechaty MessagePayload.
 */
export function messageToPayload(
  msg: Message,
  selfId: string,
): PUPPET.payloads.Message {
  const isRoom = msg.chatId.includes('@chatroom')
  const timestamp = Math.floor(new Date(msg.timestamp).getTime() / 1000)

  const base: PUPPET.payloads.MessageBase = {
    id:        String(msg.localId),
    talkerId:  msg.sender ?? msg.chatId,
    text:      msg.content || undefined,
    timestamp,
    type:      wechatTypeToMessageType(msg.type),
  }

  if (isRoom) {
    return {
      ...base,
      roomId:        msg.chatId,
      mentionIdList: msg.isMentioned ? [selfId] : [],
    }
  }

  return {
    ...base,
    listenerId: msg.isSelf ? msg.chatId : selfId,
  }
}
