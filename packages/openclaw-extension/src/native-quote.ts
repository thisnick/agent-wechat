import type { NativeMessage } from "@agent-wechat/shared";

export function renderEmbeddedQuote(message: NativeMessage, permitted = true, imageBody?: string) {
  const comment = (message.body ?? "").slice(0, 16384);
  const source = message.quote;
  const unavailable = (state: string) => ({ body: `${comment}${comment ? "\n\n" : ""}[Quoted message ${state}]`, image: false,
    quote: { state, type: source?.type, senderId: source?.senderId ?? undefined } });
  if (!source) return unavailable("unavailable");
  if (!permitted) return unavailable("not permitted");
  const state = source.state ?? "missing";
  if (state === "recalled") return unavailable("recalled");
  if (!["embedded", "missing", "unsupported"].includes(state)
    || (source.embeddedText != null && (typeof source.embeddedText !== "string" || source.embeddedText.length > 16384))
    || (source.embeddedImage != null && typeof source.embeddedImage !== "boolean")) return unavailable("invalid");
  const label = source.type === 1 ? "text" : source.type === 3 ? "image" : source.type === 34 ? "voice" : source.type === 43 ? "video"
    : source.type === 49 && source.appMessageType === 5 ? "link" : source.type === 49 && source.appMessageType === 6 ? "file" : "message";
  const image = state === "embedded" && source.type === 3 && source.embeddedImage === true;
  if (state === "embedded" && !image && !source.embeddedText) return unavailable("invalid");
  const content = state === "missing" ? "" : image ? imageBody ?? "[Image metadata embedded in quote; image not retrieved]" : source.embeddedText ?? "";
  const status = state === "unsupported" ? `[${label} attachment unavailable from embedded quote]` : state === "missing" ? "[Embedded quote content unavailable]" : "";
  return { body: `${comment}${comment ? "\n\n" : ""}[Quoted ${label} from ${source.senderId ?? "unknown sender"}; embedded in reply]\n${[content, status].filter(Boolean).join("\n")}\n[/Quoted message]`,
    image, quote: { state, type: source.type, senderId: source.senderId ?? undefined } };
}
