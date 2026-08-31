// Local type shim: openclaw 2026.8.x ships plugin-sdk/channel-targets.js without
// a .d.ts (packaging regression upstream). Declare just what we import.
declare module "openclaw/plugin-sdk/channel-targets" {
  export function buildChannelKeyCandidates(
    chatId: string,
    ...normalized: Array<string | null | undefined>
  ): string[];
  export function resolveChannelEntryMatchWithFallback<T>(params: {
    entries: Record<string, T>;
    keys: string[];
    wildcardKey?: string;
    normalizeKey?: (raw: string) => string | null | undefined;
  }): { entry?: T; wildcardEntry?: T };
}
