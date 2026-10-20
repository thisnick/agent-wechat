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
