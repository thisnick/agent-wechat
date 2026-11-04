import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
import { CallSummaryStore, formatCallTranscript, formatRecentCallContext } from "./calls-memory.ts";

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
