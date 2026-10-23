import { useEffect, useRef, useState } from "react";

/** Asset reads start near the viewport, rather than for the entire history page. */
export function useVisible<T extends HTMLElement>() {
  const element = useRef<T>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (visible || !element.current) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin: "100px" },
    );
    observer.observe(element.current);
    return () => observer.disconnect();
  }, [visible]);
  return { element, visible };
}
