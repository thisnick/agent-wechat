import {
  buildChannelKeyCandidates,
  resolveAllowlistProviderRuntimeGroupPolicy,
  resolveChannelEntryMatchWithFallback,
  resolveDefaultGroupPolicy,
  resolveSenderCommandAuthorization,
  type OpenClawConfig,
} from "openclaw/plugin-sdk";
import type { ResolvedWeChatAccount, WeChatDmPolicy, WeChatGroupPolicy } from "./types.js";

type WeChatGroupEntry = NonNullable<ResolvedWeChatAccount["groups"]>[string];

type CommandAuthorizationDeps = {
  shouldComputeCommandAuthorized: (rawBody: string, cfg: OpenClawConfig) => boolean;
  resolveCommandAuthorizedFromAuthorizers: (params: {
    useAccessGroups: boolean;
    authorizers: Array<{ configured: boolean; allowed: boolean }>;
  }) => boolean;
  readAllowFromStore: () => Promise<string[]>;
};

export type WeChatPolicyContext = {
  dmPolicy: WeChatDmPolicy;
  groupPolicy: WeChatGroupPolicy;
  requireMention: boolean;
  groupEnabled: boolean;
  effectiveAllowFrom: string[];
  effectiveGroupAllowFrom: string[];
};

export type WeChatAccessDecision =
  | { allowed: true; reason: string }
  | { allowed: false; reason: string };

export type WeChatMentionGateResult = {
  effectiveWasMentioned: boolean;
  shouldSkip: boolean;
  shouldBypassMention: boolean;
};

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
  return trimmed.replace(/^wechat:/i, "").trim();
}

export function normalizeWeChatAllowFrom(values: Array<string | number> | null | undefined): string[] {
  const normalized = (values ?? [])
    .map((entry) => String(entry).trim())
    .filter(Boolean)
    .map((entry) => (entry === "*" ? "*" : normalizeWeChatId(entry)))
    .filter(Boolean);
  return unique(normalized);
}

export function isWeChatSenderAllowed(
  senderId: string | undefined,
  allowFrom: string[],
): boolean {
  if (allowFrom.includes("*")) {
    return true;
  }
  const normalizedSender = senderId ? normalizeWeChatId(senderId) : "";
  if (!normalizedSender) {
    return false;
  }
  return allowFrom.includes(normalizedSender);
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
  storeAllowFrom?: string[];
}): WeChatPolicyContext {
  const dmPolicy = normalizeDmPolicy(params.account.dmPolicy);
  const configuredAllowFrom = normalizeWeChatAllowFrom(params.account.allowFrom);
  const configuredGroupAllowFrom = normalizeWeChatAllowFrom(params.account.groupAllowFrom);
  const normalizedStoreAllowFrom =
    dmPolicy === "allowlist" ? [] : normalizeWeChatAllowFrom(params.storeAllowFrom);
  const effectiveAllowFrom = unique([...configuredAllowFrom, ...normalizedStoreAllowFrom]);
  const groupBase = configuredGroupAllowFrom.length > 0 ? configuredGroupAllowFrom : configuredAllowFrom;
  const effectiveGroupAllowFrom = unique([...groupBase, ...normalizedStoreAllowFrom]);

  const { groupEntry, wildcardEntry } = resolveGroupEntry({
    account: params.account,
    chatId: params.chatId,
  });

  const groupEnabled = firstDefined(groupEntry?.enabled, wildcardEntry?.enabled, true) !== false;
  const requireMention =
    firstDefined(groupEntry?.requireMention, wildcardEntry?.requireMention, true) !== false;

  const defaultGroupPolicy = resolveDefaultGroupPolicy(params.cfg);
  const { groupPolicy: fallbackGroupPolicy } = resolveAllowlistProviderRuntimeGroupPolicy({
    providerConfigPresent: params.cfg.channels?.wechat !== undefined,
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

  const groupAllowOverride = normalizeWeChatAllowFrom(
    firstDefined(groupEntry?.allowFrom, wildcardEntry?.allowFrom),
  );
  const groupAllowFrom =
    groupAllowOverride.length > 0
      ? unique([...groupAllowOverride, ...normalizedStoreAllowFrom])
      : effectiveGroupAllowFrom;

  return {
    dmPolicy,
    groupPolicy,
    requireMention,
    groupEnabled,
    effectiveAllowFrom,
    effectiveGroupAllowFrom: groupAllowFrom,
  };
}

export function resolveWeChatInboundAccessDecision(params: {
  isGroup: boolean;
  senderId: string | undefined;
  policy: WeChatPolicyContext;
}): WeChatAccessDecision {
  if (params.isGroup) {
    if (!params.policy.groupEnabled) {
      return { allowed: false, reason: "group-config-disabled" };
    }
    if (params.policy.groupPolicy === "disabled") {
      return { allowed: false, reason: "groupPolicy=disabled" };
    }
    if (params.policy.groupPolicy === "allowlist") {
      if (params.policy.effectiveGroupAllowFrom.length === 0) {
        return { allowed: false, reason: "groupPolicy=allowlist (empty allowlist)" };
      }
      if (!isWeChatSenderAllowed(params.senderId, params.policy.effectiveGroupAllowFrom)) {
        return { allowed: false, reason: "groupPolicy=allowlist (sender not allowlisted)" };
      }
    }
    return { allowed: true, reason: `groupPolicy=${params.policy.groupPolicy}` };
  }

  if (params.policy.dmPolicy === "disabled") {
    return { allowed: false, reason: "dmPolicy=disabled" };
  }
  if (params.policy.dmPolicy === "allowlist") {
    if (params.policy.effectiveAllowFrom.length === 0) {
      return { allowed: false, reason: "dmPolicy=allowlist (empty allowlist)" };
    }
    if (!isWeChatSenderAllowed(params.senderId, params.policy.effectiveAllowFrom)) {
      return { allowed: false, reason: "dmPolicy=allowlist (sender not allowlisted)" };
    }
  }
  return { allowed: true, reason: `dmPolicy=${params.policy.dmPolicy}` };
}

export async function resolveWeChatCommandAuthorization(params: {
  cfg: OpenClawConfig;
  rawBody: string;
  isGroup: boolean;
  senderId: string | undefined;
  dmPolicy: WeChatDmPolicy;
  allowFromForCommands: string[];
  deps: CommandAuthorizationDeps;
}): Promise<boolean | undefined> {
  const normalizedSenderId = normalizeWeChatId(params.senderId ?? "");
  const { commandAuthorized } = await resolveSenderCommandAuthorization({
    cfg: params.cfg,
    rawBody: params.rawBody,
    isGroup: params.isGroup,
    dmPolicy: params.dmPolicy,
    configuredAllowFrom: params.allowFromForCommands,
    senderId: normalizedSenderId,
    isSenderAllowed: (senderId, allowFrom) =>
      isWeChatSenderAllowed(senderId, normalizeWeChatAllowFrom(allowFrom)),
    readAllowFromStore: async () => normalizeWeChatAllowFrom(await params.deps.readAllowFromStore()),
    shouldComputeCommandAuthorized: params.deps.shouldComputeCommandAuthorized,
    resolveCommandAuthorizedFromAuthorizers: params.deps.resolveCommandAuthorizedFromAuthorizers,
  });
  return commandAuthorized;
}

export function resolveWeChatMentionGate(params: {
  isGroup: boolean;
  requireMention: boolean;
  canDetectMention: boolean;
  wasMentioned: boolean;
  implicitMention?: boolean;
  allowTextCommands: boolean;
  hasControlCommand: boolean;
  commandAuthorized: boolean;
}): WeChatMentionGateResult {
  const implicitMention = params.implicitMention === true;
  const baseWasMentioned = params.wasMentioned || implicitMention;
  const shouldBypassMention =
    params.isGroup &&
    params.requireMention &&
    !baseWasMentioned &&
    params.allowTextCommands &&
    params.hasControlCommand &&
    params.commandAuthorized;
  const effectiveWasMentioned = baseWasMentioned || shouldBypassMention;
  const shouldSkip = params.requireMention && params.canDetectMention && !effectiveWasMentioned;
  return { effectiveWasMentioned, shouldSkip, shouldBypassMention };
}
