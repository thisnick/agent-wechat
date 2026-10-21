import type { SecretInput } from "openclaw/plugin-sdk/secret-input";
import { bindNativeSecretContext } from "./native-secrets.ts";

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
  token: SecretInput;
  consumerStateDir: string;
  readPolicy?: "never" | "on_inbound" | "on_model_dispatch";
  gatewayFacade?: { enabled: boolean; prefix?: string };
};

export type WeChatConfig = {
  backend?: "native";
  native?: WeChatNativeConfig;
  enabled?: boolean;
  dmPolicy?: WeChatDmPolicy;
  allowFrom?: string[];
  groupPolicy?: WeChatGroupPolicy;
  groupAllowFrom?: string[];
  groups?: Record<string, WeChatGroupConfig>;
  /** Maximum inbound attachment size kept in OpenClaw-managed storage. */
  mediaMaxMb?: number;
};

export type ResolvedWeChatAccount = {
  backend?: "native";
  native?: WeChatNativeConfig;
  accountId: string;
  enabled: boolean;
  serverUrl: string;
  dmPolicy: WeChatDmPolicy;
  allowFrom: string[];
  groupPolicy: WeChatGroupPolicy;
  groupAllowFrom: string[];
  groups: Record<string, WeChatGroupConfig>;
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
export const DEFAULT_MEDIA_MAX_MB = 50;
export const DEFAULT_ACCOUNT_ID = "default";

export function resolveWeChatAccount(
  cfg: Record<string, unknown>,
  accountId?: string,
): ResolvedWeChatAccount | null {
  const wechat = (cfg as { channels?: { "agent-wechat"?: WeChatConfig } })
    .channels?.["agent-wechat"];
  const plugin = (cfg as any)?.plugins?.entries?.["agent-wechat"]?.config as Partial<WeChatConfig> | undefined;
  const requested = wechat?.backend ?? plugin?.backend;
  if ((requested && requested !== "native") || (wechat && ("serverUrl" in wechat || "token" in wechat))) {
    throw new Error("The official-client container backend has been removed. Configure channels.agent-wechat.native with a managed or external native daemon and a token reference; preserve the stopped official profile for takeover.");
  }
  const selectedNative = wechat?.native ?? plugin?.native;
  if (!selectedNative) return null;
  const native = bindNativeSecretContext(selectedNative, cfg, wechat?.native
    ? "channels.agent-wechat.native" : "plugins.entries.agent-wechat.config.native");
  const serverUrl = native.mode === "external" ? native.endpoint ?? "" : `http://${native.listen ?? "127.0.0.1:6174"}`;
  if (!serverUrl) return null;

  return {
    backend: "native",
    native,
    accountId: accountId ?? DEFAULT_ACCOUNT_ID,
    enabled: wechat?.enabled !== false,
    serverUrl,
    dmPolicy: normalizeDmPolicy(wechat?.dmPolicy),
    allowFrom: wechat?.allowFrom ?? [],
    groupPolicy: normalizeGroupPolicy(wechat?.groupPolicy),
    groupAllowFrom: wechat?.groupAllowFrom ?? [],
    groups: wechat?.groups ?? {},
    mediaMaxMb: Number.isInteger(wechat?.mediaMaxMb) && (wechat?.mediaMaxMb ?? 0) > 0 &&
        (wechat?.mediaMaxMb ?? 0) <= 1024
      ? wechat?.mediaMaxMb!
      : DEFAULT_MEDIA_MAX_MB,
  };
}

export interface WeChatMonitorOptions {
  account: ResolvedWeChatAccount;
  abortSignal: AbortSignal;
  runtime: any;
  setStatus: (next: any) => void;
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void };
  cfg: any;
}
