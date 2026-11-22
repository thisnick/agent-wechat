import assert from 'node:assert/strict';
import { test } from 'node:test';
import { downsamplePcm16, readPcm16Stats, SpeechGate } from './audio.ts';

const loud = (): Int16Array => new Int16Array(80).fill(3000);
const quiet = (): Int16Array => new Int16Array(80);

test('readPcm16Stats reports rms and peak', () => {
  const stats = readPcm16Stats(new Int16Array([3000, -3000, 3000, -3000]));
  assert.equal(stats.peak, 3000);
  assert.equal(stats.rms, 3000);
  assert.equal(readPcm16Stats(new Int16Array([0, 0])).rms, 0);
});

test('gate opens after the speech-frame run and fires the trigger once', () => {
  let triggers = 0;
  const gate = new SpeechGate();
  assert.equal(gate.accept(readPcm16Stats(quiet()), 0), false);
  assert.equal(gate.accept(readPcm16Stats(loud()), 10), false); // run 1
  assert.equal(gate.accept(readPcm16Stats(loud()), 20), false); // run 2
  assert.equal(gate.accept(readPcm16Stats(loud()), 30), false); // run 3
  assert.equal(gate.accept(readPcm16Stats(loud()), 40, () => { triggers += 1; return true; }), true); // open
  assert.equal(triggers, 1);
  assert.equal(gate.accept(readPcm16Stats(loud()), 50), true);
  assert.equal(triggers, 1);
});

test('gate closes after sustained silence and honors the cooldown', () => {
  const gate = new SpeechGate({ cooldownMs: 300 });
  for (let i = 0; i < 4; i++) gate.accept(readPcm16Stats(loud()), i * 10);
  assert.equal(gate.accept(readPcm16Stats(loud()), 40), true); // opened during the run
  for (let i = 0; i < 10; i++) gate.accept(readPcm16Stats(quiet()), 100 + i * 10);
  assert.equal(gate.accept(readPcm16Stats(quiet()), 200), false); // closed again
  // Reopening inside the cooldown is refused: 230 - 30 < 300.
  for (let i = 0; i < 4; i++) gate.accept(readPcm16Stats(loud()), 200 + i * 10);
  assert.equal(gate.accept(readPcm16Stats(loud()), 230, () => true), false);
  // At 360 the cooldown (since the 30 ms trigger) has elapsed, so it opens.
  for (let i = 0; i < 3; i++) gate.accept(readPcm16Stats(loud()), 330 + i * 10);
  assert.equal(gate.accept(readPcm16Stats(loud()), 360, () => true), true);
});

test('a declined trigger reports false for that frame but the gate stays open', () => {
  const gate = new SpeechGate();
  for (let i = 0; i < 3; i++) gate.accept(readPcm16Stats(loud()), 300 + i * 10);
  // The opening frame asks the caller; declining reports false exactly once.
  assert.equal(gate.accept(readPcm16Stats(loud()), 330, () => false), false);
  assert.equal(gate.accept(readPcm16Stats(loud()), 340), true);
});

test('downsamplePcm16 shrinks the frame proportionally', () => {
  const input = Int16Array.from({ length: 480 }, (_, i) => (i % 2 === 0 ? 1000 : -1000));
  const output = downsamplePcm16(input, 24_000, 8_000);
  assert.equal(output.length, 160);
});
