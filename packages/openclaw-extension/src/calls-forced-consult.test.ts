import assert from "node:assert/strict";
import { test } from "node:test";
import { createCallForcedConsult } from "./calls-forced-consult.ts";

const sdkPath: string = "openclaw/plugin-sdk/realtime-voice";
const stock = await import(sdkPath);

function fixture(answer: { text: string; fastContext?: true } = { text: "Verified answer" }) {
  const timers = new Set<() => void>();
  const questions: string[] = [];
  const spoken: string[] = [];
  const fastContextSpoken: boolean[] = [];
  let clears = 0;
  let finish = () => {};
  const forced = createCallForcedConsult({
    classifySkippableRealtimeVoiceConsultTranscript: stock.classifySkippableRealtimeVoiceConsultTranscript,
    createRealtimeVoiceForcedConsultCoordinator: () => stock.createRealtimeVoiceForcedConsultCoordinator({
      setTimer(fn: () => void) { timers.add(fn); return { clear: () => timers.delete(fn) }; },
    }),
  }, {
    active: () => true,
    consult(question) {
      questions.push(question);
      return new Promise(resolve => { finish = () => resolve(answer); });
    },
    clearAudio() { clears++; },
    speak(text, fastContext) { spoken.push(text); fastContextSpoken.push(fastContext === true); },
  });
  return { forced, questions, spoken, fastContextSpoken, get clears() { return clears; },
    fire() { for (const callback of [...timers]) if (timers.delete(callback)) callback(); },
    async complete() { finish(); await new Promise<void>(resolve => setImmediate(resolve)); },
  };
}

test("always consults only for a final caller question and speaks the result", async () => {
  const f = fixture();
  f.forced.onTranscript("user", "What is the", false);
  f.forced.onTranscript("assistant", "Private assistant output", true);
  f.fire();
  assert.deepEqual(f.questions, []);
  f.forced.onTranscript("user", "What is the schedule?", true);
  f.fire();
  assert.deepEqual(f.questions, ["What is the schedule?"]);
  assert.equal(f.clears, 1);
  await f.complete();
  assert.deepEqual(f.spoken, ["Verified answer"]);
  assert.equal(f.clears, 2);
  f.forced.close();
});

test("native consultation cancels a matching fallback, or shares the one already running", async () => {
  for (const fallbackFirst of [false, true]) {
    const f = fixture();
    const question = "What is the schedule?";
    f.forced.onTranscript("user", question, true);
    if (fallbackFirst) f.fire();
    const native = f.forced.native(question, "tool-1");
    f.fire();
    assert.deepEqual(f.questions, [question]);
    assert.deepEqual(await f.forced.native("Look up a different question", "tool-2"), {
      error: "A consultation is in progress. Wait for its result before retrying.", retryable: true, started: false,
    });
    await f.complete();
    assert.deepEqual(await native, { text: "Verified answer" });
    assert.deepEqual(f.spoken, []);
    f.forced.close();
  }
});

test("closing a call clears a queued fallback and suppresses any result already in flight", async () => {
  for (const start of [false, true]) {
    const f = fixture();
    f.forced.onTranscript("user", "What is the schedule?", true);
    if (start) f.fire();
    f.forced.close();
    f.fire();
    await f.complete();
    assert.equal(f.questions.length, start ? 1 : 0);
    assert.deepEqual(f.spoken, []);
  }
});

test("a fast-context fallback answer is marked for interpretation and does not block asking the same question again", async () => {
  const question = "What did we decide?";
  const agent = fixture();
  agent.forced.onTranscript("user", question, true); agent.fire(); await agent.complete();
  assert.deepEqual(agent.fastContextSpoken, [false]);
  assert.deepEqual(await agent.forced.native(question, "tool-1"), { text: "The caller already received this answer. Do not repeat it.", alreadyDelivered: true });
  agent.forced.close();

  const fast = fixture({ text: "Synthetic memory snippet", fastContext: true });
  fast.forced.onTranscript("user", question, true); fast.fire(); await fast.complete();
  assert.deepEqual(fast.spoken, ["Synthetic memory snippet"]);
  assert.deepEqual(fast.fastContextSpoken, [true]);
  const again = fast.forced.native(question, "tool-1");
  await fast.complete();
  assert.deepEqual(fast.questions, [question, question], "asking again starts a new consultation");
  assert.deepEqual(await again, { text: "Synthetic memory snippet", fastContext: true });
  fast.forced.close();
});
