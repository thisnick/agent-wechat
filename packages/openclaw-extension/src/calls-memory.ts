import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";

/** What the voice model heard and said, in order: the input to a call summary. */
export type CallTranscriptEntry = { at: number; role: "user" | "assistant"; text: string };
export type CallSummary = { endedAt: number; seconds: number; direction: "incoming" | "outgoing"; summary: string };

const SUMMARIES_PER_CONTACT = 3;
const MAX_CONTACTS = 500;
const MAX_SUMMARY_CHARS = 1200;
/** A long call keeps its opening (why they called) and its most recent part. */
const TRANSCRIPT_HEAD_CHARS = 6_000;
const TRANSCRIPT_TAIL_CHARS = 24_000;
const MAX_CONTEXT_CHARS = 3_000;
const MAX_MESSAGE_CHARS = 300;

/**
 * The last few call summaries for each contact, which the next call's voice
 * model gets as recent context. This is OpenClaw-side call memory, not WeChat
 * state: the summaries exist nowhere in the official profile. Transcripts are
 * not kept here; they go to the agent in the call's own session.
 */
export class CallSummaryStore {
  private writing: Promise<void> = Promise.resolve();
  private readonly file: string;
  constructor(file: string) { this.file = file; }

  async recent(accountId: string, contactId: string): Promise<CallSummary[]> {
    return (await this.read())[key(accountId, contactId)] ?? [];
  }

  record(accountId: string, contactId: string, summary: CallSummary): Promise<void> {
    // One writer at a time: each write starts from the previous one's result.
    const next = this.writing.then(async () => {
      const all = await this.read();
      const id = key(accountId, contactId);
      all[id] = [{ ...summary, summary: clip(summary.summary, MAX_SUMMARY_CHARS) }, ...(all[id] ?? [])].slice(0, SUMMARIES_PER_CONTACT);
      const kept = Object.entries(all).sort(([, a], [, b]) => (b[0]?.endedAt ?? 0) - (a[0]?.endedAt ?? 0)).slice(0, MAX_CONTACTS);
      await mkdir(dirname(this.file), { recursive: true, mode: 0o700 });
      const temporary = `${this.file}.${process.pid}.tmp`;
      await writeFile(temporary, `${JSON.stringify(Object.fromEntries(kept))}\n`, { mode: 0o600 });
      await rename(temporary, this.file);
    });
    this.writing = next.catch(() => {});
    return next;
  }

  private async read(): Promise<Record<string, CallSummary[]>> {
    try {
      const value = JSON.parse(await readFile(this.file, "utf8"));
      if (!value || typeof value !== "object" || Array.isArray(value)) return {};
      const result: Record<string, CallSummary[]> = {};
      for (const [id, items] of Object.entries(value)) {
        if (Array.isArray(items)) result[id] = items.filter(isSummary).slice(0, SUMMARIES_PER_CONTACT);
      }
      return result;
    } catch { return {}; }
  }
}

const key = (accountId: string, contactId: string) => `${accountId}/${contactId}`;
const isSummary = (item: unknown): item is CallSummary => {
  const value = item as Partial<CallSummary> | null;
  return !!value && typeof value.summary === "string" && Number.isFinite(value.endedAt) && Number.isFinite(value.seconds)
    && (value.direction === "incoming" || value.direction === "outgoing");
};

function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length <= max ? flat : `${flat.slice(0, max - 1).replace(/[\uD800-\uDBFF]$/, "").trimEnd()}…`;
}

function stamp(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}

/** "Caller"/"You" lines with call-relative times, cut in the middle when long. */
export function formatCallTranscript(entries: CallTranscriptEntry[], startedAt: number): string {
  const lines = entries.map(entry => `[${stamp(entry.at - startedAt)}] ${entry.role === "user" ? "Caller" : "You"}: ${entry.text.replace(/\s+/g, " ").trim()}`);
  const text = lines.join("\n");
  if (text.length <= TRANSCRIPT_HEAD_CHARS + TRANSCRIPT_TAIL_CHARS) return text;
  const head = text.slice(0, TRANSCRIPT_HEAD_CHARS);
  const tail = text.slice(-TRANSCRIPT_TAIL_CHARS);
  return `${head.slice(0, head.lastIndexOf("\n"))}\n[… part of the call omitted …]\n${tail.slice(tail.indexOf("\n") + 1)}`;
}

export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m} min ${s % 60} s` : `${Math.floor(m / 60)} h ${m % 60} min`;
}

function ago(ms: number, now: number): string {
  const minutes = Math.round((now - ms) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  return days === 1 ? "yesterday" : `${days} days ago`;
}

const KINDS: Record<number, string> = { 3: "image", 34: "voice message", 42: "contact card", 43: "video", 47: "sticker", 48: "location", 49: "shared item", 50: "call" };

/** One chat message as the voice model sees it; system notices and recalls are skipped. */
function chatLine(message: NativeMessage, now: number): string | undefined {
  if (message.recall || message.type === 10000 || message.type === 10002) return;
  const body = message.type === 1 ? clip(message.body ?? "", MAX_MESSAGE_CHARS) : `[${KINDS[message.type] ?? "message"}]`;
  if (!body) return;
  const at = message.createdAt ? Date.parse(message.createdAt) : NaN;
  const who = message.direction === "outgoing" ? "You" : "Caller";
  return `- ${Number.isFinite(at) ? `(${ago(at, now)}) ` : ""}${who}: ${body}`;
}

/**
 * The recent-context section of the voice instructions: earlier calls with
 * this contact, then the latest messages of the chat with them, oldest first.
 * `messages` is newest first, as the native API returns it.
 */
export function formatRecentCallContext(summaries: CallSummary[], messages: NativeMessage[], now = Date.now()): string | undefined {
  const calls = summaries.map(item => `- ${ago(item.endedAt, now)}, ${item.direction === "incoming" ? "they called" : "you called them"} (${formatDuration(item.seconds)}): ${clip(item.summary, 600)}`);
  const chat = messages.slice().reverse().flatMap(message => chatLine(message, now) ?? []);
  if (!calls.length && !chat.length) return;
  const build = () => [
    "Recent context with this caller, for continuity. Use it when it helps; don't read it out, and it may be out of date.",
    ...(calls.length ? [`Earlier calls, newest first:\n${calls.join("\n")}`] : []),
    ...(chat.length ? [`Latest WeChat messages with them, oldest first:\n${chat.join("\n")}`] : []),
  ].join("\n\n");
  let text = build();
  // Keep the newest messages when the whole does not fit.
  while (text.length > MAX_CONTEXT_CHARS && chat.length) { chat.shift(); text = build(); }
  return text.slice(0, MAX_CONTEXT_CHARS);
}
