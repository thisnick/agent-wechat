import { useState, useSyncExternalStore } from "react";
import type { NativeMessage } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { embeddedQuotePreview, localQuote } from "./quote-preview.ts";
import { MediaCard } from "./media.tsx";
import { SharedLink } from "./shared-link.tsx";
import { useConversationNavigation } from "./conversation-navigation.tsx";

export function QuotedReply({
  message,
  connection,
  name,
}: {
  message: NativeMessage;
  connection: Connection;
  name: (id: string) => string;
}) {
  const state = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  );
  const local = localQuote(message, state.messages);
  const quote = embeddedQuotePreview(message, local);
  const navigation = useConversationNavigation();
  const [jumping, setJumping] = useState(false);
  const [jumpFailed, setJumpFailed] = useState(false);
  const canJump = !!message.quote && !!navigation;
  async function jump() {
    if (!navigation || jumping) return;
    setJumping(true);
    setJumpFailed(false);
    const found = await navigation.jumpQuote(message);
    setJumping(false);
    setJumpFailed(!found);
  }
  return (
    <blockquote
      className="quote-preview"
      aria-label="Quoted message"
      role={canJump ? "button" : undefined}
      tabIndex={canJump ? 0 : undefined}
      aria-busy={jumping || undefined}
      title={canJump ? "Go to quoted message" : undefined}
      onClick={canJump ? () => void jump() : undefined}
      onKeyDown={(event) => {
        if (canJump && ["Enter", " "].includes(event.key)) {
          event.preventDefault();
          void jump();
        }
      }}
    >
      <strong>
        {quote.own
          ? "You"
          : quote.senderId
          ? name(quote.senderId)
          : "Quoted message"}
      </strong>
      {quote.content?.link ? (
        <SharedLink message={quote.content} connection={connection} compact />
      ) : quote.content ? (
        <MediaCard
          message={quote.content}
          connection={connection}
          previewOnly
        />
      ) : (
        <p>{quote.text}</p>
      )}
      {(jumping || jumpFailed) && (
        <small role="status">
          {jumping
            ? "Finding message…"
            : "Message isn’t in this chat’s history."}
        </small>
      )}
    </blockquote>
  );
}
