import { createHash } from "node:crypto";
import { resolveConfiguredSecretInputString } from "openclaw/plugin-sdk/secret-input-runtime";
import type { ResolvedWeChatCallsConfig, ResolvedWeChatVoiceCallsConfig } from "./calls-config.ts";

type SecretConfig = Parameters<typeof resolveConfiguredSecretInputString>[0]["config"];
const secrets = new WeakMap<ResolvedWeChatVoiceCallsConfig, unknown>();

/** Keep API credentials/references out of resolved account data and status. */
export function bindCallVoiceSecret(config: ResolvedWeChatVoiceCallsConfig, value: unknown): ResolvedWeChatVoiceCallsConfig {
  secrets.set(config, value); return config;
}
export function callsConfigurationKey(config: ResolvedWeChatCallsConfig): string {
  return createHash("sha256").update(JSON.stringify([config, config.voice && secrets.get(config.voice)])).digest("hex");
}
export async function resolveCallVoiceApiKey(config: ResolvedWeChatVoiceCallsConfig, cfg: unknown, env: NodeJS.ProcessEnv = process.env): Promise<string | undefined> {
  const value = secrets.get(config);
  if (value === undefined) return undefined;
  try {
    const resolved = await resolveConfiguredSecretInputString({ config: (cfg ?? {}) as SecretConfig, env, value,
      path: "calls.voice.realtime.providers.openai.apiKey", unresolvedReasonStyle: "generic" });
    if (resolved.unresolvedRefReason || !resolved.value || resolved.value.length > 8192 || !/^[\x21-\x7e]+$/.test(resolved.value)) throw Error("unresolved");
    return resolved.value;
  } catch {
    throw Error("Calling OpenAI API key could not be resolved; check OpenClaw secret configuration");
  }
}
