import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { MonitorStateStore } from "./monitor-state.ts";

type Item = { id: number; body: string };
const isItem = (value: unknown): value is Item =>
  !!value && typeof value === "object" &&
  typeof (value as Item).id === "number" && typeof (value as Item).body === "string";

test("monitor state survives a new store instance", async (t) => {
  const stateDir = await fs.mkdtemp(path.join(os.tmpdir(), "wechat-state-test-"));
  t.after(() => fs.rm(stateDir, { recursive: true, force: true }));
  const first = new MonitorStateStore<Item>(stateDir, "default");
  await first.save(new Map([["chat", 77]]), new Map([["room@chatroom", [{ id: 77, body: "history" }]]]));

  const second = new MonitorStateStore<Item>(stateDir, "default");
  const loaded = await second.load(isItem);
  assert.equal(loaded.lastSeen.chat, 77);
  assert.deepEqual(loaded.groupHistory["room@chatroom"], [{ id: 77, body: "history" }]);
});

test("monitor state drops malformed entries", async (t) => {
  const stateDir = await fs.mkdtemp(path.join(os.tmpdir(), "wechat-state-test-"));
  t.after(() => fs.rm(stateDir, { recursive: true, force: true }));
  const store = new MonitorStateStore<Item>(stateDir, "default");
  await store.save(new Map([["chat", 1]]), new Map([["room", [{ id: 1, body: "ok" }]]]));
  const files = await fs.readdir(path.join(stateDir, "agent-wechat", "monitor"));
  const statePath = path.join(stateDir, "agent-wechat", "monitor", files[0]);
  await fs.writeFile(statePath, JSON.stringify({ version: 1, lastSeen: { chat: -1 }, groupHistory: { room: [{ nope: true }] } }));
  const loaded = await store.load(isItem);
  assert.deepEqual(Object.keys(loaded.lastSeen), []);
  assert.deepEqual(Object.keys(loaded.groupHistory), []);
});
