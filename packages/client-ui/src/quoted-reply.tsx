import { useState, useSyncExternalStore } from "react";
import type { NativeMessage } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
import { embeddedQuotePreview, localQuote } from "./quote-preview.ts";
import { MediaCard } from "./media.tsx";
import { SharedLink } from "./shared-link.tsx";
import { useConversationNavigation } from "./conversation-navigation.tsx";
import { emojiText } from "./emoji-text.ts";

export function QuotedReply({
  message,
  connection,
  name,
}: {
  message: NativeMessage;
  connection: Connection;
  name: (id: string) => string;
}) {
  useLocale();
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
      aria-label={t("Quoted message", "引用消息")}
      role={canJump ? "button" : undefined}
      tabIndex={canJump ? 0 : undefined}
      aria-busy={jumping || undefined}
      title={canJump ? t("Go to quoted message", "跳转到引用消息") : undefined}
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
          ? t("You", "我")
          : quote.senderId
          ? name(quote.senderId)
          : t("Quoted message", "引用消息")}
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
        <p>{emojiText(quote.text)}</p>
      )}
      {(jumping || jumpFailed) && (
        <small role="status">
          {jumping
            ? t("Finding message…", "正在查找消息…")
            : t(
                "Message isn’t in this chat’s history.",
                "该消息不在此会话的聊天记录中。",
              )}
        </small>
      )}
    </blockquote>
  );
}
