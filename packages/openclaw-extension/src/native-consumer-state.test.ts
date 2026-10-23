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

test("recalls invalidate buffered targets, quoted references and prepared paths before persisting the cursor", t => {
  const state = fixture(t); const target = { ...message("target", "r@chatroom"), body: "original private body",
    link: { title: "Private shared title", description: null, url: "https://example.invalid/private-share", sourceName: null } };
  state.accept("1", target, "live"); state.bufferGroupMessage(target.id, 50);
  const quote = { ...message("quote", "r@chatroom"), serverId: "9007199254741000", type: 49, appMessageType: 57,
    body: "unrelated comment", quote: { serverId: target.serverId!, type: 1 } };
  state.accept("2", quote, "live");
  const trigger = { ...message("trigger", "r@chatroom"), serverId: "9007199254741001" }; state.accept("3", trigger, "live");
  state.beginDispatch(trigger.id, { messageIds: [trigger.id], historyInputs: [{ id: target.id }], assets: [{ path: "/private/original-file", sourceMessageId: target.id }] } as any, [target.id]);
  const prepared = state.preparedCallback([trigger.id], "on_model_dispatch");
  const affected = state.acceptRecall("4", { threadId: target.threadId, serverId: target.serverId!, observedAt: "2026-10-21T12:00:00Z" });
  assert.deepEqual(new Set(affected), new Set([target.id, quote.id, trigger.id]));
  assert.equal(state.get(target.id)?.message.body, "[Message recalled]"); assert.equal(state.get(target.id)?.groupContext, false);
  assert.equal(state.get(target.id)?.message.link, undefined);
  assert.equal(state.get(quote.id)?.message.body, "unrelated comment"); assert.equal(state.get(quote.id)?.message.quote, undefined);
  assert.equal(state.get(trigger.id)?.state, "needs_review"); assert.equal(state.get(trigger.id)?.dispatchManifest, undefined);
  assert.throws(() => prepared("late-run"), /invalidated/);
  const bytes = readFileSync(state.file, "utf8"); assert.ok(!bytes.includes("original private body")); assert.ok(!bytes.includes("/private/original-file"));
  assert.ok(!bytes.includes("private-share")); assert.ok(!bytes.includes("Private shared title"));
  const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a");
  assert.equal(reopened.cursor, "4"); assert.equal(reopened.isRecalled(target), true);
});
test("out-of-order recalls survive restart and suppress a later original without requiring its opaque ID", t => {
  const state = fixture(t); const target = message("late-original");
  state.acceptRecall("1", { threadId: target.threadId, serverId: target.serverId!, observedAt: "2026-10-21T12:00:00Z" });
  const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a");
  reopened.accept("2", target, "live");
  assert.equal(reopened.get(target.id)?.state, "ignored"); assert.equal(reopened.get(target.id)?.message.body, "[Message recalled]");
  assert.equal(reopened.isCurrent(target), false);
  const unrelated = { ...target, id: "other-thread", threadId: "other" }; reopened.accept("3", unrelated, "live");
  assert.equal(reopened.get(unrelated.id)?.state, "received"); assert.equal(reopened.get(unrelated.id)?.message.body, "fixture");
});
test("recalling a quoted original retains the unrelated buffered comment with its quote removed", t => {
  const state = fixture(t); const original = message("original", "r@chatroom");
  const parent = { ...message("parent", "r@chatroom"), serverId: "9007199254741000", type: 49, appMessageType: 57,
    body: "Unrelated comment to keep", quote: { serverId: original.serverId!, type: 1 } };
  state.accept("1", parent, "live"); state.bufferGroupMessage(parent.id, 50);
  state.acceptRecall("2", { threadId: original.threadId, serverId: original.serverId!, observedAt: "2026-10-21T12:00:00Z" });
  const trigger = { ...message("trigger", "r@chatroom"), serverId: "9007199254741001" }; state.accept("3", trigger, "live");
  const history = state.groupHistory(original.threadId, trigger.id, 50);
  assert.equal(history.length, 1); assert.equal(history[0].message.body, "Unrelated comment to keep");
  assert.equal(history[0].message.quote, undefined); assert.equal(history[0].message.id, parent.id);
  assert.equal(state.isCurrent(parent), false);
});
test("invalid recall identities cannot advance durable custody", t => {
  const state = fixture(t); const target = message("m"); state.accept("1", target, "live");
  for (const data of [{ threadId: "t", serverId: "0", observedAt: "2026-10-21T12:00:00Z" },
    { messageId: "m", threadId: "t", serverId: "3", observedAt: "invalid" }]) {
    assert.throws(() => state.acceptRecall("2", data)); assert.equal(state.cursor, "1"); assert.equal(state.get("m")?.message.body, "fixture");
  }
});
test("recall notices keep cached authors and survive replay and restart", t => {
  const state=fixture(t), target={...message('notice'),senderId:'cached-author',direction:'outgoing'};
  state.accept('1',target,'live');
  const recall={threadId:target.threadId,serverId:target.serverId!,observedAt:'2026-10-23T00:00:00Z',replacementText:'Localized notice <text> & detail'};
  state.acceptRecall('2',recall);
  const cleared=state.get(target.id)!.message;
  assert.equal(cleared.senderId,'cached-author'); assert.equal(cleared.direction,'outgoing');
  assert.equal(cleared.body,'[Message recalled]'); assert.equal(cleared.recall?.replacementText,recall.replacementText);
  state.acceptRecall('3',{...recall,replacementText:'Later wording'});
  state.acceptRecall('4',{threadId:target.threadId,serverId:target.serverId!,observedAt:'2026-10-23T01:00:00Z',replacementText:null});
  const reopened=new NativeConsumerState(join(state.file,'..'),'server','a');
  assert.equal(reopened.cursor,'4'); assert.equal(reopened.get(target.id)!.message.senderId,'cached-author');
  assert.equal(reopened.get(target.id)!.message.direction,'outgoing');
  assert.equal(reopened.get(target.id)!.message.recall?.replacementText,recall.replacementText);
  assert.deepEqual(Object.keys(reopened.get(target.id)!.message.recall!).sort(),['observedAt','replacementText']);
});
test("a later API notice enriches an early recall without replacing cached authors", t => {
  const state=fixture(t), target={...message('cached'),senderId:'cached-author',direction:'outgoing'};
  state.accept('1',target,'live');
  const recall={threadId:target.threadId,serverId:target.serverId!,observedAt:'2026-10-23T00:00:00Z'};
  state.acceptRecall('2',recall);
  state.accept('3',{...target,type:10000,senderId:null,direction:null,body:'[Message recalled]',recall:{...recall,replacementText:'Official notice'}},'live');
  assert.equal(state.get(target.id)!.message.senderId,'cached-author');
  assert.equal(state.get(target.id)!.message.direction,'outgoing');
  assert.equal(state.get(target.id)!.message.recall?.replacementText,'Official notice');
});
test("a legacy cleared sender cannot manufacture an incoming author", t => {
  const state=fixture(t), target={...message('legacy-cleared'),type:10000,senderId:'',direction:'incoming',body:'[Message recalled]'};
  state.accept('1',target,'import');
  state.acceptRecall('2',{threadId:target.threadId,serverId:target.serverId!,observedAt:'2026-10-23T00:00:00Z'});
  assert.equal(state.get(target.id)!.message.senderId,null);
  assert.equal(state.get(target.id)!.message.direction,null);
});
test("invalid notices cannot advance or modify persisted recall custody", t => {
  const state=fixture(t), target=message('invalid-metadata'); state.accept('1',target,'live');
  const before=readFileSync(state.file,'utf8');
  for(const change of [{replacementText:42},{replacementText:''},{replacementText:'bad\u0001notice'}, {replacementText:'🧪'.repeat(4097)}]) {
    assert.throws(()=>state.acceptRecall('2',{threadId:target.threadId,serverId:target.serverId!,observedAt:'2026-10-23T00:00:00Z',...change} as any),/recall metadata/);
    assert.equal(state.cursor,'1'); assert.equal(state.get(target.id)!.message.body,'fixture');
    assert.equal(readFileSync(state.file,'utf8'),before);
  }
});
test("a stale local-ID recall hint advances its exact tombstone while preserving the new occupant", t => {
  const state = fixture(t); const current = message("reused-id"); state.accept("1", current, "live");
  state.update(current.id, { state: "dispatching", dispatchManifest: { messageIds: [current.id], assets: [] } as any });
  const old = { ...current, serverId: "9007199254740998", body: "Old recalled body" };
  assert.deepEqual(state.acceptRecall("2", { messageId: current.id, threadId: current.threadId, serverId: old.serverId, observedAt: "2026-10-21T12:00:00Z" }), []);
  assert.equal(state.cursor, "2"); assert.equal(state.isRecalled(current), false); assert.equal(state.isRecalled(old), true);
  assert.equal(state.get(current.id)?.message.body, "fixture"); assert.equal(state.get(current.id)?.state, "dispatching");
  assert.ok(state.get(current.id)?.dispatchManifest); assert.equal(state.get(current.id)?.invalidatedByRecall, undefined);
  const late = { ...old, id: "old-source-row" }; state.accept("3", late, "live");
  assert.equal(state.get(late.id)?.state, "ignored"); assert.equal(state.get(late.id)?.message.body, "[Message recalled]");
});
test("a recalled source ID admits its new exact occupant and fences the old callback and updates", t => {
  const state = fixture(t); const a = message("reused"); state.accept("1", a, "live");
  const revisionA = state.get(a.id)!.revision;
  state.beginDispatch(a.id, { messageIds: [a.id], assets: [] } as any);
  const preparedA = state.preparedCallback([a.id], "on_model_dispatch");
  state.acceptRecall("2", { threadId: a.threadId, serverId: a.serverId!, observedAt: "2026-10-21T12:00:00Z" });
  const b = { ...a, serverId: "9007199254741000", body: "New occupant" };
  assert.equal(state.accept("3", b, "live"), true);
  const revisionB = state.get(b.id)!.revision; assert.notEqual(revisionB, revisionA);
  assert.equal(state.get(b.id)?.state, "received"); assert.equal(state.get(b.id)?.invalidatedByRecall, undefined);
  assert.equal(state.update(a.id, { state: "needs_review", readPending: true }, revisionA), false);
  assert.throws(() => preparedA("old-run"), /source identity changed/);
  assert.equal(state.accept("4", { ...b }, "live"), false); assert.equal(state.get(b.id)?.revision, revisionB);
  assert.equal(state.accept("5", a, "live"), false); assert.equal(state.get(b.id)?.revision, revisionB);
  assert.equal(state.get(b.id)?.message.body, "New occupant"); assert.equal(state.cursor, "5");
  const reopened = new NativeConsumerState(join(state.file, ".."), "server", "a");
  assert.equal(reopened.get(b.id)?.message.body, "New occupant"); assert.equal(reopened.get(b.id)?.revision, revisionB);
  assert.equal(reopened.get(b.id)?.readPending, undefined); assert.equal(reopened.isRecalled(a), true); assert.equal(reopened.isRecalled(b), false);
});
test("replacing historical custody revokes its old manifest without disturbing the fresh occupant", t => {
  const state = fixture(t); const a = message("history", "r@chatroom"); state.accept("1", a, "live"); state.bufferGroupMessage(a.id, 50);
  const trigger = { ...message("trigger", "r@chatroom"), serverId: "9007199254741001" }; state.accept("2", trigger, "live");
  state.beginDispatch(trigger.id, { messageIds: [trigger.id], historyInputs: [{ id: a.id }], assets: [] } as any, [a.id]);
  const prepared = state.preparedCallback([trigger.id], "on_model_dispatch");
  const b = { ...a, serverId: "9007199254741000", body: "New historical occupant" }; state.accept("3", b, "live");
  assert.throws(() => prepared("old-history-run"), /source identity changed/);
  assert.equal(state.get(trigger.id)?.state, "needs_review"); assert.equal(state.get(trigger.id)?.dispatchManifest, undefined);
  assert.equal(state.get(b.id)?.state, "received"); assert.equal(state.get(b.id)?.message.body, "New historical occupant");
});
