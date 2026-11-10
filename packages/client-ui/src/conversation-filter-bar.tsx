import { useEffect, useRef, useState } from "react";
import { t } from "./i18n.ts";
import { Icon } from "./ui.tsx";
import {
  categoryLabel,
  conversationCategories,
  type ConversationCategory,
} from "./conversation-filters.ts";

function UnreadCount({ count }: { count: number }) {
  return (
    <>
      <span className="sr-only">{t("conversationFilterBar.unread", { count })}</span>
      {count > 0 && (
        <span className="filter-unread" aria-hidden="true">{count > 99 ? "99+" : count}</span>
      )}
    </>
  );
}

export function ConversationFilterBar({
  selected,
  counts,
  onChange,
}: {
  selected: ConversationCategory[];
  counts: Record<ConversationCategory | "total", number>;
  onChange: (categories: ConversationCategory[]) => void;
}) {
  const track = useRef<HTMLDivElement>(null);
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
    const container = track.current!;
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
    <div className="conversation-filters" role="group" aria-label={t("conversationFilterBar.conversationCategories")}>
      <button
        type="button"
        className="filter-chip"
        aria-pressed={selected.length === 0}
        onClick={() => onChange([])}
      >
        {t("conversationFilterBar.all")}<span className="sr-only"> {t("conversationFilterBar.conversations")}</span>
        <UnreadCount count={counts.total} />
      </button>
      <div className="filter-track" ref={track}>
        {overflow && (
          <button
            type="button"
            className="filter-scroll previous"
            aria-label={t("conversationFilterBar.scrollCategoriesLeft")}
            disabled={edges.start}
            onClick={() => scroll(-1)}
          >
            <Icon name="chevron" />
          </button>
        )}
        <div
          className="filter-scroller"
          data-can-scroll-left={!edges.start}
          data-can-scroll-right={!edges.end}
          ref={scroller}
          onScroll={updateEdges}
        >
          <div className="filter-chips" ref={chips}>
            {conversationCategories.map(({ id }) => {
              const pressed = selected.includes(id);
              return (
                <button
                  type="button"
                  key={id}
                  className="filter-chip"
                  aria-pressed={pressed}
                  onClick={() => onChange(pressed ? selected.filter((item) => item !== id) : [...selected, id])}
                >
                  {categoryLabel(id)}<UnreadCount count={counts[id]} />
                </button>
              );
            })}
          </div>
        </div>
        {overflow && (
          <button
            type="button"
            className="filter-scroll"
            aria-label={t("conversationFilterBar.scrollCategoriesRight")}
            disabled={edges.end}
            onClick={() => scroll(1)}
          >
            <Icon name="chevron" />
          </button>
        )}
      </div>
    </div>
  );
}
