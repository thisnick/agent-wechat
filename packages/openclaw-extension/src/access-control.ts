import type { OpenClawConfig } from "openclaw/plugin-sdk/core";
import {
  buildChannelKeyCandidates,
  resolveChannelEntryMatchWithFallback,
} from "openclaw/plugin-sdk/channel-targets";
import type {
  ChannelIngressRouteDescriptor,
  ResolveStableChannelMessageIngressParams,
  ResolvedChannelMessageIngress,
  StableChannelIngressIdentityParams,
} from "openclaw/plugin-sdk/channel-ingress-runtime";
import {
  resolveAllowlistProviderRuntimeGroupPolicy,
  resolveDefaultGroupPolicy,
} from "openclaw/plugin-sdk/runtime-group-policy";
import type { ResolvedWeChatAccount, WeChatDmPolicy, WeChatGroupPolicy } from "./types.js";

type WeChatGroupEntry = NonNullable<ResolvedWeChatAccount["groups"]>[string];

/**
 * WeChat-specific facts that feed OpenClaw's channel ingress resolver. Lists are the
 * configured entries (normalized ids, `*`, `accessGroup:<name>`); core derives the
 * effective allowlists, command owners and access-group membership from them.
 */
export type WeChatPolicyContext = {
  dmPolicy: WeChatDmPolicy;
  groupPolicy: WeChatGroupPolicy;
  requireMention: boolean;
  groupEnabled: boolean;
  allowFrom: string[];
  groupAllowFrom: string[];
  /** Per-group `allowFrom` from `groups.<id>` or `groups["*"]`; replaces groupAllowFrom for that group. */
  groupSenderAllowFrom?: string[];
  /**
   * `dmPolicy: "open"` without `"*"` in `allowFrom`. The plugin has always documented open as
   * "anyone can DM", but OpenClaw core admits nobody unless the wildcard is listed, so the
   * wildcard is added and the monitor warns once.
   */
  impliedOpenDmWildcard?: boolean;
  /** Whether an empty groupAllowFrom falls back to allowFrom (core default for WeChat: yes). */
  groupAllowFromFallbackToAllowFrom: boolean;
  /** Command owners when they differ from the effective DM allowlist. */
  commandOwnerAllowFrom?: string[];
};

/** Same signature as the runtime's `channel.inbound.ingress.resolveStable` and the SDK helper. */
export type WeChatIngressResolver = (
  params: ResolveStableChannelMessageIngressParams,
) => Promise<ResolvedChannelMessageIngress>;

const INVISIBLE_TEXT_RE = /[\u200b-\u200f\u202a-\u202e\u2060-\u206f]/g;
const WECHAT_MENTION_START_RE = /^[@＠]/u;

function unique(values: string[]): string[] {
  return Array.from(new Set(values));
}

function normalizeDmPolicy(policy: string | undefined): WeChatDmPolicy {
  if (policy === "open" || policy === "allowlist" || policy === "disabled") {
    return policy;
  }
  return "disabled";
}

function normalizeGroupPolicy(policy: string | undefined): WeChatGroupPolicy | undefined {
  if (policy === "open" || policy === "allowlist" || policy === "disabled") {
    return policy;
  }
  return undefined;
}

function firstDefined<T>(...values: Array<T | undefined>): T | undefined {
  for (const value of values) {
    if (value !== undefined) {
      return value;
    }
  }
  return undefined;
}

export function normalizeWeChatId(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) {
    return "";
  }
  return trimmed.replace(/^(agent-)?wechat:/i, "").trim();
}

export function normalizeWeChatAllowFrom(values: Array<string | number> | null | undefined): string[] {
  const normalized = (values ?? [])
    .map((entry) => String(entry).trim())
    .filter(Boolean)
    .map((entry) => (entry === "*" ? "*" : normalizeWeChatId(entry)))
    .filter(Boolean);
  return unique(normalized);
}

function findCommandTokenStart(input: string): number {
  const match = /(?:^|\s)([/!][A-Za-z])/u.exec(input);
  if (!match) {
    return -1;
  }
  const whole = match[0] ?? "";
  const startsWithSpace = whole.startsWith(" ");
  return (match.index ?? 0) + (startsWithSpace ? 1 : 0);
}

export function normalizeWeChatCommandBody(
  raw: string,
  params?: { isGroup?: boolean; wasMentioned?: boolean },
): string {
  const trimmed = raw.replace(INVISIBLE_TEXT_RE, "").trim();
  if (!trimmed) {
    return "";
  }
  const isGroup = params?.isGroup === true;
  const wasMentioned = params?.wasMentioned === true;
  if (!isGroup || !wasMentioned) {
    return trimmed;
  }
  const commandStart = findCommandTokenStart(trimmed);
  if (commandStart < 0) {
    return trimmed;
  }
  const rawPrefix = trimmed.slice(0, commandStart);
  const prefix = rawPrefix.trim();
  if (!prefix) {
    return trimmed.slice(commandStart).trimStart();
  }
  // WeChat terminates each @-mention with a hair space (U+2005).
  // Split on hair space to preserve spaces within display names
  // (e.g. "@Agent Name\u2005" is a single mention token).
  // The hair space may be within rawPrefix (between multiple mentions)
  // or at trimmed[commandStart] (between last mention and the command).
  const hasHairSpace = rawPrefix.includes("\u2005") || trimmed[commandStart] === "\u2005";
  if (!hasHairSpace) {
    return trimmed;
  }
  const mentionRegion = rawPrefix + (trimmed[commandStart] === "\u2005" ? "\u2005" : "");
  const prefixTokens = mentionRegion
    .split(/\u2005+/)
    .map((token) => token.trim())
    .filter(Boolean);
  if (prefixTokens.length > 0 && prefixTokens.every((token) => WECHAT_MENTION_START_RE.test(token))) {
    return trimmed.slice(commandStart).trimStart();
  }
  return trimmed;
}

function resolveGroupEntry(params: {
  account: ResolvedWeChatAccount;
  chatId: string;
}): {
  groupEntry?: WeChatGroupEntry;
  wildcardEntry?: WeChatGroupEntry;
} {
  const groups = params.account.groups ?? {};
  const normalizedChatId = normalizeWeChatId(params.chatId);
  const keys = buildChannelKeyCandidates(params.chatId, normalizedChatId);
  const match = resolveChannelEntryMatchWithFallback<WeChatGroupEntry>({
    entries: groups,
    keys,
    wildcardKey: "*",
    normalizeKey: normalizeWeChatId,
  });
  return {
    groupEntry: match.entry,
    wildcardEntry: match.wildcardEntry,
  };
}

export function resolveWeChatPolicyContext(params: {
  account: ResolvedWeChatAccount;
  cfg: OpenClawConfig;
  chatId: string;
}): WeChatPolicyContext {
  const { groupEntry, wildcardEntry } = resolveGroupEntry({
    account: params.account,
    chatId: params.chatId,
  });

  const groupEnabled = firstDefined(groupEntry?.enabled, wildcardEntry?.enabled, true) !== false;
  const requireMention =
    firstDefined(groupEntry?.requireMention, wildcardEntry?.requireMention, true) !== false;

  const defaultGroupPolicy = resolveDefaultGroupPolicy(params.cfg);
  const { groupPolicy: fallbackGroupPolicy } = resolveAllowlistProviderRuntimeGroupPolicy({
    providerConfigPresent: params.cfg.channels?.["agent-wechat"] !== undefined,
    groupPolicy: normalizeGroupPolicy(params.account.groupPolicy),
    defaultGroupPolicy: normalizeGroupPolicy(defaultGroupPolicy),
  });
  const groupPolicy =
    normalizeGroupPolicy(
      firstDefined(
        groupEntry?.groupPolicy,
        wildcardEntry?.groupPolicy,
        params.account.groupPolicy,
        defaultGroupPolicy,
      ),
    ) ?? fallbackGroupPolicy;

  const groupSenderAllowFrom = normalizeWeChatAllowFrom(
    firstDefined(groupEntry?.allowFrom, wildcardEntry?.allowFrom),
  );

  const dmPolicy = normalizeDmPolicy(params.account.dmPolicy);
  const allowFrom = normalizeWeChatAllowFrom(params.account.allowFrom);
  const groupAllowFrom = normalizeWeChatAllowFrom(params.account.groupAllowFrom);
  const impliedOpenDmWildcard = dmPolicy === "open" && !allowFrom.includes("*");
  // The implied wildcard only opens DMs. Group senders and command owners keep using the
  // configured list, so the group fallback is applied here instead of by core.
  const scoped = impliedOpenDmWildcard
    ? {
        allowFrom: ["*", ...allowFrom],
        impliedOpenDmWildcard,
        groupAllowFrom: groupAllowFrom.length > 0 ? groupAllowFrom : allowFrom,
        groupAllowFromFallbackToAllowFrom: false,
        commandOwnerAllowFrom: allowFrom,
      }
    : { allowFrom, groupAllowFrom, groupAllowFromFallbackToAllowFrom: true };

  return {
    dmPolicy,
    groupPolicy,
    requireMention,
    groupEnabled,
    ...scoped,
    ...(groupSenderAllowFrom.length > 0 ? { groupSenderAllowFrom } : {}),
  };
}

/** WeChat ids (`wxid_…`, `…@chatroom`) are stable and come from the logged-in client's database. */
export const WECHAT_INGRESS_IDENTITY: StableChannelIngressIdentityParams = {
  key: "wechat-id",
  entryIdPrefix: "wechat",
  sensitivity: "pii",
  normalize: (value) => normalizeWeChatId(value) || null,
};

/**
 * Resolve one inbound WeChat message through OpenClaw's channel ingress policy: DM and
 * group sender gates, the per-group route (enabled / sender override), and optionally the
 * control-command and mention gates. WeChat has no pairing flow, so events never pair.
 */
export function resolveWeChatIngress(params: {
  resolve: WeChatIngressResolver;
  cfg: OpenClawConfig;
  accountId: string;
  policy: WeChatPolicyContext;
  isGroup: boolean;
  chatId: string;
  senderId: string | undefined;
  readStoreAllowFrom?: () => Promise<readonly string[]>;
  /** Run the command gate for this message. */
  command?: { allowTextCommands: boolean; hasControlCommand: boolean };
  /** Run the mention gate (groups only) with this mention fact. */
  mention?: { wasMentioned: boolean; allowTextCommands: boolean };
}): Promise<ResolvedChannelMessageIngress> {
  const { policy, isGroup } = params;
  const routes: ChannelIngressRouteDescriptor[] = [];
  if (isGroup && !policy.groupEnabled) {
    routes.push({
      id: "wechat-group",
      kind: "route",
      configured: true,
      matched: true,
      enabled: true,
      allowed: false,
      blockReason: "group_disabled",
    });
  }
  // A per-group allowFrom is that group's sender allowlist, for messages and commands alike.
  const groupAllowFrom = (isGroup && policy.groupSenderAllowFrom) || policy.groupAllowFrom;
  const mention = isGroup ? params.mention : undefined;
  const readStoreAllowFrom = params.readStoreAllowFrom;
  return params.resolve({
    channelId: "agent-wechat",
    accountId: params.accountId,
    identity: WECHAT_INGRESS_IDENTITY,
    cfg: params.cfg as ResolveStableChannelMessageIngressParams["cfg"],
    subject: { stableId: normalizeWeChatId(params.senderId ?? "") || null },
    conversation: { kind: isGroup ? "group" : "direct", id: params.chatId },
    event: { kind: "message", authMode: "inbound", mayPair: false },
    dmPolicy: policy.dmPolicy,
    groupPolicy: policy.groupPolicy,
    policy: {
      groupAllowFromFallbackToAllowFrom: policy.groupAllowFromFallbackToAllowFrom,
      ...(mention
        ? { activation: { requireMention: policy.requireMention, allowTextCommands: mention.allowTextCommands } }
        : {}),
    },
    allowFrom: policy.allowFrom,
    groupAllowFrom,
    ...(routes.length > 0 ? { route: routes } : {}),
    ...(readStoreAllowFrom ? { readStoreAllowFrom: async () => readStoreAllowFrom() } : {}),
    ...(mention ? { mentionFacts: { canDetectMention: true, wasMentioned: mention.wasMentioned } } : {}),
    command: params.command
      ? {
          ...params.command,
          // Group commands are authorized by the group's sender allowlist only, as before.
          groupOwnerAllowFrom: "none",
          ...(policy.commandOwnerAllowFrom && !isGroup
            ? { commandOwnerAllowFrom: policy.commandOwnerAllowFrom, commandGroupAllowFromFallbackToAllowFrom: false }
            : {}),
        }
      : false,
  });
}
