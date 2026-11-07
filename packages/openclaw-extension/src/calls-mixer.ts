import type { CallAudioFormat, CallAudioTrack, CallParticipant, NativeCallMediaFrame } from "@agent-wechat/shared";

type Chunk = { offset: number; pcm: Buffer };
type Source = { track: CallAudioTrack; chunks: Chunk[]; sequence?: number; nextOffset: number; muted: boolean };
const SLOT_MS = 20;
const ALIGNMENT_MS = 40;
const MAX_CHUNKS = 64;
const MAX_TRACKS = 256;

/** One call clock, separate source queues, and one PCM output for the model.
 * Arrival order never concatenates simultaneous speakers. No speaker identity
 * is inferred from the mixed output or from silence. */
export class CallPcmMixer {
  private readonly sources = new Map<number, Source>();
  private anchor?: { at: number; offset: number };
  private cursor?: number;
  private readonly slotSamples: number;
  private readonly format: CallAudioFormat;
  constructor(format: CallAudioFormat) {
    if (format.channels !== 1 || format.frameMs !== SLOT_MS || ![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)) throw Error("Unsupported conference PCM format");
    this.slotSamples = format.sampleRate * SLOT_MS / 1000;
    this.format = format;
  }
  synchronize(tracks: { sendTrack: CallAudioTrack; receiveTracks: CallAudioTrack[] }, participants: CallParticipant[] = []): void {
    if (tracks.receiveTracks.length > MAX_TRACKS) throw Error("Too many conference tracks");
    const current = new Set<number>();
    const members = new Map(participants.map(member => [member.participantId, member]));
    for (const track of tracks.receiveTracks) {
      if (track.direction !== "receive" || track.id === tracks.sendTrack.id
        || (tracks.sendTrack.participantId !== undefined && track.participantId === tracks.sendTrack.participantId)) continue;
      if (track.format.sampleRate !== this.format.sampleRate || track.format.channels !== 1 || track.format.frameMs !== SLOT_MS) throw Error("Conference PCM format changed");
      if (current.has(track.id)) throw Error("Duplicate conference track");
      current.add(track.id);
      const member = track.participantId === undefined ? undefined : members.get(track.participantId);
      const muted = member?.membership === "left" || member?.microphoneEnabled === false;
      let source = this.sources.get(track.id);
      if (!source || source.track.generation !== track.generation || source.track.participantId !== track.participantId) {
        source = { track: structuredClone(track), chunks: [], nextOffset: this.cursor ?? 0, muted };
        this.sources.set(track.id, source);
      }
      if (muted !== source.muted) source.chunks.length = 0;
      source.muted = muted;
    }
    for (const id of this.sources.keys()) if (!current.has(id)) this.sources.delete(id);
  }
  push(frame: NativeCallMediaFrame, now: number): void {
    const source = this.sources.get(frame.trackId);
    if (!source || source.track.generation !== frame.generation || source.muted) return;
    if (frame.payload.byteLength % 2 || frame.payload.byteLength > 65536 || !frame.payload.byteLength) throw Error("Invalid conference PCM chunk");
    const offset = frame.sampleOffset ?? source.nextOffset;
    if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isFinite(now)) throw Error("Invalid conference PCM clock");
    if (frame.sequence !== undefined) {
      if (!Number.isSafeInteger(frame.sequence) || frame.sequence < 0) throw Error("Invalid conference PCM sequence");
      if (source.sequence !== undefined && frame.sequence <= source.sequence) return;
      source.sequence = frame.sequence;
    }
    const count = frame.payload.byteLength / 2;
    source.nextOffset = offset + count;
    if (!Number.isSafeInteger(source.nextOffset)) throw Error("Invalid conference PCM clock");
    if (!this.anchor) this.anchor = { at: now, offset: Math.floor(offset / this.slotSamples) * this.slotSamples };
    if (this.cursor !== undefined && offset + count <= this.cursor) return;
    // A remote source cannot make the model accumulate an arbitrary future backlog.
    const horizon = (this.cursor ?? this.anchor.offset) + this.format.sampleRate * 2;
    if (offset > horizon) return;
    const maxBytes = this.format.sampleRate * 4;
    const skip = Math.max(0, frame.payload.byteLength - maxBytes);
    const pcm = Buffer.from(frame.payload.subarray(skip));
    let queuedBytes = source.chunks.reduce((sum, chunk) => sum + chunk.pcm.length, 0);
    while (source.chunks.length && (source.chunks.length >= MAX_CHUNKS || queuedBytes + pcm.length > maxBytes)) queuedBytes -= source.chunks.shift()!.pcm.length;
    source.chunks.push({ offset: offset + skip / 2, pcm });
  }
  render(now: number): { pcm: Buffer; sampleOffset: number } | undefined {
    const anchor = this.anchor;
    if (!anchor || now < anchor.at + ALIGNMENT_MS) return;
    const offset = anchor.offset + Math.floor((now - anchor.at - ALIGNMENT_MS) / SLOT_MS) * this.slotSamples;
    if (this.cursor !== undefined && offset < this.cursor) return;
    const end = offset + this.slotSamples;
    const mixed = new Int32Array(this.slotSamples);
    for (const source of this.sources.values()) {
      if (!source.muted) {
        const filled = new Uint8Array(this.slotSamples);
        for (const chunk of source.chunks) {
          const start = Math.max(offset, chunk.offset);
          const stop = Math.min(end, chunk.offset + chunk.pcm.length / 2);
          for (let at = start; at < stop; at++) {
            const index = at - offset;
            if (!filled[index]) { mixed[index] += chunk.pcm.readInt16LE((at - chunk.offset) * 2); filled[index] = 1; }
          }
        }
      }
      source.chunks = source.chunks.filter(chunk => chunk.offset + chunk.pcm.length / 2 > end);
    }
    let peak = 32767;
    for (const sample of mixed) peak = Math.max(peak, Math.abs(sample));
    const pcm = Buffer.alloc(this.slotSamples * 2);
    for (let i = 0; i < mixed.length; i++) pcm.writeInt16LE(Math.round(mixed[i] * 32767 / peak), i * 2);
    this.cursor = end;
    return { pcm, sampleOffset: offset };
  }
  clear(): void { for (const source of this.sources.values()) source.chunks.length = 0; }
}
