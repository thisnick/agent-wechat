/**
 * M2 reply collection (DESIGN.md §5).
 *
 * Consumes COMMITTED session events via a pure `sessionProjections` fold and
 * produces a bounded queue of replies that are safe to send to WeChat:
 * only assistant text from turns that closed with `reason: completed`.
 * Interrupted / aborted / error / max-tokens tails are discarded.
 *
 * The module is deliberately host-agnostic at runtime: the fold works on a
 * minimal structural event shape, so it can be unit-tested with plain objects
 * and no live DSH runtime. The adapter binds real `SessionEvent`s in `apply`.
 */

import { z } from 'zod';

// ---------------------------------------------------------------------------
// Minimal structural event shape (a subset of DSH SessionEvent)
// ---------------------------------------------------------------------------

export interface TextBlockView {
  readonly type: 'text';
  readonly text: string;
}

export interface AssistantMessageView {
  readonly content: readonly unknown[];
  readonly interrupted?: true;
}

export interface AssistantMessageEventView {
  readonly type: 'assistant/message';
  readonly seq: number;
  readonly data: {
    readonly turn: number;
    readonly message: AssistantMessageView;
  };
}

export interface TurnEndEventView {
  readonly type: 'turn/end';
  readonly seq: number;
  readonly data: {
    readonly turn: number;
    readonly reason: { readonly kind: string };
  };
}

export type ReplyEventView = AssistantMessageEventView | TurnEndEventView | { readonly type: string; readonly seq: number };

// ---------------------------------------------------------------------------
// Projection state
// ---------------------------------------------------------------------------

/** One sendable reply extracted from a completed turn. */
export interface ReadyReply {
  /** DSH session event seq of the final assistant message in the turn. */
  readonly seq: number;
  readonly turn: number;
  readonly text: string;
}

export const ReplyStateSchema = z.object({
  /** assistant text buffered per still-open turn (turn number as key). */
  turns: z.record(z.string(), z.array(z.object({ seq: z.number().int(), text: z.string() }))),
  /** Completed-turn replies, oldest first, bounded. */
  ready: z.array(z.object({ seq: z.number().int(), turn: z.number().int(), text: z.string() })),
});
export type ReplyState = z.infer<typeof ReplyStateSchema>;

/** Keep the ready queue bounded across very long-lived sessions. */
export const MAX_READY_REPLIES = 200;

export function initReplyState(): ReplyState {
  return { turns: {}, ready: [] };
}

/** Extract plain text blocks; reasoning / tool / image / file blocks are ignored. */
export function extractAssistantText(content: readonly unknown[]): string {
  const parts: string[] = [];
  for (const block of content) {
    if (
      block &&
      typeof block === 'object' &&
      (block as { type?: unknown }).type === 'text' &&
      typeof (block as { text?: unknown }).text === 'string'
    ) {
      parts.push((block as TextBlockView).text);
    }
  }
  return parts.join('');
}

/**
 * Strip leading whitespace-only lines from a completed reply. Some models
 * emit a run of `\n` at the head of their first text block; forwarding those
 * verbatim shows up as a wall of empty lines in the chat client. Leading
 * blank lines are never meaningful, but real leading indentation on the
 * first content line is preserved (the regex needs a newline to match).
 */
export const trimLeadingBlankLines = (text: string): string =>
  text.replace(/^(?:[^\S\n]*\n)+[^\S\n]*/, '');

const isAssistantMessage = (event: ReplyEventView): event is AssistantMessageEventView =>
  event.type === 'assistant/message';

const isTurnEnd = (event: ReplyEventView): event is TurnEndEventView => event.type === 'turn/end';

/**
 * Pure fold over committed events. Unrecognized events return the SAME state
 * reference (zero downstream work per the projection contract).
 */
export function collectReplyState(state: ReplyState, event: ReplyEventView): ReplyState {
  if (isAssistantMessage(event)) {
    // Interrupted assistant messages are a cancelled prefix — never send them.
    if (event.data.message.interrupted) return state;
    const text = extractAssistantText(event.data.message.content);
    if (!text) return state;
    const key = String(event.data.turn);
    const buffered = state.turns[key] ?? [];
    return {
      ...state,
      turns: {
        ...state.turns,
        [key]: [...buffered, { seq: event.seq, text }],
      },
    };
  }

  if (isTurnEnd(event)) {
    const key = String(event.data.turn);
    const buffered = state.turns[key];
    // Discard the buffer for every turn, completed or not (no leaks on abort).
    const turns = { ...state.turns };
    delete turns[key];
    if (event.data.reason.kind !== 'completed' || !buffered || buffered.length === 0) {
      return { ...state, turns };
    }
    // One WeChat reply per completed turn: concatenate its text blocks in order.
    const reply: ReadyReply = {
      seq: buffered[buffered.length - 1]!.seq,
      turn: event.data.turn,
      text: trimLeadingBlankLines(buffered.map((b) => b.text).join('')),
    };
    return {
      turns,
      ready: [...state.ready, reply].slice(-MAX_READY_REPLIES),
    };
  }

  return state;
}

// ---------------------------------------------------------------------------
// Delivery planning
// ---------------------------------------------------------------------------

export interface DeliveryPlan {
  readonly key: string;
  readonly seq: number;
  readonly turn: number;
  readonly text: string;
}

/** Stable idempotency key for one completed-turn reply. */
export const replyOutboundKey = (sessionId: string, seq: number, turn: number): string =>
  `reply:${sessionId}:${seq}:${turn}`;

/**
 * Pure planner: given the projection state and the set of keys already
 * enqueued, return only replies that still need delivery. Restart-safe because
 * the enqueued-key set is derived from the durable outbox.
 */
export function planDeliveries(
  sessionId: string,
  state: ReplyState,
  enqueuedKeys: ReadonlySet<string>,
): DeliveryPlan[] {
  const plans: DeliveryPlan[] = [];
  for (const reply of state.ready) {
    const key = replyOutboundKey(sessionId, reply.seq, reply.turn);
    if (!enqueuedKeys.has(key) && reply.text) {
      plans.push({ key, seq: reply.seq, turn: reply.turn, text: reply.text });
    }
  }
  return plans;
}

// ---------------------------------------------------------------------------
// Projection definition (structural mirror of DSH ProjectionDefinition)
// ---------------------------------------------------------------------------

export interface ReplyProjectionDefinition {
  readonly key: string;
  readonly stateSchema: z.ZodType<ReplyState>;
  readonly stateVersion: number;
  init(): ReplyState;
  apply(state: ReplyState, event: ReplyEventView): ReplyState;
}

/** The host-only projection unit registered via `ctx.sessionProjections.register`. */
export const replyProjection: ReplyProjectionDefinition = {
  key: 'agent_wechat_replies',
  stateSchema: ReplyStateSchema,
  stateVersion: 1,
  init: initReplyState,
  apply: collectReplyState,
};
