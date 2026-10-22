import { randomUUID } from "node:crypto";
import type { NativeOperation, NativeWeChatClient } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import { nativeOutboundDelivery, submitNativeText, type NativeOutboundDelivery } from "./native-send.ts";
import type { ResolvedWeChatAccount } from "./types.ts";
const operations = new Map<string, { client: NativeWeChatClient; accountId: string; operation: NativeOperation }>();
const terminal = new Set(["succeeded", "failed", "cancelled", "expired", "rejected", "needs_login"]);
export async function nativeLoginStart(account: ResolvedWeChatAccount, options: { force?: boolean; timeoutMs?: number } = {}) {
  const native = account.native!; const { client } = await ensureNativeHost(native);
  const existing = operations.get(account.accountId);
  if (existing && existing.accountId === native.accountId && !options.force) {
    existing.operation = await client.operation(native.accountId, existing.operation.id);
    if (!terminal.has(existing.operation.state)) return nativeLoginWait(account.accountId, Math.min(options.timeoutMs ?? 30000, 30000), true);
  }
  const operation = await client.login(native.accountId, { allowQr: true, forceQr: options.force === true,
    timeoutSeconds: Math.max(10, Math.min(900, Math.ceil((options.timeoutMs ?? 300000) / 1000))), computerName: "agent-wechat" });
  operations.set(account.accountId, { client, accountId: native.accountId, operation });
  return nativeLoginWait(account.accountId, Math.min(options.timeoutMs ?? 30000, 30000), true);
}
export async function nativeLoginWait(accountId: string, timeoutMs = 30000, stopAtQr = false): Promise<{ connected: boolean; qrDataUrl?: string; message: string }> {
  const active = operations.get(accountId); if (!active) return { connected: false, message: "No native login operation; start login first" };
  const deadline = Date.now() + Math.max(1000, Math.min(timeoutMs, 300000));
  while (Date.now() < deadline) {
    active.operation = await active.client.operation(active.accountId, active.operation.id);
    const state = active.operation.state;
    if (state === "succeeded") return { connected: true, message: "Native login succeeded" };
    if (terminal.has(state)) return { connected: false, message: `Native login ${state}` };
    if (stopAtQr && (active.operation.result as any)?.qrAvailable === true) {
      const generation=(active.operation.result as any)?.qrGeneration;
      const png = await active.client.qr(active.accountId, active.operation.id, typeof generation === "number" ? generation : undefined);
      return { connected: false, qrDataUrl: `data:image/png;base64,${Buffer.from(png).toString("base64")}`, message: "Scan the white-background native QR and approve login" };
    }
    if (stopAtQr && state === "awaiting_approval") return { connected: false, message: "Approve native login on your phone" };
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  return { connected: false, message: "Native login remains pending; wait on the same operation rather than starting a replacement" };
}
export async function sendNativeText(account: ResolvedWeChatAccount, threadId: string, text: string, key = randomUUID()): Promise<NativeOutboundDelivery> {
  const native = account.native!; const { client } = await ensureNativeHost(native);
  const send = await submitNativeText(client, native.accountId, threadId, text, key);
  return nativeOutboundDelivery(client, native.accountId, threadId, send.localMessageId ? [send.localMessageId] : [],
    { sendId: send.id, state: send.state, serverIds: send.serverId ? [send.serverId] : undefined });
}

export function createNativeLoginTool(account: ResolvedWeChatAccount) {
  return {
    label: "WeChat Login", name: "wechat_login",
    description: "Inspect/start native login, disconnect retaining trust, or request durable logout. Logout commits local inhibition and reports its native server outcome separately; it does not promise saved-device revocation. Explicitly confirmed forget removes local authentication while retaining history. A QR is returned as an image when available.",
    parameters: { type: "object", properties: { action: { type: "string", enum: ["status", "start", "disconnect", "logout", "forget"] }, force: { type: "boolean" }, confirm: { type: "boolean" } }, required: ["action"] },
    execute: async (_id: string, params: unknown) => {
      const args = params as { action: string; force?: boolean; confirm?: boolean };
      const native = account.native!; const { client } = await ensureNativeHost(native);
      if (args.action === "start") {
        const result = await nativeLoginStart(account, { force: args.force });
        const content: Array<{ type: "text"; text: string } | { type: "image"; data: string; mimeType: string }> = [{ type: "text", text: result.message }];
        if (result.qrDataUrl) content.push({ type: "image", data: result.qrDataUrl.split(",")[1], mimeType: "image/png" });
        return { content, details: { connected: result.connected } };
      }
      const result = args.action === "status" ? await client.account(native.accountId)
        : args.action === "disconnect" ? await client.disconnect(native.accountId)
        : args.action === "logout" ? await client.logout(native.accountId, randomUUID())
        : args.action === "forget" && args.confirm === true ? await client.forgetCredentials(native.accountId, randomUUID(), true)
        : (() => { throw new Error("Unknown native login action"); })();
      return { content: [{ type: "text" as const, text: JSON.stringify(result) }], details: result };
    },
  };
}
