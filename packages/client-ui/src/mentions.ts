import type { TextMentions } from "@agent-wechat/shared";

export type MentionToken = {
  userId: string | null;
  label: string;
  start: number;
  end: number;
};
export const MENTION_SEPARATOR = "\u2005";
const identity = (value: unknown): value is string =>
  typeof value === "string" && /^[A-Za-z0-9_@.-]{1,256}$/.test(value) && value !== "notify@all";

/** Offsets belong to the draft's UTF-16 text, never to DOM text containing controls. */
export function validMentionTokens(text: string, value: unknown): value is MentionToken[] {
  if (!Array.isArray(value) || value.length > 256) return false;
  let end = 0;
  for (const token of value) {
    if (!token || typeof token !== "object" || (!identity(token.userId) && token.userId !== null)
      || typeof token.label !== "string" || !token.label.trim() || new TextEncoder().encode(token.label).length > 1024
      || /[\u0000-\u001f\u007f-\u009f\u2005\uD800-\uDFFF]/u.test(token.label)
      || !Number.isInteger(token.start) || !Number.isInteger(token.end)
      || token.start < end || token.end > text.length
      || text.slice(token.start, token.end) !== `@${token.label}${MENTION_SEPARATOR}`) return false;
    end = token.end;
  }
  return !(value.some(token => token.userId === null) && value.some(token => token.userId !== null))
    && new Set(value.filter(token => token.userId !== null).map(token => token.userId)).size <= 128;
}

export function mentionOptions(text: string, tokens: MentionToken[]): TextMentions | undefined {
  if (!validMentionTokens(text, tokens)) throw new Error("Choose the mentioned members again.");
  if (!tokens.length) return undefined;
  if (new TextEncoder().encode(text).length > 65536 || text.includes("\0")) throw new Error("Message is too long or contains unsupported characters.");
  const ids = [...new Set(tokens.flatMap(token => token.userId === null ? [] : [token.userId]))];
  return {
    ...(ids.length ? { userIds: ids } : { all: true }),
    positions: tokens.map(token => ({ start: token.start, end: token.end, index: token.userId === null ? 0 : ids.indexOf(token.userId) })),
  };
}

export function validMentionReceipt(text: string, value: unknown): value is TextMentions {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const input = value as TextMentions;
  const ids = input.userIds ?? [];
  if (Object.keys(input).some(key => !["userIds", "all", "positions"].includes(key))
    || !Array.isArray(ids) || ids.length > 128 || ids.some(id => !identity(id))
    || new Set(ids).size !== ids.length || (input.all === true) === (ids.length > 0)
    || (input.all !== undefined && typeof input.all !== "boolean")
    || !Array.isArray(input.positions) || !input.positions.length || input.positions.length > 256
    || !input.positions.every(position => position && typeof position === "object" && Number.isInteger(position.index)
      && !Object.keys(position).some(key => !["start", "end", "index"].includes(key))
      && Number.isInteger(position.start) && Number.isInteger(position.end)
      && position.index >= 0 && position.index < (input.all ? 1 : ids.length))) return false;
  const tokens = input.positions.map(position => ({
    userId: input.all ? null : ids[position.index],
    label: text.slice(position.start + 1, position.end - 1),
    start: position.start,
    end: position.end,
  }));
  return input.positions.every(position => position && Number.isInteger(position.index)
    && position.index >= 0 && position.index < (input.all ? 1 : ids.length))
    && validMentionTokens(text, tokens)
    && (input.all || ids.every(id => tokens.some(token => token.userId === id)));
}

export function mentionQuery(text: string, caret: number, tokens: MentionToken[]) {
  const match = text.slice(0, caret).match(/(?:^|\s)@([^@\n\u2005]{0,80})$/u);
  if (!match) return null;
  const start = caret - match[1].length - 1;
  return tokens.some(token => start >= token.start && start < token.end) ? null : { start, end: caret, query: match[1] };
}
