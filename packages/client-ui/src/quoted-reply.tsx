import { useEffect, useState, useSyncExternalStore } from "react";
import type {
  NativeMessage,
  NativeQuoteResolution,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { localQuote, quotedText } from "./quote-preview.ts";
import { useVisible } from "./visible.tsx";
import { MediaCard } from "./media.tsx";

export function QuotedReply({
  message,
  connection,
  name,
}: {
  message: NativeMessage;
  connection: Connection;
  name: (id: string) => string;
}) {
  const { element, visible } = useVisible<HTMLQuoteElement>();
  const state = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  );
  const local = localQuote(message, state.messages);
  const [quote, setQuote] = useState<NativeQuoteResolution | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!visible || local) return;
    let cancelled = false;
    void connection.session.client
      .quotedMessage(message.accountId, message.id)
      .then((result) => {
        if (result.threadId !== message.threadId)
          throw new Error("Invalid quoted conversation");
        if (!cancelled) setQuote(result);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [
    connection.session,
    message.accountId,
    message.threadId,
    message.id,
    message.version,
    visible,
    local?.id,
  ]);
  const original = local ?? quote?.message;
  const recalled = quote?.state === "recalled" || !!original?.recall;
  const sender = original?.senderId ?? quote?.senderId;
  const own = original?.direction === "outgoing";
  const image = !recalled && (original?.type === 3 || quote?.embeddedImage);
  return (
    <blockquote
      className="quote-preview"
      ref={element}
      aria-label="Quoted message"
    >
      <strong>{own ? "You" : sender ? name(sender) : "Quoted message"}</strong>
      {image ? (
        <MediaCard
          message={original ?? message}
          connection={connection}
          previewOnly
        />
      ) : (
        <p>
          {recalled
            ? "Message recalled"
            : original
              ? quotedText(original)
              : quote?.embeddedText ||
                (failed
                  ? "Couldn’t load the quoted message."
                  : quote
                    ? "The quoted message is unavailable."
                    : "Loading quoted message…")}
        </p>
      )}
    </blockquote>
  );
}
