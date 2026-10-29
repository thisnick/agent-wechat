import aliases from "./emoji-aliases.json" with { type: "json" };

// Unicode approximations for display only. Never feed this text into a send,
// copy, search query, message receipt or cache: those retain the source text.
const escaped = Object.keys(aliases)
  .sort((a, b) => b.length - a.length)
  .map((alias) => alias.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
const tokens = new RegExp(
  `(?:[hH][tT][tT][pP][sS]?:\\/\\/|[wW][wW][wW]\\.)[^\\s<>]+|${escaped.join("|")}`,
  "g",
);
const catalog: Readonly<Record<string, string>> = aliases;

export function emojiText(text: string): string {
  return text.replace(tokens, (token) => catalog[token] ?? token);
}
