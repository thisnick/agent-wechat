import { createHash } from "node:crypto";
import { resolveConfiguredSecretInputString } from "openclaw/plugin-sdk/secret-input-runtime";
import type { WeChatNativeConfig } from "./types.js";

type SecretConfig = Parameters<typeof resolveConfiguredSecretInputString>[0]["config"];
type SecretContext = { config: SecretConfig; path: string };
const contexts = new WeakMap<WeChatNativeConfig, SecretContext>();

/** Keep provider configuration out of the public/native configuration object. */
export function bindNativeSecretContext(native: WeChatNativeConfig, config: Record<string, unknown>, path: string): WeChatNativeConfig {
  if ("tokenFile" in native) throw new Error("native.tokenFile has been removed; use native.token with an OpenClaw SecretRef or environment reference");
  const copy = { ...native };
  contexts.set(copy, { config: config as SecretConfig, path: `${path}.token` });
  return copy;
}

export function nativeConfigurationKey(native: WeChatNativeConfig): string {
  return createHash("sha256").update(JSON.stringify({ native, providers: contexts.get(native)?.config.secrets })).digest("hex");
}

export async function resolveNativeToken(native: WeChatNativeConfig, env: NodeJS.ProcessEnv = process.env): Promise<string> {
  if ("tokenFile" in native) throw new Error("native.tokenFile has been removed; use native.token with an OpenClaw SecretRef or environment reference");
  const context = contexts.get(native);
  let value: string | undefined;
  try {
    const result = await resolveConfiguredSecretInputString({ config: context?.config ?? {}, env,
      value: native.token, path: context?.path ?? "native.token", unresolvedReasonStyle: "generic" });
    if (result.unresolvedRefReason) throw new Error("unresolved");
    value = result.value;
  } catch {
    // Provider errors can include secret identifiers or output. Never copy them.
    throw new Error("Native API token reference could not be resolved; check OpenClaw secret configuration");
  }
  if (!value || value.length < 32 || value.length > 1024 || !/^[\x21-\x7e]+$/.test(value)) {
    throw new Error("Native API token must contain 32 to 1024 printable ASCII bytes without spaces");
  }
  return value;
}
