import { useRef, type ReactNode } from "react";
import { MessageScroller } from "@shadcn/react/message-scroller";
import { Icon } from "./ui.tsx";
import { ScrollToQuotedMessage } from "./conversation-navigation.tsx";

export const ConversationItem = MessageScroller.Item;

export function ConversationScroll({
  children,
  searching,
  history,
  leading,
}: {
  children: ReactNode;
  searching: boolean;
  history: { hasMore: boolean; loading: boolean; load: () => void };
  leading?: ReactNode;
}) {
  const showedHistory = useRef(false);
  showedHistory.current ||= history.hasMore;
  if (searching)
    return (
      <div className="messages" role="region" aria-label="Search results">
        {children}
      </div>
    );
  return (
    <MessageScroller.Provider autoScroll defaultScrollPosition="end">
      <ScrollToQuotedMessage />
      <MessageScroller.Root className="message-scroller">
        <MessageScroller.Viewport className="messages">
          {showedHistory.current ? (
            <button
              className="load-history"
              disabled={history.loading || !history.hasMore}
              onClick={history.load}
            >
              {!history.hasMore
                ? "Beginning of conversation"
                : history.loading
                  ? "Loading older messages…"
                  : "Load older messages"}
            </button>
          ) : history.loading ? (
            <p className="date-label">Loading messages…</p>
          ) : null}
          {/* A prepended page must not remove a date prefix from an existing message row. */}
          {leading}
          <MessageScroller.Content>{children}</MessageScroller.Content>
        </MessageScroller.Viewport>
        <MessageScroller.Button
          className="jump-to-latest"
          aria-label="Jump to latest message"
        >
          <Icon name="down" /> Latest messages
        </MessageScroller.Button>
      </MessageScroller.Root>
    </MessageScroller.Provider>
  );
}
