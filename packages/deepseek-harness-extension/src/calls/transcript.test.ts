import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CallTranscript, clipText, formatCallTranscript } from './transcript.ts';

test('snapshot writes create standalone entries', () => {
  const t = new CallTranscript();
  t.add('user', 'hello', true, true, 1_000);
  t.add('assistant', 'hi there', true, true, 2_000);
  assert.deepEqual(t.entries.map(entry => [entry.role, entry.text]), [
    ['user', 'hello'],
    ['assistant', 'hi there'],
  ]);
});

test('streamed deltas accumulate; a final after streaming repeats and is skipped', () => {
  const t = new CallTranscript();
  t.add('assistant', 'Hel', false, false, 1_000);
  t.add('assistant', 'lo wo', false, false, 1_100);
  t.add('assistant', 'Hello world', true, false, 1_200);
  assert.equal(t.entries.length, 1);
  // The streamed deltas stand; the late final repeats them and is dropped.
  assert.equal(t.entries[0]!.text, 'Hello wo');
});

test('a final without prior deltas is written directly', () => {
  const t = new CallTranscript();
  t.add('user', 'what time is it', true, false, 5_000);
  assert.equal(t.entries.length, 1);
  assert.equal(t.entries[0]!.text, 'what time is it');
});

test('clipText flattens whitespace, truncates and never splits surrogate pairs', () => {
  assert.equal(clipText('  a   b  ', 10), 'a b');
  const emoji = '😀'.repeat(20);
  const clipped = clipText(emoji, 5);
  assert.ok(clipped.endsWith('…'));
  assert.ok(!/[\uD800-\uDBFF]$/.test(clipped.replace('…', '')));
});

test('formatCallTranscript stamps call-relative [mm:ss] lines', () => {
  const text = formatCallTranscript(
    [
      { at: 1_000, role: 'user', text: 'hello' },
      { at: 61_000, role: 'assistant', text: 'hi' },
    ],
    1_000,
  );
  assert.equal(text, '[00:00] Caller: hello\n[01:00] You: hi');
});

test('formatCallTranscript keeps head and tail with an elision marker when long', () => {
  const chunk = 'x'.repeat(400);
  const entries = Array.from({ length: 100 }, (_, i) => ({
    at: i * 1_000,
    role: (i % 2 === 0 ? 'user' : 'assistant') as 'user' | 'assistant',
    text: `${i}:${chunk}`,
  }));
  const text = formatCallTranscript(entries, 0);
  assert.ok(text.length < entries.length * 410);
  assert.ok(text.startsWith('[00:00] Caller:'));
  assert.ok(text.includes('\n…\n'));
});
