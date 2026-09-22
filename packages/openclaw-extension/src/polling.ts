import type { Chat, Message } from "@agent-wechat/shared";

type PagingClient = {
  listChats(limit?: number, offset?: number): Promise<Chat[]>;
  listMessages(chatId: string, limit?: number, offset?: number): Promise<Message[]>;
};

export async function listAllChats(client: PagingClient): Promise<Chat[]> {
  const pageSize = 100;
  const chats: Chat[] = [];
  for (let offset = 0; ; offset += pageSize) {
    const page = await client.listChats(pageSize, offset);
    chats.push(...page);
    if (page.length < pageSize) return chats;
  }
}

export async function listMessageWindow(
  client: PagingClient,
  chatId: string,
  previousLastSeen: number,
  firstPoll: boolean,
  unreadCount: number,
): Promise<Message[]> {
  const pageSize = 200;
  const messages: Message[] = [];
  const firstPollTarget = Math.max(unreadCount, 20);
  for (let offset = 0; ; offset += pageSize) {
    const page = await client.listMessages(chatId, pageSize, offset);
    messages.push(...page);
    if (page.length < pageSize) break;
    if (firstPoll && messages.length >= firstPollTarget) break;
    if (!firstPoll && page.some((message) => message.localId <= previousLastSeen)) break;
  }
  return messages;
}
