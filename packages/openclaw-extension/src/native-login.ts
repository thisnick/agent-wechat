import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import type { NativeOperation, NativeWeChatClient } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { nativeOutboundDelivery, submitNativeText, type NativeOutboundDelivery } from "./native-send.ts";
import { resolveWeChatAccount, type ResolvedWeChatAccount } from "./types.ts";
type LoginAccess = { host?: typeof ensureNativeHost; signal?: AbortSignal; assertCurrent?: () => void };
type LoginResult = { connected: boolean; state: string; operationId?: string; qrDataUrl?: string; message: string };
const operations = new Map<string, { client: NativeWeChatClient; accountId: string; configurationKey: string; operation: NativeOperation }>();
const terminal = new Set(["succeeded", "failed", "cancelled", "expired", "rejected", "needs_login"]);
function guard(access: LoginAccess): void { access.signal?.throwIfAborted(); access.assertCurrent?.(); }
function validateOperation(operation: NativeOperation, accountId: string, operationId?: string): void {
  if (operation.accountId !== accountId || operation.kind !== "login" || (operationId && operation.id !== operationId)) throw Error("Native login operation identity does not match its account");
}
export async function nativeLoginStart(account: ResolvedWeChatAccount, options: { force?: boolean; timeoutMs?: number } = {}, access: LoginAccess = {}): Promise<LoginResult> {
  guard(access);
  const native = account.native!; const { client } = await (access.host ?? ensureNativeHost)(native);
  guard(access);
  const configurationKey = nativeConfigurationKey(native);
  const existing = operations.get(account.accountId);
  if (existing && existing.configurationKey === configurationKey) {
    const refreshed = await client.operation(native.accountId, existing.operation.id);
    guard(access); validateOperation(refreshed, native.accountId, existing.operation.id);
    existing.operation = refreshed;
    existing.client = client;
    if (!terminal.has(existing.operation.state)) return nativeLoginWait(account, Math.min(options.timeoutMs ?? 30000, 30000), true, access);
  }
  guard(access);
  const operation = await client.login(native.accountId, { allowQr: true, forceQr: options.force === true,
    timeoutSeconds: Math.max(10, Math.min(900, Math.ceil((options.timeoutMs ?? 300000) / 1000))), computerName: "agent-wechat" }, randomUUID());
  validateOperation(operation, native.accountId);
  operations.set(account.accountId, { client, accountId: native.accountId, configurationKey, operation });
  guard(access);
  return nativeLoginWait(account, Math.min(options.timeoutMs ?? 30000, 30000), true, access);
}
export async function nativeLoginWait(account: ResolvedWeChatAccount, timeoutMs = 30000, stopAtQr = false, access: LoginAccess = {}): Promise<LoginResult> {
  guard(access);
  const active = operations.get(account.accountId);
  if (!active || active.configurationKey !== nativeConfigurationKey(account.native!)) return { connected: false, state: "idle", message: "No native login operation; start login first" };
  const deadline = Date.now() + Math.max(1000, Math.min(timeoutMs, 300000));
  while (Date.now() < deadline) {
    guard(access);
    const refreshed = await active.client.operation(active.accountId, active.operation.id);
    guard(access); validateOperation(refreshed, active.accountId, active.operation.id);
    active.operation = refreshed;
    const state = active.operation.state;
    const result = { connected: state === "succeeded", state, operationId: active.operation.id };
    if (state === "succeeded") return { ...result, message: "Native login succeeded" };
    if (terminal.has(state)) return { ...result, message: `Native login ${state}` };
    if (stopAtQr && state === "awaiting_approval") return { ...result, message: "Approve native login on your phone" };
    if (stopAtQr && (active.operation.result as any)?.qrAvailable === true) {
      const generation=(active.operation.result as any)?.qrGeneration;
      const png = await active.client.qr(active.accountId, active.operation.id, typeof generation === "number" ? generation : undefined);
      guard(access);
      return { ...result, qrDataUrl: `data:image/png;base64,${Buffer.from(png).toString("base64")}`, message: "Scan the white-background native QR and approve login" };
    }
    await delay(Math.min(500, Math.max(0, deadline - Date.now())), undefined, { signal: access.signal });
  }
  guard(access);
  return { connected: false, state: active.operation.state, operationId: active.operation.id, message: "Native login remains pending; wait on the same operation rather than starting a replacement" };
}
export async function sendNativeText(account: ResolvedWeChatAccount, threadId: string, text: string, key = randomUUID()): Promise<NativeOutboundDelivery> {
  const native = account.native!; const { client } = await ensureNativeHost(native);
  const send = await submitNativeText(client, native.accountId, threadId, text, key);
  return nativeOutboundDelivery(client, native.accountId, threadId, send.localMessageId ? [send.localMessageId] : [],
    { sendId: send.id, state: send.state, serverIds: send.serverId ? [send.serverId] : undefined });
}

export function createNativeLoginTool(account: ResolvedWeChatAccount, dependencies: { host?: typeof ensureNativeHost; context?: OpenClawPluginToolContext } = {}): ChannelAgentTool & { catalogMode: "direct-only" } {
  const actions = ["status", "start", "wait", "disconnect", "logout", "forget"];
  const configurationKey = nativeConfigurationKey(account.native!);
  return {
    label: "WeChat Login", name: "wechat_login",
    catalogMode: "direct-only",
    description: "Check WeChat status, start or resume native login, or wait for the same login operation. Show the returned QR image to the user to scan and approve on their phone; do not generate a replacement QR. Repeated start reuses an active operation. For a new operation, force uses QR instead of saved trust. timeoutMs bounds the wait. Disconnect retains saved trust. Logout reports its durable native outcome; it does not promise saved-device revocation. Forget requires confirm=true and removes local authentication while retaining history.",
    parameters: { type: "object", additionalProperties: false, properties: {
      action: { type: "string", enum: actions },
      force: { type: "boolean", description: "With start, use QR instead of saved trust for a new login. An active login is still reused." },
      timeoutMs: { type: "integer", minimum: 1000, maximum: 300000, description: "Wait bound in milliseconds; defaults to 30000. Start returns as soon as a QR is available." },
      confirm: { type: "boolean", description: "Required true for forget." },
    }, required: ["action"] },
    execute: async (_id: string, params: unknown, signal?: AbortSignal) => {
      if (!params || typeof params !== "object" || Array.isArray(params)) throw Error("Expected login arguments");
      const args = params as Record<string, unknown>;
      if (Object.keys(args).some(key => !["action", "force", "timeoutMs", "confirm"].includes(key)) || !actions.includes(args.action as string)
        || (args.force !== undefined && typeof args.force !== "boolean") || (args.confirm !== undefined && typeof args.confirm !== "boolean")
        || (args.timeoutMs !== undefined && (!Number.isSafeInteger(args.timeoutMs) || (args.timeoutMs as number) < 1000 || (args.timeoutMs as number) > 300000))) throw Error("Invalid native login arguments");
      if (args.action === "forget" && args.confirm !== true) throw Error("Forgetting native credentials requires confirm=true");
      const access: LoginAccess = { host: dependencies.host, signal, assertCurrent: () => {
        dependencies.context?.assertInvocationCurrent?.();
        if (!dependencies.context) return;
        const context = dependencies.context;
        const config = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
        const current = config && resolveWeChatAccount(config as Record<string, unknown>);
        if (!current?.enabled || !current.native || nativeConfigurationKey(current.native) !== configurationKey) throw Error("Native login configuration changed; invoke the tool again");
      } };
      guard(access);
      if (args.action === "start" || args.action === "wait") {
        const result = args.action === "start"
          ? await nativeLoginStart(account, { force: args.force as boolean | undefined, timeoutMs: args.timeoutMs as number | undefined }, access)
          : await nativeLoginWait(account, args.timeoutMs as number | undefined, false, access);
        const content: Array<{ type: "text"; text: string } | { type: "image"; data: string; mimeType: string }> = [{ type: "text", text: result.message }];
        if (result.qrDataUrl) content.push({ type: "image", data: result.qrDataUrl.split(",")[1], mimeType: "image/png" });
        return { content, details: { connected: result.connected, state: result.state, operationId: result.operationId } };
      }
      const native = account.native!; const { client } = await (access.host ?? ensureNativeHost)(native);
      guard(access);
      const result = args.action === "status" ? await client.account(native.accountId)
        : args.action === "disconnect" ? await client.disconnect(native.accountId)
        : args.action === "logout" ? await client.logout(native.accountId, randomUUID())
        : args.action === "forget" && args.confirm === true ? await client.forgetCredentials(native.accountId, randomUUID(), true)
        : (() => { throw new Error("Unknown native login action"); })();
      if (args.action !== "status") operations.delete(account.accountId);
      guard(access);
      return { content: [{ type: "text" as const, text: JSON.stringify(result) }], details: result };
    },
  };
}

export function registerNativeLoginTool(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => {
    const config = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
    const account = config && resolveWeChatAccount(config as Record<string, unknown>);
    return account?.enabled && account.native ? [createNativeLoginTool(account, { context })] : [];
  } }, { names: ["wechat_login"] });
}
