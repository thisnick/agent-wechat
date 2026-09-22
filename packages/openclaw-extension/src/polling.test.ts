import test from "node:test";
import assert from "node:assert/strict";
import type { Chat, Message } from "@agent-wechat/shared";
import { listAllChats, listMessageWindow } from "./polling.ts";

function message(localId: number): Message {
  return {
    localId,
    serverId: localId,
    chatId: "chat",
    type: 1,
    content: String(localId),
    timestamp: new Date(localId * 1000).toISOString(),
  };
}

test("chat polling continues beyond the first 50 chats", async () => {
  const all = Array.from({ length: 125 }, (_, index) => ({ id: String(index) }) as Chat);
  const calls: number[] = [];
  const client = {
    async listChats(limit = 50, offset = 0) {
      calls.push(offset);
      return all.slice(offset, offset + limit);
    },
    async listMessages() { return []; },
  };
  assert.equal((await listAllChats(client)).length, 125);
  assert.deepEqual(calls, [0, 100]);
});

test("catch-up pages backward until it reaches the saved cursor", async () => {
  const all = Array.from({ length: 300 }, (_, index) => message(300 - index));
  const calls: number[] = [];
  const client = {
    async listChats() { return []; },
    async listMessages(_chatId: string, limit = 50, offset = 0) {
      calls.push(offset);
      return all.slice(offset, offset + limit);
    },
  };
  const messages = await listMessageWindow(client, "chat", 50, false, 0);
  assert.equal(messages.filter((item) => item.localId > 50).length, 250);
  assert.deepEqual(calls, [0, 200]);
});

test("first unread poll fetches enough pages for the unread count", async () => {
  const all = Array.from({ length: 450 }, (_, index) => message(450 - index));
  const client = {
    async listChats() { return []; },
    async listMessages(_chatId: string, limit = 50, offset = 0) {
      return all.slice(offset, offset + limit);
    },
  };
  assert.equal((await listMessageWindow(client, "chat", 0, true, 350)).length, 400);
});
