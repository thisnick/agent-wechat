export type WeChatDmPolicy = "allowlist" | "open" | "disabled";
export type WeChatGroupPolicy = "open" | "disabled" | "allowlist";

export type WeChatGroupConfig = {
  enabled?: boolean;
  requireMention?: boolean;
  groupPolicy?: WeChatGroupPolicy;
  allowFrom?: string[];
};

export type WeChatNativeConfig = {
  mode: "managed" | "external";
  accountId: string;
  endpoint?: string;
  executable?: string;
  serverConfig?: string;
  dataDir?: string;
  listen?: string;
  tokenFile: string;
  consumerStateDir: string;
  readPolicy?: "never" | "on_model_dispatch";
  gatewayFacade?: { enabled: boolean; prefix?: string };
};

export type WeChatConfig = {
  backend?: "legacy" | "native";
  native?: WeChatNativeConfig;
  enabled?: boolean;
  serverUrl?: string;
  token?: string;
  dmPolicy?: WeChatDmPolicy;
  allowFrom?: string[];
  groupPolicy?: WeChatGroupPolicy;
  groupAllowFrom?: string[];
  groups?: Record<string, WeChatGroupConfig>;
  pollIntervalMs?: number;
  authPollIntervalMs?: number;
  /** Maximum inbound attachment size kept in OpenClaw-managed storage. */
  mediaMaxMb?: number;
};

export type ResolvedWeChatAccount = {
  backend?: "legacy" | "native";
  native?: WeChatNativeConfig;
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
  mediaMaxMb: number;
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
export const DEFAULT_MEDIA_MAX_MB = 50;
export const DEFAULT_ACCOUNT_ID = "default";

export function resolveWeChatAccount(
  cfg: Record<string, unknown>,
  accountId?: string,
): ResolvedWeChatAccount | null {
  const wechat = (cfg as { channels?: { "agent-wechat"?: WeChatConfig } })
    .channels?.["agent-wechat"];
  const plugin = (cfg as any)?.plugins?.entries?.["agent-wechat"]?.config as Partial<WeChatConfig> | undefined;
  const backend = wechat?.backend ?? plugin?.backend ?? "legacy";
  const native = wechat?.native ?? plugin?.native;
  const serverUrl = backend === "native"
    ? native?.mode === "external" ? native.endpoint ?? "" : `http://${native?.listen ?? "127.0.0.1:6174"}`
    : wechat?.serverUrl;
  if (!serverUrl || (backend === "native" && !native)) return null;

  return {
    backend,
    native,
    accountId: accountId ?? DEFAULT_ACCOUNT_ID,
    enabled: wechat?.enabled !== false,
    serverUrl,
    token: wechat?.token,
    dmPolicy: normalizeDmPolicy(wechat?.dmPolicy),
    allowFrom: wechat?.allowFrom ?? [],
    groupPolicy: normalizeGroupPolicy(wechat?.groupPolicy),
    groupAllowFrom: wechat?.groupAllowFrom ?? [],
    groups: wechat?.groups ?? {},
    pollIntervalMs: wechat?.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS,
    authPollIntervalMs:
      wechat?.authPollIntervalMs ?? DEFAULT_AUTH_POLL_INTERVAL_MS,
    mediaMaxMb: Number.isInteger(wechat?.mediaMaxMb) && (wechat?.mediaMaxMb ?? 0) > 0 &&
        (wechat?.mediaMaxMb ?? 0) <= 1024
      ? wechat?.mediaMaxMb!
      : DEFAULT_MEDIA_MAX_MB,
  };
}
