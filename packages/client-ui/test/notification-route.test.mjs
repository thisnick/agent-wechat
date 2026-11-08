import test from "node:test";
import assert from "node:assert/strict";
import { formatRoute } from "../src/routes.ts";

globalThis.window = globalThis;
const { notificationRoute } = await import("../src/desktop.ts");

test("a notification opens its message when known, else its conversation", () => {
  const target = { connectionId: "c", serverId: "hub-1", accountId: "account-1", threadId: "123@chatroom" };
  assert.equal(formatRoute(notificationRoute(target)), "#/hub-1/account-1/chats/123%40chatroom");
  assert.equal(
    formatRoute(notificationRoute({ ...target, messageId: "msg_5" })),
    "#/hub-1/account-1/chats/123%40chatroom/message/msg_5",
  );
});

test("a notification target from an older app version still opens its conversation", () => {
  const target = { connectionId: "c", serverId: "hub-1", accountId: "a", threadId: "wxid_peer" };
  assert.deepEqual(notificationRoute(target), { name: "chat", server: "hub-1", account: "a", thread: "wxid_peer" });
});
