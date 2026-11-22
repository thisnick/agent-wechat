import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  collectReplyState,
  extractAssistantText,
  trimLeadingBlankLines,
  initReplyState,
  MAX_READY_REPLIES,
  planDeliveries,
  ReplyStateSchema,
  replyOutboundKey,
  type AssistantMessageEventView,
  type ReplyEventView,
  type TurnEndEventView,
} from './reply-collector.ts';

const assistant = (seq: number, turn: number, blocks: unknown[], interrupted?: true): AssistantMessageEventView => ({
  type: 'assistant/message',
  seq,
  data: {
    turn,
    message: { content: blocks, ...(interrupted ? { interrupted } : {}) },
  },
});

const turnEnd = (seq: number, turn: number, kind: string): TurnEndEventView => ({
  type: 'turn/end',
  seq,
  data: { turn, reason: { kind } },
});

const text = (s: string) => ({ type: 'text', text: s });

test('extractAssistantText joins only text blocks in order', () => {
  assert.equal(
    extractAssistantText([text('hello '), { type: 'reasoning', text: 'secret' }, text('world'), { type: 'image' }]),
    'hello world',
  );
});

test('trimLeadingBlankLines drops a leading run of blank lines but keeps content indentation', () => {
  assert.equal(trimLeadingBlankLines('\n\n\n\nHi! 👋 online'), 'Hi! 👋 online');
  assert.equal(trimLeadingBlankLines('  \n\t\n  Hello'), 'Hello');
  // Leading spaces on the first content line (no preceding newline) are kept.
  assert.equal(trimLeadingBlankLines('   indented'), '   indented');
  // Interior blank lines are left untouched.
  assert.equal(trimLeadingBlankLines('\nA\n\nB'), 'A\n\nB');
});

test('completed turn strips leading blank lines from the buffered reply', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('\n\n\nhi there')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  assert.equal(state.ready.length, 1);
  assert.equal(state.ready[0]!.text, 'hi there');
});

test('completed turn emits one reply from its buffered text', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('a'), text('b')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  assert.equal(state.ready.length, 1);
  assert.equal(state.ready[0]!.text, 'ab');
  assert.equal(state.ready[0]!.seq, 1);
  assert.equal(state.ready[0]!.turn, 1);
  assert.deepEqual(state.turns, {});
});

test('aborted/error/max-tokens turns discard buffered text', () => {
  for (const kind of ['aborted', 'error', 'max-tokens', 'interrupted', 'blocked']) {
    let state = initReplyState();
    state = collectReplyState(state, assistant(1, 1, [text('partial')]));
    state = collectReplyState(state, turnEnd(2, 1, kind));
    assert.equal(state.ready.length, 0, kind);
    assert.deepEqual(state.turns, {}, kind);
  }
});

test('interrupted assistant prefix is never buffered', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('cut off')], true));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  assert.equal(state.ready.length, 0);
});

test('multiple turns accumulate in order', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('one')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  state = collectReplyState(state, assistant(3, 2, [text('two')]));
  state = collectReplyState(state, turnEnd(4, 2, 'completed'));
  assert.deepEqual(state.ready.map((r) => r.text), ['one', 'two']);
});

test('unrecognized events keep the same state reference', () => {
  const state = initReplyState();
  const same = collectReplyState(state, { type: 'step/start', seq: 5 } as ReplyEventView);
  assert.equal(same, state);
});

test('ready queue is bounded to MAX_READY_REPLIES', () => {
  let state = initReplyState();
  for (let i = 0; i < MAX_READY_REPLIES + 10; i++) {
    state = collectReplyState(state, assistant(i * 2 + 1, i, [text(String(i))]));
    state = collectReplyState(state, turnEnd(i * 2 + 2, i, 'completed'));
  }
  assert.equal(state.ready.length, MAX_READY_REPLIES);
  assert.equal(state.ready[0]!.text, '10');
});

test('state survives zod schema round-trip', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('persist me')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  const parsed = ReplyStateSchema.parse(JSON.parse(JSON.stringify(state)));
  assert.deepEqual(parsed, state);
});

test('replyOutboundKey is stable and seq-sensitive', () => {
  assert.equal(replyOutboundKey('sess', 7, 3), 'reply:sess:7:3');
  assert.notEqual(replyOutboundKey('sess', 7, 3), replyOutboundKey('sess', 8, 3));
});

test('planDeliveries returns only replies not already enqueued', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('first')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  state = collectReplyState(state, assistant(3, 2, [text('second')]));
  state = collectReplyState(state, turnEnd(4, 2, 'completed'));
  const firstKey = replyOutboundKey('sess', 1, 1);
  const plans = planDeliveries('sess', state, new Set([firstKey]));
  assert.equal(plans.length, 1);
  assert.equal(plans[0]!.text, 'second');
  assert.equal(plans[0]!.key, replyOutboundKey('sess', 3, 2));
});

test('planDeliveries is empty when all replies are enqueued', () => {
  let state = initReplyState();
  state = collectReplyState(state, assistant(1, 1, [text('only')]));
  state = collectReplyState(state, turnEnd(2, 1, 'completed'));
  const key = replyOutboundKey('sess', 1, 1);
  assert.equal(planDeliveries('sess', state, new Set([key])).length, 0);
});
