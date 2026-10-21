import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, readFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { NativeConsumerState } from "./native-consumer-state.ts";
import type { NativeMessage } from "@agent-wechat/shared";
const message = (id: string, threadId = "t"): NativeMessage => ({ id, accountId: "a", threadId, serverId: "9007199254740999", senderId: "u", type: 1, direction: "incoming", body: "fixture", createdAt: "2026-10-20T12:00:00Z", mentions: { state: "unknown", userIds: [], self: null, all: null, source: null }, version: "1", media: [] });
function fixture(t: any): NativeConsumerState { const dir = mkdtempSync(join(tmpdir(), "native-custody-")); t.after(() => rmSync(dir, { recursive: true, force: true })); return new NativeConsumerState(dir, "server", "a"); }
test("native intake ACK custody does not mark read or coerce string IDs", t => { const state = fixture(t); state.accept("9007199254740999", message("m1"), "live"); assert.equal(state.cursor, "9007199254740999"); assert.equal(state.get("m1")?.readPending, undefined); assert.equal(state.get("m1")?.message.serverId, "9007199254740999"); });
test("prepared callback durably records only exact new input IDs and preserves ownership return", t => {
  const state = fixture(t); for (const [i, id] of ["m1", "gap", "m3"].entries()) state.accept(String(i + 1), message(id), "live");
  state.update("m1", { state: "dispatching" }); state.update("m3", { state: "dispatching" });
  const callback = state.preparedCallback(["m1", "m3"], "on_model_dispatch", () => "reply-dispatch");
  assert.equal(callback("run_1"), "reply-dispatch");
  assert.equal(state.get("m1")?.readPending, true); assert.equal(state.get("m3")?.readPending, true); assert.equal(state.get("gap")?.readPending, undefined);
  const durable = JSON.parse(readFileSync(state.file, "utf8")); assert.equal(durable.items.m1.runId, "run_1"); assert.equal(durable.items.gap.state, "received");
});
test("read never policy retains handoff receipt without a read projection", t => { const state = fixture(t); state.accept("1", message("m"), "live"); state.update("m", { state: "dispatching" }); state.preparedCallback(["m"], "never")("run"); assert.equal(state.get("m")?.state, "handed_off"); assert.equal(state.get("m")?.readPending, false); });
test("structured prepared callback records run identity and clears prior uncertainty", t => { const state = fixture(t); state.accept("1", message("m"), "live"); state.update("m", { state: "dispatching", handoffUnconfirmed: true }); state.preparedCallback(["m"], "on_model_dispatch")({ runId: "structured-run" }); assert.equal(state.get("m")?.runId, "structured-run"); assert.equal(state.get("m")?.handoffUnconfirmed, false); });
test("unknown crash dispatch is held for review rather than re-dispatched", t => { const state = fixture(t); state.accept("1", message("m"), "live"); state.update("m", { state: "dispatching" }); const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a"); assert.equal(reopened.get("m")?.state, "needs_review"); assert.equal(reopened.get("m")?.readPending, undefined); });
test("ignored and omitted messages stay unread, cross-thread callback is rejected", t => { const state = fixture(t); state.accept("1", message("m"), "import"); state.update("m", { state: "ignored" }); assert.equal(state.get("m")?.readPending, undefined); assert.throws(() => state.preparedCallback(["m"], "on_model_dispatch")); });
test("cross-account event and backwards cursor cannot advance durable custody", t => { const state = fixture(t); state.accept("2", message("m"), "live"); assert.throws(() => state.accept("3", { ...message("x"), accountId: "other" }, "live")); assert.throws(() => state.accept("1")); assert.equal(state.cursor, "2"); });

test("failed durable custody write does not advance the in-memory cursor or admit an ACK", t => {
  const state = fixture(t); state.accept("1", message("first"), "live");
  const directory = join(state.file, ".."); chmodSync(directory, 0o755);
  try { assert.throws(() => state.accept("2", message("second"), "live"), /private/); assert.equal(state.cursor, "1"); assert.equal(state.get("second"), undefined); }
  finally { chmodSync(directory, 0o700); }
  const reopened = new NativeConsumerState(directory, "server", "a"); assert.equal(reopened.cursor, "1"); assert.equal(reopened.get("second"), undefined);
});
test("group context survives restart in ingress order with a per-thread bound", t => {
  const state = fixture(t);
  for (let i = 1; i <= 4; i++) { state.accept(String(i), { ...message(`m${i}`, "r@chatroom"), serverId: String(5 - i) }, "live"); state.bufferGroupMessage(`m${i}`, 2); }
  state.accept("5", message("trigger", "r@chatroom"), "live");
  const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a");
  assert.deepEqual(reopened.groupHistory("r@chatroom", "trigger", 50).map(item => item.message.id), ["m3", "m4"]);
  assert.equal(reopened.get("m1")?.groupContext, false); assert.equal(reopened.get("m4")?.readPending, undefined);
  reopened.accept("6", message("later", "r@chatroom"), "live"); reopened.bufferGroupMessage("later", 2);
  assert.deepEqual(reopened.groupHistory("r@chatroom", "trigger", 50).map(item => item.message.id), ["m4"]);
});
test("context offered to an uncertain turn is not replayed or attached to the next turn", t => {
  const state = fixture(t); state.accept("1", message("context", "r@chatroom"), "live"); state.bufferGroupMessage("context", 50);
  state.accept("2", message("trigger", "r@chatroom"), "live");
  state.beginDispatch("trigger", {} as any, ["context"]);
  const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a");
  assert.equal(reopened.get("trigger")?.state, "needs_review"); assert.equal(reopened.get("context")?.groupContext, false);
  reopened.accept("3", message("next", "r@chatroom"), "live"); assert.deepEqual(reopened.groupHistory("r@chatroom", "next", 50), []);
});
test("disabled history does not retain group context or create read receipts", t => {
  const state = fixture(t); state.accept("1", message("context", "r@chatroom"), "live"); state.bufferGroupMessage("context", 0);
  state.accept("2", message("trigger", "r@chatroom"), "live"); assert.deepEqual(state.groupHistory("r@chatroom", "trigger", 50), []);
  assert.equal(state.get("context")?.readPending, undefined);
});
