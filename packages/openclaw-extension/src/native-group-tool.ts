import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { ensureNativeHost } from "./native-host.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";

const name = "wechat_group_info";
/** Owner-only account-wide metadata. Group ownership is not OpenClaw authority. */
export function createNativeGroupTools(context: OpenClawPluginToolContext, dependencies: { host?: typeof ensureNativeHost } = {}): ChannelAgentTool[] {
  if (context.senderIsOwner !== true || typeof context.assertInvocationCurrent !== "function") return [];
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.enabled || !selected.native || context.agentAccountId && context.agentAccountId !== selected.accountId
    || context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId) return [];
  const accountId = selected.native.accountId;
  const configurationKey = nativeConfigurationKey(selected.native);
  return [{ name, label: "WeChat Group Info",
    description: "Read cached group information and one page of members for the configured WeChat account. Available only to the trusted OpenClaw owner. Does not create groups, invite people or change settings. Cached member absence does not prove non-membership.",
    parameters: { type: "object", additionalProperties: false, properties: {
      groupId: { type: "string", minLength: 10, maxLength: 256, description: "Exact ordinary group username ending in @chatroom." },
      limit: { type: "integer", minimum: 1, maximum: 100 },
      cursor: { type: "string", maxLength: 8192 },
      username: { type: "string", minLength: 1, maxLength: 256, description: "Optionally inspect one exact member instead of listing a page." },
    }, required: ["groupId"] } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some(k => !["groupId", "limit", "cursor", "username"].includes(k))) throw Error("Invalid group info request");
      const args = input as { groupId?: unknown; limit?: unknown; cursor?: unknown; username?: unknown };
      const identity = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 256 && !/[\s\u0000-\u001f\u007f]/u.test(value);
      if (!identity(args.groupId) || !args.groupId.endsWith("@chatroom") || args.groupId.length <= 9
        || args.limit != null && (!Number.isInteger(args.limit) || Number(args.limit) < 1 || Number(args.limit) > 100)
        || args.cursor != null && (typeof args.cursor !== "string" || args.cursor.length > 8192)
        || args.username != null && !identity(args.username)
        || args.username != null && (args.limit != null || args.cursor != null)) throw Error("Invalid group info selectors");
      const guard = () => {
        signal?.throwIfAborted();
        if (context.senderIsOwner !== true) throw Error("OpenClaw owner authorization is required");
        context.assertInvocationCurrent!();
        const account = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
        if (!account?.enabled || !account.native || account.accountId !== selected.accountId || account.native.accountId !== accountId
          || nativeConfigurationKey(account.native) !== configurationKey) throw Error("WeChat account configuration changed; start a new turn");
        return account.native;
      };
      const native = guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(native); guard();
      const group = await client.group(accountId, args.groupId, signal); guard();
      if (group.id !== args.groupId || group.roomId !== args.groupId) throw Error("Group identity is invalid");
      const members = typeof args.username === "string"
        ? await client.groupMember(accountId, args.groupId, args.username, signal)
        : await client.groupMembers(accountId, args.groupId, { limit: args.limit == null ? 50 : Number(args.limit), cursor: args.cursor as string | undefined }, signal);
      guard();
      const details = { accountId, group, members, source: "cached", membershipAbsence: "unconfirmed" };
      return { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
    },
  }];
}
export function registerNativeGroupTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeGroupTools(context) }, { names: [name] });
}
