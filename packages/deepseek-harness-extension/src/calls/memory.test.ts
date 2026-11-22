import assert from 'node:assert/strict';
import { test } from 'node:test';
import { memoryCallMemoryTable, memoryKey, CallSummaryStore } from './memory.ts';
import type { CallSummary } from './memory.ts';

const summary = (endedAt: number, text = 'talked'): CallSummary => ({ endedAt, seconds: 60, direction: 'incoming', summary: text });

test('memoryKey isolates policy-keyed entries from legacy keys', () => {
  assert.equal(memoryKey('default', 'wxid_a'), 'default/wxid_a');
  assert.equal(memoryKey('default', 'wxid_a', 'owner'), JSON.stringify(['default', 'wxid_a', 'owner']));
});

test('record keeps the three newest summaries per contact, newest first', async () => {
  const store = new CallSummaryStore(memoryCallMemoryTable());
  await store.record('default', 'wxid_a', summary(1));
  await store.record('default', 'wxid_a', summary(2));
  await store.record('default', 'wxid_a', summary(3));
  await store.record('default', 'wxid_a', summary(4));
  const recent = await store.recent('default', 'wxid_a');
  assert.deepEqual(recent.map(entry => entry.endedAt), [4, 3, 2]);
});

test('policy-keyed and legacy entries live in separate buckets', async () => {
  const store = new CallSummaryStore(memoryCallMemoryTable());
  await store.record('default', 'wxid_a', summary(1), 'owner');
  assert.deepEqual(await store.recent('default', 'wxid_a'), []);
  assert.equal((await store.recent('default', 'wxid_a', 'owner')).length, 1);
});

test('overlapping writes serialize; every write lands', async () => {
  const store = new CallSummaryStore(memoryCallMemoryTable());
  await Promise.all([
    store.record('default', 'wxid_a', summary(1)),
    store.record('default', 'wxid_a', summary(2)),
    store.record('default', 'wxid_a', summary(3)),
  ]);
  const recent = await store.recent('default', 'wxid_a');
  assert.equal(recent.length, 3);
});

test('formatSummaryRecap lists recent calls or returns empty', async () => {
  const { formatSummaryRecap } = await import('./memory.ts');
  assert.equal(formatSummaryRecap([]), '');
  const text = formatSummaryRecap([summary(1, 'planned the launch'), summary(2, 'quick hello')]);
  assert.match(text, /^Recent calls with this contact:/);
  assert.match(text, /\(incoming, 60s\) quick hello/);
});
