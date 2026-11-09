import type { NativeThread } from "@agent-wechat/shared";
import type { ViewState } from "./browser-cache.ts";

export const conversationCategories = [
  { id: "dm", label: "DMs" },
  { id: "group", label: "Groups" },
  { id: "official", label: "Official" },
  { id: "service", label: "Service" },
] as const;

export type ConversationCategory = (typeof conversationCategories)[number]["id"];

export function savedConversationCategories(view: ViewState): ConversationCategory[] {
  const saved = view.categories;
  if (Array.isArray(saved)) {
    return conversationCategories.map(({ id }) => id).filter((id) => saved.includes(id));
  }
  return view.filter === "groups" ? ["group"] : [];
}

export function conversationCategory(thread: NativeThread): ConversationCategory | undefined {
  const { classification, kind } = thread;
  if (kind === "group" || classification?.category === "group") return "group";
  if (classification?.businessKind === "official_account") return "official";
  if (classification?.businessKind === "service_account") return "service";
  if (classification?.category === "system") return undefined;
  if (kind === "direct") return "dm";
}

export function conversationUnreadCounts(threads: NativeThread[]) {
  const counts = { total: 0, dm: 0, group: 0, official: 0, service: 0 };
  for (const thread of threads) {
    const unread = Math.max(0, thread.unreadCount ?? 0);
    const category = conversationCategory(thread);
    counts.total += unread;
    if (category) counts[category] += unread;
  }
  return counts;
}
