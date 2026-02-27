export type WeChatDmPolicy = "allowlist" | "open" | "disabled";
export type WeChatGroupPolicy = "open" | "disabled" | "allowlist";

export type WeChatGroupConfig = {
  enabled?: boolean;
  requireMention?: boolean;
  groupPolicy?: WeChatGroupPolicy;
  allowFrom?: string[];
};

export type WeChatConfig = {
  enabled?: boolean;
  serverUrl: string;
  token?: string;
  dmPolicy?: WeChatDmPolicy;
  allowFrom?: string[];
  groupPolicy?: WeChatGroupPolicy;
  groupAllowFrom?: string[];
  groups?: Record<string, WeChatGroupConfig>;
  pollIntervalMs?: number;
  authPollIntervalMs?: number;
};

export type ResolvedWeChatAccount = {
  accountId: string;
  enabled: boolean;
  serverUrl: string;
  token?: string;
  dmPolicy: WeChatDmPolicy;
  allowFrom: string[];
  groupPolicy: WeChatGroupPolicy;
  groupAllowFrom: string[];
  groups: Record<string, WeChatGroupConfig>;
  pollIntervalMs: number;
  authPollIntervalMs: number;
};

function normalizeDmPolicy(policy: unknown): WeChatDmPolicy {
  return policy === "allowlist" || policy === "open" || policy === "disabled"
    ? policy
    : "disabled";
}

function normalizeGroupPolicy(policy: unknown): WeChatGroupPolicy {
  return policy === "open" || policy === "disabled" || policy === "allowlist"
    ? policy
    : "disabled";
}

// Defaults
export const DEFAULT_POLL_INTERVAL_MS = 1000;
export const DEFAULT_AUTH_POLL_INTERVAL_MS = 30_000;
export const DEFAULT_ACCOUNT_ID = "default";

export function resolveWeChatAccount(
  cfg: Record<string, unknown>,
  accountId?: string,
): ResolvedWeChatAccount | null {
  const wechat = (cfg as { channels?: { wechat?: WeChatConfig } }).channels
    ?.wechat;
  if (!wechat?.serverUrl) return null;

  return {
    accountId: accountId ?? DEFAULT_ACCOUNT_ID,
    enabled: wechat.enabled !== false,
    serverUrl: wechat.serverUrl,
    token: wechat.token,
    dmPolicy: normalizeDmPolicy(wechat.dmPolicy),
    allowFrom: wechat.allowFrom ?? [],
    groupPolicy: normalizeGroupPolicy(wechat.groupPolicy),
    groupAllowFrom: wechat.groupAllowFrom ?? [],
    groups: wechat.groups ?? {},
    pollIntervalMs: wechat.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS,
    authPollIntervalMs:
      wechat.authPollIntervalMs ?? DEFAULT_AUTH_POLL_INTERVAL_MS,
  };
}
