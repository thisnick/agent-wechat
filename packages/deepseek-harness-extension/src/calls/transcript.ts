/**
 * Call transcript and summary formatting (DESIGN.md §6.3). Ported from the
 * OpenClaw adapter's `calls-memory.ts` transcript half: one entry per turn,
 * realtime deltas accumulate, a final for a role that already streamed deltas
 * repeats them and is skipped. A long call keeps its opening and its most
 * recent part. No audio is ever recorded — text only.
 */
export type CallTranscriptEntry = { at: number; role: 'user' | 'assistant'; text: string };

const MAX_TRANSCRIPT_ENTRIES = 2_000;
const MAX_ENTRY_CHARS = 4_000;
const TRANSCRIPT_HEAD_CHARS = 6_000;
const TRANSCRIPT_TAIL_CHARS = 24_000;

export class CallTranscript {
  readonly entries: CallTranscriptEntry[] = [];
  private open: CallTranscriptEntry | undefined;
  private streamed: Record<'user' | 'assistant', boolean> = { user: false, assistant: false };

  add(role: 'user' | 'assistant', text: string, final: boolean, snapshot: boolean, at = Date.now()): void {
    if (snapshot || (final && !this.streamed[role])) this.write(role, text, at, true);
    else if (!final) {
      this.write(role, text, at, false);
      this.streamed[role] = true;
    }
    if (final) {
      this.streamed[role] = false;
      if (this.open?.role === role) this.open = undefined;
    }
  }

  private write(role: 'user' | 'assistant', text: string, at: number, replace: boolean): void {
    if (this.open?.role === role) {
      this.open.text = (replace ? text : this.open.text + text).slice(0, MAX_ENTRY_CHARS);
    } else if (this.entries.length < MAX_TRANSCRIPT_ENTRIES && text.trim()) {
      this.open = { at, role, text: text.slice(0, MAX_ENTRY_CHARS) };
      this.entries.push(this.open);
    }
  }
}

export function clipText(text: string, max: number): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length <= max ? flat : `${flat.slice(0, max - 1).replace(/[\uD800-\uDBFF]$/, '').trimEnd()}…`;
}

function stamp(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
}

/** "Caller"/"You" lines with call-relative times, cut in the middle when long. */
export function formatCallTranscript(entries: CallTranscriptEntry[], startedAt: number): string {
  const lines = entries
    .filter(entry => entry.text.trim())
    .map(entry => `[${stamp(entry.at - startedAt)}] ${entry.role === 'user' ? 'Caller' : 'You'}: ${entry.text.replace(/\s+/g, ' ').trim()}`);
  const text = lines.join('\n');
  if (text.length <= TRANSCRIPT_HEAD_CHARS + TRANSCRIPT_TAIL_CHARS) return text;
  const head = text.slice(0, TRANSCRIPT_HEAD_CHARS);
  const tail = text.slice(-TRANSCRIPT_TAIL_CHARS);
  return `${head}\n…\n${tail}`;
}
