import test from "node:test";
import assert from "node:assert/strict";
import { conversationUnreadCounts } from "../src/conversation-filters.ts";

test("the inbox total includes system chats and older hubs can still show direct chats", () => {
  assert.deepEqual(conversationUnreadCounts([
    { kind: "direct", unreadCount: 5 },
    { kind: "direct", classification: { category: "system" }, unreadCount: 2 },
    { kind: "direct", classification: { category: "group" }, unreadCount: 3 },
    { kind: "group", unreadCount: null },
  ]), { total: 10, dm: 5, group: 3, official: 0, service: 0 });
});
