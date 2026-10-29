import test from "node:test";
import assert from "node:assert/strict";
import { connect } from "../src/connection.ts";

// Exercise the receipt filter used by an actual reopened connection. The
// transport is synthetic, and recovery is allowed to inspect receipts only.
test("connection reload retains combined quote and inline mention receipts", async (t) => {
  const quote = { messageId: "source", serverId: "18446744073709551615" };
  const mentions = { userIds: ["jin"], positions: [{ start: 0, end: 5, index: 0 }] };
  const pending = { key: "combined-key", text: "@Jin\u2005please check", threadId: "room@chatroom", quote, mentions, uncertain: true };
  const values = new Map([["agentwechat-receipts:fixture-hub:a", JSON.stringify([
    pending,
    { ...pending, key: "bad-quote", quote: { ...quote, serverId: "0" } },
    { ...pending, key: "bad-mentions", mentions: { ...mentions, positions: [{ start: 0, end: 4, index: 0 }] } },
    { ...pending, key: "direct", threadId: "peer" },
  ])]]);
  const priorStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: { getItem: key => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value), removeItem: key => values.delete(key) } });
  t.after(() => priorStorage ? Object.defineProperty(globalThis, "sessionStorage", priorStorage) : delete globalThis.sessionStorage);
  const calls = [];
  const page = { items: [], nextCursor: null, asOfEventCursor: "0", sourceGeneration: "1", coverage: {} };
  const transport = async (input, init) => {
    const url = new URL(input); calls.push([init?.method ?? "GET", url.pathname]);
    assert.ok(!init?.method || init.method === "GET", "reload never resubmits text");
    if (url.pathname === "/v1/accounts") return Response.json({ items: [{ id: "a", authenticated: true }] });
    if (url.pathname.endsWith("/capabilities")) return Response.json({});
    if (url.pathname.endsWith("/sends/by-idempotency-key")) return Response.json({ id: "send1", accountId: "a", destination: pending.threadId, state: "needs_review" });
    if (url.pathname.endsWith("/events/stream")) return new Response("");
    return Response.json(page);
  };
  const connection = await connect("http://127.0.0.1:6174", "synthetic-token", false, transport, { serverId: "fixture-hub" });
  t.after(() => connection.close());
  await connection.session.activate("a");
  const sends = connection.session.snapshot().sends;
  assert.equal(sends.length, 1);
  assert.deepEqual(sends[0].quote, quote);
  assert.deepEqual(sends[0].mentions, mentions);
  assert.equal(sends[0].key, pending.key);
  assert.ok(calls.every(([method]) => method === "GET"));
});
