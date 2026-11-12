// URL-addressable views. A route names what the user wants to see and carries
// only identifiers, never message text or access keys. App state decides what
// can actually be shown; see main.tsx.
//
// This file must stay free of TypeScript-only runtime syntax (enums, parameter
// properties): scripts/interface-listing.mjs imports it with Node's type stripping.

export type RouteName =
  | "servers"
  | "accounts"
  | "login"
  | "chats"
  | "chat"
  | "message"
  | "media"
  | "info"
  | "mini-programs"
  | "contacts"
  | "new-chat"
  | "settings";

export type Route =
  | { name: "servers" }
  | { name: "accounts"; server: string }
  | { name: "login"; server: string; account: string }
  | { name: "chats"; server: string; account: string }
  | { name: "chat"; server: string; account: string; thread: string }
  | { name: "message"; server: string; account: string; thread: string; message: string }
  | { name: "media"; server: string; account: string; thread: string; message: string }
  | { name: "info"; server: string; account: string; thread: string }
  | { name: "mini-programs"; server: string; account: string }
  | { name: "contacts"; server: string; account: string }
  | { name: "new-chat"; server: string; account: string }
  | { name: "settings"; server: string; account: string };

/** Every route that belongs to one account on one server. */
export type AccountRoute = Exclude<Route, { name: "servers" | "accounts" }>;

/** The routes shown by the signed-in client: a view, or a dialog over a view. */
export type ChatsRoute = Exclude<AccountRoute, { name: "login" }>;

/** The routes of the signed-in client that are a full view rather than a dialog. */
export type ViewRoute = Exclude<ChatsRoute, { name: "new-chat" | "settings" }>;

export type RouteEntry = {
  /** Slash-separated pattern; `{param}` segments are identifiers. */
  pattern: string;
  name: RouteName;
  /** What the route shows, in one sentence. */
  description: string;
};

/** Every route the app understands. The listing in interfaces/ui is generated from this. */
export const routeTable: readonly RouteEntry[] = [
  {
    pattern: "/servers",
    name: "servers",
    description: "Saved hubs in the desktop app, or the hub picker in a browser.",
  },
  {
    pattern: "/{server}/accounts",
    name: "accounts",
    description: "The accounts on a hub, where you sign in, add or manage one.",
  },
  {
    pattern: "/{server}/{account}/login",
    name: "login",
    description: "Sign in to one account with a QR code.",
  },
  {
    pattern: "/{server}/{account}/chats",
    name: "chats",
    description: "The chat list of an account.",
  },
  {
    pattern: "/{server}/{account}/chats/{thread}",
    name: "chat",
    description: "One conversation.",
  },
  {
    pattern: "/{server}/{account}/chats/{thread}/message/{message}",
    name: "message",
    description: "A conversation scrolled to one message, which is highlighted.",
  },
  {
    pattern: "/{server}/{account}/chats/{thread}/media/{message}",
    name: "media",
    description: "The photo and video viewer, opened on one message of a conversation.",
  },
  {
    pattern: "/{server}/{account}/chats/{thread}/info",
    name: "info",
    description: "Conversation details, including group settings for a group chat.",
  },
  {
    pattern: "/{server}/{account}/mini-programs",
    name: "mini-programs",
    description: "Mini Programs and package availability for the selected account.",
  },
  {
    pattern: "/{server}/{account}/contacts",
    name: "contacts",
    description: "The contact list of an account.",
  },
  {
    pattern: "/{server}/{account}/new-chat",
    name: "new-chat",
    description: "The dialog for starting a conversation or creating a group.",
  },
  {
    pattern: "/{server}/{account}/settings",
    name: "settings",
    description: "Settings for the app, the hub and the account.",
  },
];

/** The identifier names in a pattern, in order. */
export function routeParams(pattern: string): string[] {
  return pattern.split("/").flatMap((part) => (part.startsWith("{") ? [part.slice(1, -1)] : []));
}

// A pasted link can be arbitrarily long; no real identifier comes close.
const MAX_SEGMENT = 2048;

function compile(entry: RouteEntry) {
  return { entry, parts: entry.pattern.split("/").slice(1) };
}
const compiled = routeTable.map(compile);

/**
 * Reads a location hash such as `#/hub/account/chats/room%40chatroom`. Returns
 * `null` for an empty, unknown or malformed hash so the caller can fall back to
 * its default view. One trailing slash is ignored.
 */
export function parseRoute(hash: string): Route | null {
  const path = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!path.startsWith("/")) return null;
  const segments = path.replace(/\/+$/, "").split("/").slice(1);
  for (const { entry, parts } of compiled) {
    if (parts.length !== segments.length) continue;
    const route: Record<string, string> = { name: entry.name };
    let matched = true;
    for (let index = 0; matched && index < parts.length; index++) {
      const part = parts[index];
      if (!part.startsWith("{")) {
        matched = part === segments[index];
        continue;
      }
      let value: string;
      try {
        value = decodeURIComponent(segments[index]);
      } catch {
        return null;
      }
      if (!value || value.length > MAX_SEGMENT) return null;
      route[part.slice(1, -1)] = value;
    }
    if (matched) return route as Route;
  }
  return null;
}

/** The location hash for a route. `parseRoute(formatRoute(route))` returns an equal route. */
export function formatRoute(route: Route): string {
  const entry = compiled.find((item) => item.entry.name === route.name);
  if (!entry) throw new Error(`Unknown route ${(route as { name: string }).name}`);
  const values = route as unknown as Record<string, string>;
  return (
    "#/" +
    entry.parts
      .map((part) => (part.startsWith("{") ? encodeURIComponent(values[part.slice(1, -1)]) : part))
      .join("/")
  );
}

/** True for every route that belongs to one account on one server. */
export function isAccountRoute(route: Route | null): route is AccountRoute {
  return !!route && route.name !== "servers" && route.name !== "accounts";
}

/** True for the routes the signed-in client shows. */
export function isChatsRoute(route: Route | null): route is ChatsRoute {
  return isAccountRoute(route) && route.name !== "login";
}

/** True for the signed-in routes that are a full view rather than a dialog. */
export function isViewRoute(route: Route | null): route is ViewRoute {
  return isChatsRoute(route) && route.name !== "new-chat" && route.name !== "settings";
}
