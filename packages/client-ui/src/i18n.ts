/**
 * UI translations. English (`locales/en.json`) is the source and the fallback:
 * a key missing or empty in another language shows the English text.
 *
 * To add a language, add `locales/<tag>.json` and one entry to `languages`.
 */
import i18next, { type ParseKeys } from "i18next";
import { createElement, Fragment, useSyncExternalStore, type ReactNode } from "react";
import en from "../locales/en.json" with { type: "json" };
import zhCN from "../locales/zh-CN.json" with { type: "json" };

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: { translation: typeof en };
  }
}

/** Shipped languages, each named in its own language. */
export const languages = [
  { tag: "en", name: "English" },
  { tag: "zh-CN", name: "简体中文" },
] as const;
export type Language = (typeof languages)[number]["tag"];
/** `system` follows the operating system or browser language. */
export type LanguagePreference = "system" | Language;

const STORAGE_KEY = "agentwechat-lang";
const resources = { en: { translation: en }, "zh-CN": { translation: zhCN } };

/** Pick the shipped language that matches a BCP 47 tag, if any. */
export function matchLanguage(tag: string | undefined | null): Language | null {
  if (!tag) return null;
  const lower = tag.toLowerCase();
  const exact = languages.find((language) => language.tag.toLowerCase() === lower);
  if (exact) return exact.tag;
  // Simplified Chinese only: Traditional variants (zh-TW, zh-HK, zh-Hant) do not match.
  if (lower === "zh" || lower.startsWith("zh-hans") || lower === "zh-sg") return "zh-CN";
  const base = lower.split("-")[0];
  return languages.find((language) => language.tag.toLowerCase() === base)?.tag ?? null;
}

function systemLanguage(): Language {
  const tags = globalThis.navigator?.languages ?? [globalThis.navigator?.language];
  for (const tag of tags) {
    const match = matchLanguage(tag);
    if (match) return match;
  }
  return "en";
}

function parsePreference(value: string | null | undefined): LanguagePreference | null {
  if (value === "system" || languages.some((language) => language.tag === value)) return value as LanguagePreference;
  return null;
}

function storedPreference(): LanguagePreference {
  try {
    const value = parsePreference(globalThis.localStorage?.getItem(STORAGE_KEY));
    if (value) return value;
  } catch {
    // Storage can be unavailable; fall back to the system language.
  }
  return "system";
}

let preference = storedPreference();
const resolve = (value: LanguagePreference): Language => (value === "system" ? systemLanguage() : value);

void i18next.init({
  resources,
  lng: resolve(preference),
  fallbackLng: "en",
  initAsync: false,
  returnEmptyString: false,
  interpolation: { escapeValue: false },
});

/** Translate a key from `locales/en.json`. */
export const t = i18next.t.bind(i18next);

/**
 * A whole translated sentence with elements in it: `{{name}}` placeholders in the
 * text are replaced by `parts.name`, so translators see one sentence and can move
 * the element, for example `richText(key, { name: <strong>{id}</strong> })`.
 */
export function richText(key: ParseKeys, parts: Record<string, ReactNode>): ReactNode[] {
  const marks = Object.fromEntries(Object.keys(parts).map((name) => [name, `\u0000${name}\u0000`]));
  return (t as (key: string, options: object) => string)(key, marks)
    .split("\u0000")
    .map((piece, index) => (index % 2 ? createElement(Fragment, { key: index }, parts[piece]) : piece));
}

/** The language the UI is shown in. */
export function currentLanguage(): Language {
  return i18next.language as Language;
}

/** The saved choice, which may be `system`. */
export function languagePreference(): LanguagePreference {
  return preference;
}

/**
 * Locale for dates and numbers: the browser default while following the system,
 * otherwise the chosen language.
 */
export function formatLocale(): string | string[] {
  return preference === "system" ? [] : currentLanguage();
}

function applyDocument(language: Language) {
  if (typeof document !== "undefined") document.documentElement.lang = language;
}

/** Tell the desktop app which language to use for its tray and notifications. */
function syncDesktop(language: Language) {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  void import("@tauri-apps/api/core")
    .then(({ invoke }) => invoke("desktop_language", { language }))
    .catch(() => {});
}

export function setLanguagePreference(next: LanguagePreference): void {
  try {
    globalThis.localStorage?.setItem(STORAGE_KEY, next);
  } catch {
    // Not saved; the choice still applies until reload.
  }
  apply(next);
}

function apply(next: LanguagePreference) {
  preference = next;
  const language = resolve(next);
  applyDocument(language);
  syncDesktop(language);
  if (language !== currentLanguage()) void i18next.changeLanguage(language);
  else listeners.forEach((listener) => listener());
}

const listeners = new Set<() => void>();
i18next.on("languageChanged", () => listeners.forEach((listener) => listener()));
/** Call `listener` whenever the UI language changes; returns an unsubscribe function. */
export function subscribeLanguage(listener: () => void) {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}
const snapshot = () => `${preference}:${currentLanguage()}`;

/**
 * Re-render when the language changes. Call it in the app root and in any
 * component that renders outside it.
 */
export function useLanguage(): [LanguagePreference, (next: LanguagePreference) => void] {
  useSyncExternalStore(subscribeLanguage, snapshot, snapshot);
  return [preference, setLanguagePreference];
}

// A change made in another window or tab applies here too.
globalThis.addEventListener?.("storage", (event: StorageEvent) => {
  if (event.key !== STORAGE_KEY) return;
  apply(parsePreference(event.newValue) ?? "system");
});

applyDocument(currentLanguage());
syncDesktop(currentLanguage());
