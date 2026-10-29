import type { ChannelPlugin } from "openclaw/plugin-sdk/core";
import { NativeApiError, type NativeWeChatClient } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import { resolveWeChatAccount, type ResolvedWeChatAccount } from "./types.ts";

type Messaging = NonNullable<ChannelPlugin<ResolvedWeChatAccount>["messaging"]>;
type TargetClient = Pick<NativeWeChatClient, "contact" | "thread">;
type ResolveClient = (cfg: Parameters<typeof resolveWeChatAccount>[0], accountId?: string | null) => Promise<{ client: TargetClient; accountId: string }>;

export function normalizeWeChatTarget(raw: string): string | undefined {
  return raw.trim().replace(/^(agent-)?wechat:/i, "").trim() || undefined;
}

const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const display = (value: unknown, id: string) => typeof value === "string" && value ? value : id;

async function configuredClient(cfg: Parameters<typeof resolveWeChatAccount>[0], accountId?: string | null) {
  const account = resolveWeChatAccount(cfg, accountId ?? undefined);
  if (!account?.native) throw new Error("No native WeChat daemon configured");
  const { client } = await ensureNativeHost(account.native);
  return { client, accountId: account.native.accountId };
}

export function createWeChatMessagingAdapter(resolveClient: ResolveClient = configuredClient): Messaging {
  return {
    normalizeTarget: normalizeWeChatTarget,
    inferTargetChatType: ({ to }) => normalizeWeChatTarget(to)?.endsWith("@chatroom") ? "group" : "direct",
    targetResolver: {
      // Treat targets as native IDs and validate them here, including malformed
      // provider prefixes, before the host can substitute a display-name match.
      looksLikeId: () => true,
      hint: "WeChat contact ID or group ID (not a nickname or alias)",
      resolveTarget: async ({ cfg, accountId, input, preferredKind }) => {
        const id = normalizeWeChatTarget(input);
        if (!id || id === "." || id === ".." || id.length > 4096 || /[\u0000-\u001f\u007f]/.test(id)) {
          throw new Error("Invalid WeChat target ID");
        }
        const kind = id.endsWith("@chatroom") ? "group" as const : "user" as const;
        if ((preferredKind === "user" && kind !== "user") || (preferredKind && preferredKind !== "user" && kind !== "group")) {
          throw new Error("WeChat target kind does not match the requested destination");
        }
        // Preserve known native ID forms, including targets not in local catalogs.
        if (kind === "group" || id.startsWith("wxid_")) return { to: id, kind, source: "normalized" };

        const native = await resolveClient(cfg as Parameters<typeof resolveWeChatAccount>[0], accountId);
        let contact: unknown;
        try {
          contact = await native.client.contact(native.accountId, id);
        } catch (error) {
          // Missing contacts can still have a direct conversation. Authentication,
          // transport and server failures must not be interpreted as a catalog miss.
          if (!(error instanceof NativeApiError) || error.status !== 404) throw error;
          const thread: unknown = await native.client.thread(native.accountId, id);
          if (!record(thread) || thread.deleted === true || thread.id !== id || thread.peerId !== id
            || thread.accountId !== native.accountId || thread.kind !== "direct") {
            throw new Error("Native WeChat conversation identity mismatch");
          }
          return { to: id, kind: "user", display: display(thread.displayName, id), source: "directory" };
        }
        if (!record(contact) || contact.deleted === true || contact.id !== id || contact.username !== id
          || (contact.accountId !== undefined && contact.accountId !== native.accountId)) {
          throw new Error("Native WeChat contact identity mismatch");
        }
        return { to: id, kind: "user", display: display(contact.displayName, id), source: "directory" };
      },
    },
  };
}
