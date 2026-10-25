import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  MessageScroller,
  useMessageScroller,
  useMessageScrollerVisibility,
  useMessageScrollerScrollable,
} from "@shadcn/react/message-scroller";
import type { BrowserCache } from "./browser-cache.ts";
import { Icon } from "./ui.tsx";
import { ScrollToQuotedMessage } from "./conversation-navigation.tsx";

export const ConversationItem = MessageScroller.Item;

export function ConversationScroll({
  children,
  searching,
  history,
  leading,
  position,
}: {
  children: ReactNode;
  searching: boolean;
  history: { hasMore: boolean; loading: boolean; load: () => void };
  leading?: ReactNode;
  position?: { cache: BrowserCache; account: string; thread: string };
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
      {position && (
        <CachedScroll position={position} loading={history.loading} />
      )}
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

function CachedScroll({
  position,
  loading,
}: {
  position: { cache: BrowserCache; account: string; thread: string };
  loading: boolean;
}) {
  const { cache, account, thread } = position;
  const key = `scroll:${JSON.stringify([account, thread])}`;
  const { scrollToMessage } = useMessageScroller();
  const { currentAnchorId } = useMessageScrollerVisibility();
  const { end } = useMessageScrollerScrollable();
  const [ready, setReady] = useState(false);
  const restoring = useRef(false);
  const latest = useRef<{ anchor: string | null; atEnd: boolean } | undefined>(
    undefined,
  );
  useEffect(() => {
    if (loading || ready || restoring.current) return;
    restoring.current = true;
    const saved = cache.preference<{ anchor?: string; atEnd?: boolean }>(key);
    let second = 0;
    const frame = requestAnimationFrame(() => {
      // Only restore anchors in the retained pages; never fetch history to restore a position.
      if (saved?.atEnd === false && typeof saved.anchor === "string")
        scrollToMessage(saved.anchor, { align: "start", behavior: "instant" });
      second = requestAnimationFrame(() => setReady(true));
    });
    return () => {
      cancelAnimationFrame(frame);
      cancelAnimationFrame(second);
      restoring.current = false;
    };
  }, [loading, ready, cache, key, scrollToMessage]);
  useEffect(() => {
    if (!ready || !currentAnchorId) return;
    latest.current = { anchor: currentAnchorId, atEnd: !end };
    const timer = setTimeout(
      () => cache.setPreference(key, latest.current),
      150,
    );
    return () => clearTimeout(timer);
  }, [ready, currentAnchorId, end, cache, key]);
  useEffect(
    () => () => {
      if (latest.current) cache.setPreference(key, latest.current);
    },
    [cache, key],
  );
  return null;
}
