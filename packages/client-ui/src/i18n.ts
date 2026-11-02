import { useSyncExternalStore } from "react";

export type Locale = "en" | "zh-CN";

const STORAGE_KEY = "agentwechat-locale";
const listeners = new Set<() => void>();

function normalizeLocale(value: unknown): Locale | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "zh-cn" || trimmed === "zh" || trimmed.startsWith("zh-hans"))
    return "zh-CN";
  if (trimmed === "en" || trimmed.startsWith("en-")) return "en";
  return null;
}

function readStoredLocale(): Locale {
  try {
    return normalizeLocale(globalThis.localStorage?.getItem(STORAGE_KEY)) ?? "en";
  } catch {
    return "en";
  }
}

let currentLocale: Locale = readStoredLocale();

function applyDocumentLang(locale: Locale): void {
  if (typeof document !== "undefined" && document.documentElement) {
    document.documentElement.lang = locale;
  }
}

applyDocumentLang(currentLocale);

export function getLocale(): Locale {
  return currentLocale;
}

export function isZh(): boolean {
  return currentLocale === "zh-CN";
}

export function dateLocale(): string | string[] {
  return currentLocale === "zh-CN" ? "zh-CN" : [];
}

export function setLocale(
  next: Locale | string,
  options?: { syncDesktop?: boolean },
): void {
  const resolved = normalizeLocale(next) ?? "en";
  const changed = resolved !== currentLocale;
  currentLocale = resolved;
  try {
    globalThis.localStorage?.setItem(STORAGE_KEY, resolved);
  } catch {
    // Ignore restricted storage environments.
  }
  applyDocumentLang(resolved);
  if (
    options?.syncDesktop !== false &&
    typeof window !== "undefined" &&
    "__TAURI_INTERNALS__" in window
  ) {
    import("@tauri-apps/api/core")
      .then(({ invoke }) => invoke("desktop_language", { language: resolved }))
      .catch(() => undefined);
  }
  if (changed) {
    for (const listener of listeners) listener();
  }
}

export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useLocale(): [Locale, (next: Locale) => void] {
  const locale = useSyncExternalStore(subscribeLocale, getLocale, getLocale);
  return [locale, (next: Locale) => setLocale(next)];
}

export function t(en: string, zh: string): string {
  return currentLocale === "zh-CN" ? zh : en;
}

export function syncDesktopLocale(language?: string | null): void {
  let stored: string | null = null;
  try {
    stored = normalizeLocale(globalThis.localStorage?.getItem(STORAGE_KEY));
  } catch {
    stored = null;
  }
  const desktopLocale = normalizeLocale(language);
  if (!stored && desktopLocale) {
    setLocale(desktopLocale, { syncDesktop: false });
  } else if (stored && desktopLocale !== stored) {
    if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      import("@tauri-apps/api/core")
        .then(({ invoke }) => invoke("desktop_language", { language: stored }))
        .catch(() => undefined);
    }
  }
}

