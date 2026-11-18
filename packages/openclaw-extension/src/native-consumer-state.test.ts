import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, readFileSync, chmodSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { NativeConsumerState } from "./native-consumer-state.ts";
import type { NativeMessage } from "@agent-wechat/shared";
const message = (id: string, threadId = "t", serverId = "9007199254740999"): NativeMessage => ({ id, accountId: "a", threadId, serverId, senderId: "u", type: 1, direction: "incoming", body: "private fixture body", createdAt: "2026-10-20T12:00:00Z", mentions: { state: "unknown", userIds: [], self: null, all: null, source: null }, version: "1", media: [] });
function directory(t: any): string { const dir = mkdtempSync(join(tmpdir(), "native-custody-")); t.after(() => rmSync(dir, { recursive: true, force: true })); return dir; }
function fixture(t: any): NativeConsumerState { return new NativeConsumerState(directory(t), "server", "a"); }
const reopen = (state: NativeConsumerState) => new NativeConsumerState(join(state.file, ".."), "server", "a");
const manifest = (ids: string[], extra: object = {}) => ({ messageIds: ids, assets: [], ...extra }) as any;

test("intake stores only the identity of queued work, never message content or string-coerced IDs", t => {
  const state = fixture(t); state.accept("9007199254740999", message("m1"), "live");
  assert.equal(state.cursor, "9007199254740999");
  assert.deepEqual({ ...state.get("m1"), revision: undefined }, { id: "m1", threadId: "t", serverId: "9007199254740999", origin: "live", cursor: "9007199254740999", revision: undefined, state: "received" });
  assert.ok(!readFileSync(state.file, "utf8").includes("private fixture body"));
  assert.equal(state.readPending("t"), false);
});
test("the cursor is the record of seen events: repeats are ignored, backward and cross-account events are refused", t => {
  const state = fixture(t); state.accept("2", message("m"), "live"); state.finish("m");
  assert.equal(state.accept("2", message("m"), "live"), false); assert.equal(state.get("m"), undefined);
  assert.throws(() => state.accept("3", { ...message("x"), accountId: "other" }, "live")); assert.throws(() => state.advance("1"));
  assert.equal(state.cursor, "2");
});
test("skipped events advance in memory and are written before acknowledgement", t => {
  const state = fixture(t); state.accept("1", message("m"), "live"); state.advance("5");
  assert.equal(state.cursor, "5"); assert.equal(reopen(state).cursor, "1");
  state.commit(); assert.equal(reopen(state).cursor, "5");
});
test("failed durable custody write does not advance the in-memory cursor or admit an ACK", t => {
  const state = fixture(t); state.accept("1", message("first"), "live");
  const dir = join(state.file, ".."); chmodSync(dir, 0o755);
  try { assert.throws(() => state.accept("2", message("second", "t", "2"), "live"), /private/); assert.equal(state.cursor, "1"); assert.equal(state.get("second"), undefined); }
  finally { chmodSync(dir, 0o700); }
  const reopened = reopen(state); assert.equal(reopened.cursor, "1"); assert.equal(reopened.get("second"), undefined);
});
test("prepared callback records exact new input IDs, queues the thread read and preserves ownership return", t => {
  const state = fixture(t); for (const [i, id] of ["m1", "gap", "m3"].entries()) state.accept(String(i + 1), message(id, "t", String(i + 1)), "live");
  state.update("m1", { state: "dispatching" }); state.update("m3", { state: "dispatching" });
  const callback = state.preparedCallback(["m1", "m3"], "on_model_dispatch", () => "reply-dispatch");
  assert.equal(callback("run_1"), "reply-dispatch");
  assert.equal(state.readPending("t"), true); assert.equal(state.get("gap")?.state, "received");
  const durable = JSON.parse(readFileSync(state.file, "utf8")); assert.equal(durable.open.m1.runId, "run_1"); assert.deepEqual(durable.reads.t.messages.map((m: any) => m.id), ["m1", "m3"]);
});
test("read never policy records the handoff without a read", t => { const state = fixture(t); state.accept("1", message("m"), "live"); state.update("m", { state: "dispatching" }); state.preparedCallback(["m"], "never")("run"); assert.equal(state.get("m")?.state, "handed_off"); assert.equal(state.readPending("t"), false); });
test("structured prepared callback records run identity and clears prior uncertainty", t => { const state = fixture(t); state.accept("1", message("m"), "live"); state.update("m", { state: "dispatching", handoffUnconfirmed: true }); state.preparedCallback(["m"], "on_model_dispatch")({ runId: "structured-run" }); assert.equal(state.get("m")?.runId, "structured-run"); assert.equal(state.get("m")?.handoffUnconfirmed, false); });
test("a turn in progress at a crash is dropped on reopen and reported, never dispatched again", t => {
  const state = fixture(t); state.accept("1", message("m"), "live"); state.accept("2", message("queued", "t", "2"), "live"); state.update("m", { state: "dispatching" });
  const reopened = reopen(state); assert.deepEqual(reopened.heldOnOpen, ["m"]); assert.equal(reopened.get("m"), undefined); assert.equal(reopened.get("queued")?.state, "received");
  assert.deepEqual(reopen(reopened).heldOnOpen, []);
});
test("cross-thread callbacks are rejected and finishing respects the current occupant", t => {
  const state = fixture(t); state.accept("1", message("m"), "live"); assert.throws(() => state.preparedCallback(["m"], "on_model_dispatch"));
  const revision = state.get("m")!.revision; assert.equal(state.finish("m", "stale"), false); assert.ok(state.get("m"));
  assert.equal(state.finish("m", revision), true); assert.equal(state.get("m"), undefined); assert.equal(state.finish("m"), false);
});
test("completion keeps a requested read, drops the finished message, and waits a bounded time for an unconfirmed handoff", t => {
  const state = fixture(t); state.accept("1", message("m"), "live"); state.beginDispatch("m", manifest(["m"]));
  assert.equal(state.complete("m", state.get("m")!.revision, { failed: false, requestRead: true, awaitHandoff: false }), true);
  assert.equal(state.get("m"), undefined); assert.equal(state.readPending("t"), true);
  for (let i = 0; i < 102; i++) {
    const id = `w${i}`; state.accept(String(10 + i), message(id, "u", String(100 + i)), "live"); state.beginDispatch(id, manifest([id]));
    if (i === 101) { const late = state.preparedCallback([id], "on_model_dispatch"); state.complete(id, state.get(id)!.revision, { failed: false, requestRead: false, awaitHandoff: true }); late("late-run"); }
    else state.complete(id, state.get(id)!.revision, { failed: false, requestRead: false, awaitHandoff: true });
  }
  const waiting = state.items().filter(item => item.state === "awaiting_handoff");
  // At most 100 wait; the oldest are dropped first, and a confirmed one leaves.
  assert.equal(waiting.length, 99); assert.equal(state.get("w0"), undefined); assert.equal(state.get("w1"), undefined); assert.equal(state.get("w101"), undefined);
  assert.equal(state.readPending("u"), true);
  assert.equal(reopen(state).items().length, 0);
});
test("a read started before a newer request does not clear it", t => {
  const state = fixture(t); state.accept("1", message("m"), "live"); state.beginDispatch("m", manifest(["m"]));
  state.complete("m", state.get("m")!.revision, { failed: false, requestRead: true, awaitHandoff: false });
  const [started] = state.pendingReads();
  state.accept("2", message("n", "t", "2"), "live"); state.beginDispatch("n", manifest(["n"]));
  state.complete("n", state.get("n")!.revision, { failed: false, requestRead: true, awaitHandoff: false });
  assert.equal(state.clearRead(started.threadId, started.token), false); assert.equal(state.readPending("t"), true);
  const [latest] = state.pendingReads(); assert.equal(state.clearRead("t", latest.token), true); assert.equal(state.readPending("t"), false);
  assert.equal(reopen(state).readPending("t"), false);
});
test("group boundaries move forward in the same write as the turn record", t => {
  const state = fixture(t); state.accept("1", message("older", "r@chatroom", "1"), "live"); state.accept("2", message("newer", "r@chatroom", "2"), "live");
  state.beginDispatch("newer", manifest(["newer"]), { boundary: { createdAt: "2026-10-20T12:00:02Z" } });
  const reopened = reopen(state);
  assert.deepEqual(reopened.groupBoundary("r@chatroom"), { cursor: "2", messageId: "newer", createdAt: "2026-10-20T12:00:02Z" });
  reopened.beginDispatch("older", manifest(["older"]), { boundary: { createdAt: "2026-10-20T12:00:01Z" } });
  assert.equal(reopened.groupBoundary("r@chatroom")?.messageId, "newer");
  assert.equal(reopened.groupBoundary("t"), undefined);
});
test("recalls cancel queued targets, re-read quoting messages and invalidate turns that offered them, without storing a tombstone", t => {
  const state = fixture(t); const target = message("target", "r@chatroom", "100");
  state.accept("1", target, "live");
  const quoting = { ...message("quoting", "r@chatroom", "101"), type: 49, appMessageType: 57, quote: { serverId: "100", type: 1 } } as NativeMessage;
  state.accept("2", quoting, "live"); const quotingRevision = state.get("quoting")!.revision;
  state.accept("3", message("trigger", "r@chatroom", "102"), "live");
  state.beginDispatch("trigger", manifest(["trigger"], { historyInputs: [{ id: "history" }] }), { inputServerIds: ["100"] });
  const prepared = state.preparedCallback(["trigger"], "on_model_dispatch");
  const affected = state.acceptRecall("4", { threadId: "r@chatroom", serverId: "100", observedAt: "2026-10-21T12:00:00Z" });
  assert.deepEqual(new Set(affected), new Set(["target", "quoting", "trigger"]));
  assert.equal(state.get("target"), undefined);
  assert.equal(state.get("quoting")?.state, "received"); assert.notEqual(state.get("quoting")?.revision, quotingRevision);
  assert.equal(state.get("trigger")?.invalidatedByRecall, true); assert.equal(state.get("trigger")?.dispatchManifest, undefined);
  assert.throws(() => prepared("late-run"), /invalidated/);
  assert.equal(state.isRecalled(target), true); assert.equal(state.isCurrent(target), false);
  assert.equal(state.accept("5", target, "live"), false); assert.equal(state.get("target"), undefined);
  const reopened = reopen(state);
  assert.equal(reopened.cursor, "5"); assert.ok(!readFileSync(state.file, "utf8").includes("observedAt"));
  // After a restart the daemon's own copy carries the recall.
  assert.equal(reopened.isRecalled(target), false); assert.equal(reopened.isRecalled({ ...target, recall: { observedAt: "2026-10-21T12:00:00Z" } }), true);
});
test("a recall cancels the read receipt its target asked for", t => {
  const state = fixture(t); state.accept("1", message("m", "t", "7"), "live"); state.beginDispatch("m", manifest(["m"]));
  state.complete("m", state.get("m")!.revision, { failed: false, requestRead: true, awaitHandoff: false });
  state.acceptRecall("2", { threadId: "t", serverId: "7", observedAt: "2026-10-21T12:00:00Z" });
  assert.equal(state.readPending("t"), false);
});
test("context availability follows recalls of the message and of its quote", t => {
  const state = fixture(t); const context = { ...message("c", "r@chatroom", "200"), quote: { serverId: "201", type: 1 } } as NativeMessage;
  assert.equal(state.isAvailable(context), true);
  state.noteRecall({ threadId: "r@chatroom", serverId: "201", observedAt: "2026-10-21T12:00:00Z" });
  assert.equal(state.isAvailable(context), false); assert.equal(state.isAvailable({ ...context, quote: undefined }), true);
  assert.equal(state.cursor, "0");
});
test("invalid recall identities cannot advance durable custody", t => {
  const state = fixture(t); state.accept("1", message("m"), "live");
  for (const data of [{ threadId: "t", serverId: "0", observedAt: "2026-10-21T12:00:00Z" },
    { messageId: "m", threadId: "t", serverId: "3", observedAt: "invalid" }, { threadId: "t", serverId: "3", observedAt: "2026-10-23T00:00:00Z", replacementText: "bad\u0001notice" }]) {
    assert.throws(() => state.acceptRecall("2", data as any)); assert.equal(state.cursor, "1"); assert.ok(state.get("m"));
  }
});
test("a stale local-ID recall hint applies to the exact server identity and leaves the new occupant running", t => {
  const state = fixture(t); const current = message("reused-id"); state.accept("1", current, "live");
  state.update(current.id, { state: "dispatching", dispatchManifest: manifest([current.id]) });
  const old = { ...current, serverId: "9007199254740998" };
  assert.deepEqual(state.acceptRecall("2", { messageId: current.id, threadId: current.threadId, serverId: old.serverId, observedAt: "2026-10-21T12:00:00Z" }), []);
  assert.equal(state.cursor, "2"); assert.equal(state.isRecalled(current), false); assert.equal(state.isRecalled(old), true);
  assert.equal(state.get(current.id)?.state, "dispatching"); assert.ok(state.get(current.id)?.dispatchManifest); assert.equal(state.get(current.id)?.invalidatedByRecall, undefined);
  state.accept("3", { ...old, id: "old-source-row" }, "live"); assert.equal(state.get("old-source-row"), undefined);
});
test("a new occupant of a source ID gets a new revision and fences the old turn's callback and updates", t => {
  const state = fixture(t); const a = message("reused"); state.accept("1", a, "live");
  const revisionA = state.get(a.id)!.revision;
  state.beginDispatch(a.id, manifest([a.id]));
  const preparedA = state.preparedCallback([a.id], "on_model_dispatch");
  const b = { ...a, serverId: "9007199254741000" };
  assert.equal(state.accept("2", b, "live"), true);
  const revisionB = state.get(b.id)!.revision; assert.notEqual(revisionB, revisionA);
  assert.equal(state.get(b.id)?.state, "received"); assert.equal(state.get(b.id)?.serverId, b.serverId);
  assert.equal(state.update(a.id, { state: "dispatching" }, revisionA), false); assert.equal(state.finish(a.id, revisionA), false);
  assert.throws(() => preparedA("old-run"), /source identity changed/);
  assert.equal(state.accept("3", { ...b }, "live"), false); assert.equal(state.get(b.id)?.revision, revisionB);
  assert.equal(reopen(state).get(b.id)?.revision, revisionB);
});
test("a full queue skips new messages instead of stopping intake", t => {
  const dir = directory(t);
  const open = Object.fromEntries(Array.from({ length: 5000 }, (_, i) => [`q${i}`, { id: `q${i}`, threadId: "t", serverId: String(i + 1), origin: "live", cursor: String(i + 1), revision: `r${i}`, state: "received" }]));
  writeFileSync(join(dir, `${createHash("sha256").update("server\0a").digest("hex")}.json`),
    JSON.stringify({ schemaVersion: 2, serverId: "server", accountId: "a", cursor: "5000", open, reads: {}, boundaries: {} }), { mode: 0o600 });
  const state = new NativeConsumerState(dir, "server", "a");
  assert.equal(state.accept("5001", message("extra", "t", "9999"), "live"), false);
  assert.equal(state.get("extra"), undefined); assert.equal(state.cursor, "5001"); assert.equal(state.skipped.full, 1);
});
test("unknown schemas and foreign identities are refused and the file preserved", t => {
  const dir = directory(t), file = join(dir, `${createHash("sha256").update("server\0a").digest("hex")}.json`);
  for (const value of [{ schemaVersion: 3, serverId: "server", accountId: "a", cursor: "1" }, { schemaVersion: 2, serverId: "other", accountId: "a", cursor: "1", open: {}, reads: {}, boundaries: {} },
    { schemaVersion: 2, serverId: "server", accountId: "a", cursor: "1", open: { x: { id: "y" } }, reads: {}, boundaries: {} }]) {
    const bytes = JSON.stringify(value); writeFileSync(file, bytes, { mode: 0o600 });
    assert.throws(() => new NativeConsumerState(dir, "server", "a"), /preserved/); assert.equal(readFileSync(file, "utf8"), bytes);
  }
});
test("a version 1 file keeps its open work, reads and group context position and drops the message copies", t => {
  const dir = directory(t);
  const item = (id: string, cursor: number, extra: object, threadId = "r@chatroom") => [id, { message: { ...message(id, threadId, String(cursor)), createdAt: `2026-10-20T12:00:0${cursor}Z` }, origin: "live", cursor: String(cursor), revision: `rev-${id}`, ...extra }];
  const items = Object.fromEntries([
    item("answered", 1, { state: "completed", readPending: true, dispatchManifest: manifest(["answered"]) }),
    item("buffered", 2, { state: "ignored", groupContext: true }),
    item("crashed", 3, { state: "dispatching", dispatchManifest: manifest(["crashed"]) }, "dm"),
    item("queued", 4, { state: "received" }, "dm"),
    item("other", 5, { state: "completed", dispatchManifest: manifest(["other"]) }, "other@chatroom"),
  ]);
  writeFileSync(join(dir, `${createHash("sha256").update("server\0a").digest("hex")}.json`),
    JSON.stringify({ schemaVersion: 1, serverId: "server", accountId: "a", cursor: "5", items,
      recalls: { [JSON.stringify(["dm", "77"])]: { threadId: "dm", serverId: "77", observedAt: "2026-10-21T12:00:00Z" } } }), { mode: 0o600 });
  const state = new NativeConsumerState(dir, "server", "a", { now: Date.parse("2026-10-20T13:00:00Z") });
  assert.equal(state.cursor, "5"); assert.deepEqual(state.heldOnOpen, ["crashed"]);
  assert.deepEqual(state.items().map(value => [value.id, value.state, value.revision]), [["queued", "received", "rev-queued"]]);
  assert.equal(state.readPending("r@chatroom"), true);
  assert.deepEqual(state.groupBoundary("r@chatroom"), { cursor: "1", createdAt: "2026-10-20T12:00:02Z" });
  assert.deepEqual(state.groupBoundary("other@chatroom"), { cursor: "5", messageId: "other", createdAt: "2026-10-20T12:00:05Z" });
  const bytes = readFileSync(state.file, "utf8");
  assert.equal(JSON.parse(bytes).schemaVersion, 2); assert.ok(!bytes.includes("private fixture body")); assert.ok(!bytes.includes("observedAt"));
});

const version1 = (dir: string, items: Record<string, unknown>, extra: object = {}) => {
  const file = join(dir, `${createHash("sha256").update("server\0a").digest("hex")}.json`);
  writeFileSync(file, JSON.stringify({ schemaVersion: 1, serverId: "server", accountId: "a", cursor: "500", items, ...extra }), { mode: 0o600 });
  return file;
};
test("a version 1 backlog keeps its recent live incoming messages for the startup limit to choose from", t => {
  const dir = directory(t), now = Date.parse("2026-11-18T12:00:00Z");
  const queued = (i: number, extra: object = {}, origin = "live") => [`q${i}`, { message: { ...message(`q${i}`, "t", String(i + 1)), createdAt: new Date(now - (400 - i) * 60000).toISOString() }, origin, state: "received", cursor: String(i + 1), ...extra }];
  version1(dir, Object.fromEntries([
    ...Array.from({ length: 150 }, (_, i) => queued(i)),
    [`old`, { message: { ...message("old", "t", "990"), createdAt: "2026-11-16T12:00:00Z" }, origin: "live", state: "received", cursor: "200" }],
    queued(160, {}, "catchup"), [`own`, { message: { ...message("own", "t", "991"), direction: "outgoing" }, origin: "live", state: "received", cursor: "201" }],
  ]));
  const state = new NativeConsumerState(dir, "server", "a", { now });
  const kept = state.items().map(item => item.id);
  assert.equal(kept.length, 150); assert.equal(kept[0], "q0"); assert.equal(kept.at(-1), "q149");
  assert.deepEqual(state.skipped, { old: 1, limit: 0, full: 0 });
});
test("unusable version 1 records do not stop the upgrade, but another account's file is refused", t => {
  const dir = directory(t);
  version1(dir, { good: { message: message("good"), origin: "live", state: "received", cursor: "3" }, broken: { message: null, state: "received" },
    mismatched: { message: message("other-id"), state: "received" }, odd: { message: { ...message("odd"), serverId: "not-a-number" }, origin: "live", state: "received", revision: 42 } },
    { recalls: { bad: { threadId: "t" } } });
  const state = new NativeConsumerState(dir, "server", "a", { now: Date.parse("2026-10-20T13:00:00Z") });
  assert.deepEqual(state.items().map(item => [item.id, item.serverId]).sort(), [["good", "9007199254740999"], ["odd", null]]);
  assert.equal(new NativeConsumerState(dir, "server", "a").items().length, 2);
  const foreign = directory(t);
  const file = version1(foreign, { x: { message: { ...message("x"), accountId: "someone-else" }, state: "received" } });
  const bytes = readFileSync(file, "utf8");
  assert.throws(() => new NativeConsumerState(foreign, "server", "a"), /preserved/); assert.equal(readFileSync(file, "utf8"), bytes);
});
test("the version 1 file is backed up once and an existing backup is never overwritten", t => {
  const dir = directory(t), file = version1(dir, {}), original = readFileSync(file, "utf8");
  writeFileSync(`${file}.v1-backup`, "earlier backup", { mode: 0o600 });
  new NativeConsumerState(dir, "server", "a");
  assert.equal(readFileSync(`${file}.v1-backup`, "utf8"), "earlier backup"); assert.notEqual(readFileSync(file, "utf8"), original);
  const fresh = directory(t), freshFile = version1(fresh, {}), freshOriginal = readFileSync(freshFile, "utf8");
  new NativeConsumerState(fresh, "server", "a");
  assert.equal(readFileSync(`${freshFile}.v1-backup`, "utf8"), freshOriginal);
});
