import { useMemo, useSyncExternalStore } from "react";
import { formatRoute, parseRoute, type Route } from "./routes.ts";

type Environment = Pick<Window, "location" | "history" | "addEventListener" | "removeEventListener">;
type Entry = { from?: string } | null;

/**
 * The address bar as a small store. `push` adds a history entry, `replace`
 * rewrites the current one, and `up` and `back` leave a view the way Back would. Each
 * entry remembers the hash it was opened from, so leaving a view can use real
 * Back when the previous entry is the wanted view instead of stacking duplicates.
 */
export function createRouter(win: Environment) {
  const listeners = new Set<() => void>();
  const notify = () => {
    for (const listener of [...listeners]) listener();
  };
  const hash = () => win.location.hash;
  const from = () => (win.history.state as Entry)?.from;
  function subscribe(listener: () => void): () => void {
    if (!listeners.size) {
      win.addEventListener("hashchange", notify);
      win.addEventListener("popstate", notify);
    }
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
      if (!listeners.size) {
        win.removeEventListener("hashchange", notify);
        win.removeEventListener("popstate", notify);
      }
    };
  }
  function push(route: Route): void {
    const next = formatRoute(route);
    if (next === hash()) return;
    win.history.pushState({ from: hash() } satisfies Entry, "", next);
    notify();
  }
  function replace(route: Route): void {
    const next = formatRoute(route);
    if (next === hash()) return;
    win.history.replaceState(win.history.state, "", next);
    notify();
  }
  /** Leaves a view for `route`: real Back when the previous entry is that route, else a replacement. */
  function up(route: Route): void {
    const target = formatRoute(route);
    if (target === hash()) return;
    if (from() === target) win.history.back();
    else replace(route);
  }
  /** Goes back to where this entry was opened from, or to `fallback` for an entry opened directly. */
  function back(fallback: Route): void {
    if (from() !== undefined) win.history.back();
    else replace(fallback);
  }
  return { subscribe, hash, push, replace, up, back };
}

export type Router = ReturnType<typeof createRouter>;

/** The address bar of this page. */
export const router: Router = createRouter(globalThis as unknown as Environment);

/** The current address hash. */
export function useHash(): string {
  return useSyncExternalStore(router.subscribe, router.hash);
}

/** The current route, or `null` when the hash is empty or not a known route. */
export function useRoute(): Route | null {
  const hash = useHash();
  return useMemo(() => parseRoute(hash), [hash]);
}
