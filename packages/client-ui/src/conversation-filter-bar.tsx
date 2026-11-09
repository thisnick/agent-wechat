import { useEffect, useRef, useState } from "react";
import { Icon } from "./ui.tsx";
import {
  conversationCategories,
  type ConversationCategory,
} from "./conversation-filters.ts";

export function UnreadCount({ count }: { count: number }) {
  return (
    <>
      <span className="sr-only">{count} unread</span>
      <span className="filter-unread" aria-hidden="true">{count > 99 ? "99+" : count}</span>
    </>
  );
}

export function ConversationFilterBar({
  selected,
  counts,
  onToggle,
}: {
  selected: ConversationCategory[];
  counts: Record<ConversationCategory, number>;
  onToggle: (category: ConversationCategory) => void;
}) {
  const bar = useRef<HTMLDivElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const chips = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState(false);
  const [edges, setEdges] = useState({ start: true, end: true });

  function updateEdges() {
    const list = scroller.current;
    if (!list) return;
    const start = list.scrollLeft < 1;
    const end = list.scrollWidth - list.clientWidth - list.scrollLeft < 1;
    setEdges((current) => current.start === start && current.end === end ? current : { start, end });
  }

  useEffect(() => {
    const container = bar.current!;
    const row = chips.current!;
    const observer = new ResizeObserver(() => {
      // Measure the row against the full bar so the buttons cannot cause overflow.
      setOverflow(row.offsetWidth > container.clientWidth);
      updateEdges();
    });
    observer.observe(container);
    observer.observe(row);
    observer.observe(scroller.current!);
    return () => observer.disconnect();
  }, []);

  function scroll(direction: -1 | 1) {
    const list = scroller.current!;
    list.scrollBy({
      left: direction * list.clientWidth,
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    });
  }

  return (
    <div className="conversation-filters" role="group" aria-label="Conversation categories" ref={bar}>
      {overflow && (
        <button
          className="filter-scroll previous"
          aria-label="Scroll categories left"
          disabled={edges.start}
          onClick={() => scroll(-1)}
        >
          <Icon name="chevron" />
        </button>
      )}
      <div className="filter-scroller" ref={scroller} onScroll={updateEdges}>
        <div className="filter-chips" ref={chips}>
          {conversationCategories.map(({ id, label }) => (
            <button
              key={id}
              className="filter-chip"
              aria-pressed={selected.includes(id)}
              onClick={() => onToggle(id)}
            >
              {label}<UnreadCount count={counts[id]} />
            </button>
          ))}
        </div>
      </div>
      {overflow && (
        <button
          className="filter-scroll"
          aria-label="Scroll categories right"
          disabled={edges.end}
          onClick={() => scroll(1)}
        >
          <Icon name="chevron" />
        </button>
      )}
    </div>
  );
}
