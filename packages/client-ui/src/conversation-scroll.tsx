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
import {
  HistoryPaging,
  HISTORY_SCROLL_EDGE,
  type HistoryPage,
} from "./history-paging.ts";

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
  history: HistoryPage;
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
      <ConversationViewport
        history={history}
        showedHistory={showedHistory.current}
        leading={leading}
      >
        {children}
      </ConversationViewport>
    </MessageScroller.Provider>
  );
}

function ConversationViewport({
  children,
  history,
  showedHistory,
  leading,
}: {
  children: ReactNode;
  history: HistoryPage;
  showedHistory: boolean;
  leading?: ReactNode;
}) {
  const { scrollToEnd } = useMessageScroller();
  const [paging] = useState(() => new HistoryPaging());
  const upward = useRef(false);
  const previousTop = useRef<number | null>(null);
  const touchY = useRef<number | null>(null);
  // HubSession reports page errors. Automatic paging waits for another visit
  // to the edge after an error; the button always permits an intentional retry.
  const load = (automatic: boolean) =>
    void paging.request(history, automatic).catch(() => {});
  function olderNearStart(element: HTMLDivElement) {
    if (element.scrollTop <= HISTORY_SCROLL_EDGE) load(true);
  }
  function followIfAtEnd(element: HTMLDivElement) {
    if (element.scrollHeight - element.scrollTop - element.clientHeight <= 8)
      scrollToEnd({ behavior: "instant" });
  }
  return (
    <MessageScroller.Root className="message-scroller">
      <MessageScroller.Viewport
        className="messages"
        onScroll={(event) => {
          const element = event.currentTarget,
            top = element.scrollTop;
          paging.observe(top);
          if (
            upward.current &&
            previousTop.current !== null &&
            top < previousTop.current
          )
            olderNearStart(element);
          previousTop.current = top;
        }}
        onWheel={(event) => {
          if (event.defaultPrevented || event.ctrlKey) return;
          upward.current = event.deltaY < 0;
          if (upward.current) olderNearStart(event.currentTarget);
          // The library releases following on every wheel event, including
          // a downward wheel at the bottom that produces no scroll event.
          else followIfAtEnd(event.currentTarget);
        }}
        onPointerDown={(event) => {
          // Scrollbar dragging counts as user scrolling; clicking a quote
          // or restoring a cached position must not fetch older pages.
          upward.current = event.target === event.currentTarget;
          previousTop.current = event.currentTarget.scrollTop;
        }}
        onKeyDown={(event) => {
          if (
            event.defaultPrevented ||
            event.ctrlKey ||
            event.metaKey ||
            event.altKey ||
            (event.key === " " && event.target !== event.currentTarget)
          )
            return;
          upward.current =
            ["ArrowUp", "PageUp", "Home"].includes(event.key) ||
            (event.key === " " && event.shiftKey);
          if (upward.current) olderNearStart(event.currentTarget);
          else if (["ArrowDown", "PageDown", "End", " "].includes(event.key))
            followIfAtEnd(event.currentTarget);
        }}
        onTouchStart={(event) => {
          upward.current = false;
          touchY.current =
            event.touches.length === 1 ? event.touches[0].clientY : null;
        }}
        onTouchMove={(event) => {
          const next =
            event.touches.length === 1 ? event.touches[0].clientY : null;
          upward.current =
            next !== null && touchY.current !== null && next > touchY.current;
          touchY.current = next;
          if (upward.current) olderNearStart(event.currentTarget);
        }}
        onTouchEnd={(event) => {
          touchY.current = null;
          followIfAtEnd(event.currentTarget);
        }}
      >
        {showedHistory ? (
          <button
            className="load-history"
            disabled={history.loading || !history.hasMore}
            onClick={() => load(false)}
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
