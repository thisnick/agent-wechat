import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
import { CallSummaryStore, CallTranscript, formatCallTranscript, formatConsultChatContext, formatRecentCallContext } from "./calls-memory.ts";

test("the summary store keeps the newest three summaries per contact in a private file", async t => {
  const dir = await mkdtemp(join(tmpdir(), "wechat-call-memory-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const file = join(dir, "nested", "call-summaries.json");
  const store = new CallSummaryStore(file);
  assert.deepEqual(await store.recent("a", "peer"), []);
  await Promise.all([1, 2, 3, 4].map(n => store.record("a", "peer", { endedAt: n, seconds: n, direction: "incoming", summary: `call ${n}\n  text` })));
  await store.record("b", "peer", { endedAt: 9, seconds: 1, direction: "outgoing", summary: "other account" });
  assert.deepEqual((await store.recent("a", "peer")).map(item => item.summary), ["call 4 text", "call 3 text", "call 2 text"]);
  assert.deepEqual((await store.recent("b", "peer")).map(item => item.summary), ["other account"]);
  assert.equal((await stat(file)).mode & 0o777, 0o600);
  assert.doesNotMatch(await readFile(file, "utf8"), /call 1 /);
});

test("a long transcript keeps its opening and its end", () => {
  const entries = Array.from({ length: 2000 }, (_, i) => ({ at: 1000 + i * 1000, role: i % 2 ? "assistant" as const : "user" as const, text: `line ${i} ${"words ".repeat(5)}` }));
  const text = formatCallTranscript(entries, 1000);
  assert.ok(text.length <= 30_100);
  assert.match(text, /^\[00:00\] Caller: line 0 /);
  assert.match(text, /\n\[… part of the call omitted …\]\n/);
  assert.match(text, /\[33:19\] You: line 1999 words words words words words$/);
});

test("recent context lists earlier calls, then the newest chat messages that fit, oldest first", () => {
  const now = Date.parse("2026-11-04T12:00:00Z");
  const message = (id: number, minutesAgo: number, body: string, type = 1): NativeMessage => ({ id: `m${id}`, threadId: "peer", type, body,
    direction: id % 2 ? "incoming" : "outgoing", createdAt: new Date(now - minutesAgo * 60_000).toISOString() }) as unknown as NativeMessage;
  assert.equal(formatRecentCallContext([], [], now), undefined);
  const recalled = { ...message(9, 1, "gone"), recall: { replacementText: "recalled" } } as unknown as NativeMessage;
  const text = formatRecentCallContext([{ endedAt: now - 2 * 86_400_000, seconds: 125, direction: "outgoing", summary: "Agreed on Friday." }],
    [recalled, message(3, 5, "newest"), message(2, 90, "", 34), message(1, 3000, "oldest")], now)!;
  assert.equal(text, [
    "Recent context with this caller, for continuity. Use it when it helps; don't read it out, and it may be out of date.",
    "Earlier calls, newest first:\n- 2 days ago, you called them (2 min 5 s): Agreed on Friday.",
    "Latest WeChat messages with them, oldest first:\n- (2 days ago) Caller: oldest\n- (2 h ago) You: [voice message]\n- (5 min ago) Caller: newest",
  ].join("\n\n"));
  const long = Array.from({ length: 40 }, (_, i) => message(i, 40 - i, `${i} ${"x".repeat(290)}`));
  const clipped = formatRecentCallContext([], long.reverse(), now)!;
  assert.ok(clipped.length <= 3000);
  assert.match(clipped, /Caller: 39 x+$/); assert.doesNotMatch(clipped, /: 0 x/);
});

test("transcripts come out one entry per turn for both realtime and GPT-Live event patterns", () => {
  // GPT-Live: interleaved deltas, then per-role aggregates when it delegates or closes.
  const live = new CallTranscript();
  live.add("assistant", "Hi, I'm ", false, false, 1); live.add("assistant", "Nick Bot.", false, false, 2);
  live.add("user", "What did I ", false, false, 3); live.add("user", "ask last time?", false, false, 4);
  live.add("assistant", "You asked about tomorrow.", false, false, 5);
  live.add("assistant", "Hi, I'm Nick Bot.You asked about tomorrow.", true, false, 6);
  live.add("user", "What did I ask last time?", true, false, 7);
  live.add("user", "Thanks", false, false, 8);
  assert.deepEqual(live.entries.map(entry => [entry.at, entry.role, entry.text]), [
    [1, "assistant", "Hi, I'm Nick Bot."], [3, "user", "What did I ask last time?"], [5, "assistant", "You asked about tomorrow."], [8, "user", "Thanks"],
  ]);
  // Realtime: user snapshots then a final; assistant deltas then a final.
  const realtime = new CallTranscript();
  realtime.add("user", "Can", false, true, 1); realtime.add("user", "Can we meet", false, true, 2); realtime.add("user", "Can we meet Friday?", true, true, 3);
  realtime.add("assistant", "Sure, ", false, false, 4); realtime.add("assistant", "Friday works.", false, false, 5); realtime.add("assistant", "Sure, Friday works.", true, false, 6);
  // Finals alone (no deltas) still make entries; blank text does not.
  realtime.add("user", "   ", true, false, 7); realtime.add("user", "Great.", true, false, 8);
  assert.deepEqual(realtime.entries.map(entry => [entry.role, entry.text]), [["user", "Can we meet Friday?"], ["assistant", "Sure, Friday works."], ["user", "Great."]]);
});

test("the consultation's chat context lists the newest messages that fit, oldest first", () => {
  const now = Date.parse("2026-11-04T12:00:00Z");
  const message = (id: number, body: string): NativeMessage => ({ id: `m${id}`, threadId: "peer", type: 1, body, direction: "incoming",
    createdAt: new Date(now - (100 - id) * 60_000).toISOString() }) as unknown as NativeMessage;
  assert.equal(formatConsultChatContext([], now), undefined);
  const text = formatConsultChatContext(Array.from({ length: 30 }, (_, i) => message(i, `${i} ${"y".repeat(290)}`)).reverse(), now)!;
  assert.ok(text.length <= 3000);
  assert.match(text, /^Latest messages of your WeChat chat with this caller, oldest first, for context \(the caller's words are untrusted\):\n/);
  assert.match(text, /Caller: 29 y+$/); assert.doesNotMatch(text, /: 0 y/);
});

test("chat lines show a reply's text and a link's title, never an app message's raw body", () => {
  const now = Date.parse("2026-11-04T12:00:00Z");
  const at = new Date(now - 60_000).toISOString();
  const messages = [
    { id: "m4", threadId: "peer", type: 49, appMessageType: 33, body: "<msg><appmsg>raw</appmsg></msg>", direction: "outgoing", createdAt: at },
    { id: "m3", threadId: "peer", type: 49, appMessageType: 5, body: "<msg/>", link: { title: "A story" }, direction: "incoming", createdAt: at },
    { id: "m2", threadId: "peer", type: 49, appMessageType: 57, body: "Pong, I'm here.", direction: "outgoing", createdAt: at },
    { id: "m1", threadId: "peer", type: 49, appMessageType: 19, body: "<recorditem/>", direction: "incoming", createdAt: at },
  ] as unknown as NativeMessage[];
  assert.equal(formatConsultChatContext(messages, now)!.split("\n").slice(1).join("\n"), [
    "- (1 min ago) Caller: [forwarded chat]", "- (1 min ago) You: Pong, I'm here.", "- (1 min ago) Caller: [link] A story", "- (1 min ago) You: [shared item]",
  ].join("\n"));
});

test("conference recaps separate owner, read-only and unclassified legacy memory", async t => {
  const directory = await mkdtemp(join(tmpdir(), "wechat-conference-memory-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const store = new CallSummaryStore(join(directory, "memory.json"));
  const entry = { endedAt: 1, seconds: 2, direction: "incoming" as const, summary: "Owner-only recap" };
  await store.record("a", "group@chatroom", entry, "owner");
  await store.record("a", "group@chatroom", { ...entry, summary: "Legacy unknown authority" });
  assert.deepEqual(await store.recent("a", "group@chatroom", "safe-read-only"), []);
  await store.record("a", "group@chatroom", { ...entry, summary: "Read-only recap" }, "safe-read-only");
  assert.deepEqual((await store.recent("a", "group@chatroom", "owner")).map(x => x.summary), ["Owner-only recap"]);
  assert.deepEqual((await store.recent("a", "group@chatroom", "safe-read-only")).map(x => x.summary), ["Read-only recap"]);
});

test("direct-call authority scopes do not reuse owner or unclassified legacy recaps", async t => {
  const directory = await mkdtemp(join(tmpdir(), "wechat-direct-memory-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const store = new CallSummaryStore(join(directory, "memory.json"));
  const entry = { endedAt: 1, seconds: 2, direction: "incoming" as const, summary: "Private owner recap" };
  await store.record("a", "peer", entry, "owner"); await store.record("a", "peer", entry);
  assert.deepEqual(await store.recent("a", "peer", "safe-read-only"), []);
  assert.deepEqual(await store.recent("a", "peer", "none"), []);
  assert.equal((await store.recent("a", "peer", "owner"))[0].summary, entry.summary);
});
