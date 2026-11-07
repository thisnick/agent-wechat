import test from "node:test";
import assert from "node:assert/strict";
import type { CallAudioTrack, CallParticipant } from "@agent-wechat/shared";
import { CallPcmMixer } from "./calls-mixer.ts";

const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
const sendTrack: CallAudioTrack = { id: 1, generation: 1, direction: "send", format };
const track = (id: number, generation = 1, participantId?: string): CallAudioTrack => ({ id, generation, direction: "receive", format, ...(participantId ? { participantId } : {}) });
function pcm(value: number, count = 320): Buffer { const bytes = Buffer.alloc(count * 2); for (let i = 0; i < count; i++) bytes.writeInt16LE(value, i * 2); return bytes; }
const frame = (id: number, value: number, offset = 0, generation = 1, sequence = 0) => ({ trackId: id, generation, sequence, sampleOffset: offset, payload: pcm(value) });
const all = (bytes: Buffer, value: number) => { for (let i = 0; i < bytes.length; i += 2) assert.equal(bytes.readInt16LE(i), value); };
const member = (microphoneEnabled?: boolean): CallParticipant => ({ participantId: "a", contactId: "contact-a", membership: "joined", ...(microphoneEnabled === undefined ? {} : { microphoneEnabled }) });

test("simultaneous conference sources share a clock and mix after 40ms, never concatenate", () => {
  const mixer = new CallPcmMixer(format);
  mixer.synchronize({ sendTrack, receiveTracks: [track(7), track(8)] });
  mixer.push(frame(8, 2000), 0);
  mixer.push(frame(7, 1000), 25);
  assert.equal(mixer.render(39), undefined);
  const first = mixer.render(40)!;
  assert.equal(first.sampleOffset, 0); all(first.pcm, 3000);
  assert.equal(mixer.render(40), undefined);
  all(mixer.render(60)!.pcm, 0);
});
test("track retirement and generation replacement discard queued and stale audio", () => {
  const mixer = new CallPcmMixer(format);
  mixer.synchronize({ sendTrack, receiveTracks: [track(7)] });
  mixer.push(frame(7, 1000), 0);
  mixer.synchronize({ sendTrack, receiveTracks: [] });
  mixer.synchronize({ sendTrack, receiveTracks: [track(7, 2)] });
  mixer.push(frame(7, 9000), 1);
  mixer.push(frame(7, 2000, 0, 2), 2);
  all(mixer.render(40)!.pcm, 2000);
});
test("mute clears a source, unknown is not mute, and unmute needs new audio", () => {
  const mixer = new CallPcmMixer(format);
  const tracks = { sendTrack, receiveTracks: [track(7, 1, "a")] };
  mixer.synchronize(tracks, [member()]); mixer.push(frame(7, 1000), 0);
  mixer.synchronize(tracks, [member(false)]);
  all(mixer.render(40)!.pcm, 0);
  mixer.push(frame(7, 9000, 320, 1, 1), 45);
  mixer.synchronize(tracks, [member(true)]);
  all(mixer.render(60)!.pcm, 0);
  mixer.push(frame(7, 2000, 640, 1, 2), 65);
  all(mixer.render(80)!.pcm, 2000);
});
test("a stalled consumer jumps to the current slot and late frames do not create a backlog", () => {
  const mixer = new CallPcmMixer(format);
  mixer.synchronize({ sendTrack, receiveTracks: [track(7)] });
  mixer.push(frame(7, 1000), 0); mixer.render(40);
  mixer.push(frame(7, 2000, 320, 1, 1), 50);
  const current = mixer.render(500)!;
  assert.equal(current.sampleOffset, 7360); all(current.pcm, 0);
  mixer.push(frame(7, 9000, 320, 1, 2), 501);
  assert.equal(mixer.render(501), undefined);
  all(mixer.render(520)!.pcm, 0);
});
test("mixing limits peaks, excludes an identified local source and ignores replayed sequences", () => {
  const mixer = new CallPcmMixer(format);
  mixer.synchronize({ sendTrack: { ...sendTrack, participantId: "self" }, receiveTracks: [track(7), track(8), track(9, 1, "self")] });
  mixer.push(frame(7, 30000), 0); mixer.push(frame(8, 30000), 0);
  mixer.push(frame(7, -30000), 1); mixer.push(frame(9, -30000), 1);
  all(mixer.render(40)!.pcm, 32767);
});
test("source format changes are reported and replacing participant association clears audio", () => {
  const mixer = new CallPcmMixer(format);
  mixer.synchronize({ sendTrack, receiveTracks: [track(7, 1, "a")] }); mixer.push(frame(7, 1000), 0);
  mixer.synchronize({ sendTrack, receiveTracks: [track(7, 1, "b")] });
  all(mixer.render(40)!.pcm, 0);
  assert.throws(() => mixer.synchronize({ sendTrack, receiveTracks: [{ ...track(7), format: { ...format, sampleRate: 24000 } }] }), /format/);
});
