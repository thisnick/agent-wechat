import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import { TransformStream as NodeTransformStream } from "node:stream/web";
import type { OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import { ensureNativeHost } from "./native-host.ts";
import type { WeChatNativeConfig } from "./types.ts";

const MAX_BODY = 128 * 1024 * 1024;
function fail(res: ServerResponse, status: number, code: string): void { if (!res.headersSent) { res.removeHeader("content-length"); res.removeHeader("content-range"); res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" }); res.end(JSON.stringify({ error: { code } })); } else res.destroy(); }
async function drain(res: ServerResponse, signal: AbortSignal): Promise<void> {
  if (res.destroyed || signal.aborted) return;
  await new Promise<void>(resolve => {
    const done = () => { res.off("drain", done); res.off("close", done); res.off("error", done); signal.removeEventListener("abort", done); resolve(); };
    res.once("drain", done); res.once("close", done); res.once("error", done); signal.addEventListener("abort", done, { once: true });
  });
}
export function facadePrefix(config: WeChatNativeConfig): string {
  const prefix = config.gatewayFacade?.prefix ?? "/plugins/agent-wechat/native";
  if (!/^\/plugins\/[a-zA-Z0-9_-]+(?:\/[a-zA-Z0-9_-]+)+$/.test(prefix) || prefix.length > 200) throw new Error("Native facade requires a namespaced /plugins/... prefix");
  return prefix;
}
/** The real Gateway authenticates this handler before invoking it. This proxy
 * additionally restricts scope to the selected daemon account and never exposes
 * daemon-wide token/account creation routes through a broader owner credential. */
export function createNativeFacade(config: WeChatNativeConfig, dependencies: { endpoint?: string; token?: string; fetch?: typeof fetch } = {}) {
  const prefix = facadePrefix(config);
  const active = new Set<AbortController>();
  const handler = async (req: IncomingMessage, res: ServerResponse): Promise<boolean> => {
    const incoming = req.url ?? "/";
    if (!incoming.startsWith(`${prefix}/`)) return false;
    if (/%2f|%5c|%00|(?:^|\/)\.{1,2}(?:\/|$)/i.test(incoming.split("?")[0])) { fail(res, 400, "invalid_facade_path"); return true; }
    const relative = incoming.slice(prefix.length + 1); const parsed = new URL(relative, "http://facade.invalid/");
    const pieces = parsed.pathname.split("/").filter(Boolean);
    let accountId; try { accountId = pieces[2] ? decodeURIComponent(pieces[2]) : undefined; } catch { fail(res, 400, "invalid_facade_path"); return true; }
    if (pieces[0] !== "v1" || pieces[1] !== "accounts" || accountId !== config.accountId) { fail(res, 403, "facade_account_forbidden"); return true; }
    if (!["GET", "HEAD", "POST", "PATCH", "DELETE"].includes(req.method ?? "")) { fail(res, 405, "method_not_allowed"); return true; }
    const abort = new AbortController(); active.add(abort); const cancel = () => abort.abort(); req.once("aborted", cancel); res.once("close", cancel);
    let tooLarge = false;
    try {
      const host = dependencies.endpoint ? undefined : await ensureNativeHost(config);
      const headers = new Headers();
      for (const name of ["content-type", "accept", "range", "if-none-match", "if-range", "last-event-id", "idempotency-key"]) { const value = req.headers[name]; if (typeof value === "string") headers.set(name, value); }
      const length = req.headers["content-length"]; if (length && (!/^\d+$/.test(length) || Number(length) > MAX_BODY)) { fail(res, 413, "payload_too_large"); return true; }
      let size = 0;
      const body = ["GET", "HEAD"].includes(req.method ?? "") ? undefined : Readable.toWeb(req).pipeThrough(new NodeTransformStream({ transform(chunk, controller) { size += chunk.byteLength; if (size > MAX_BODY) { tooLarge = true; throw new Error("payload_too_large"); } controller.enqueue(chunk); } }));
      const init = { method: req.method, headers, body: body as unknown as BodyInit | undefined, duplex: "half", signal: abort.signal, redirect: "error" } as RequestInit;
      let response: Response;
      if (host) response = await host.forward(relative, init);
      else {
        // Explicit transport fixtures exercise the HTTP boundary independently.
        headers.set("Authorization", `Bearer ${dependencies.token}`);
        response = await (dependencies.fetch ?? fetch)(new URL(relative, dependencies.endpoint!), init);
      }
      res.statusCode = response.status;
      for (const name of ["content-type", "content-length", "content-range", "accept-ranges", "etag", "cache-control", "content-disposition", "x-content-type-options"]) { const value = response.headers.get(name); if (value) res.setHeader(name, value); }
      res.setHeader("x-content-type-options", "nosniff");
      const reader = response.body?.getReader(); if (!reader) { res.end(); return true; }
      const sse = response.headers.get("content-type")?.startsWith("text/event-stream"); let emitted = 0;
      try { while (!abort.signal.aborted) { const { value, done } = await reader.read(); if (done) break; emitted += value.length; if (!sse && emitted > MAX_BODY) throw new Error("upstream_response_too_large"); if (!res.write(value)) await drain(res, abort.signal); } } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
      res.end();
    } catch { if (!abort.signal.aborted) fail(res, tooLarge ? 413 : 502, tooLarge ? "payload_too_large" : "native_facade_unavailable"); else if (!res.destroyed) res.destroy(); }
    finally { active.delete(abort); req.off("aborted", cancel); res.off("close", cancel); }
    return true;
  };
  return Object.assign(handler, { stop: () => { for (const request of active) request.abort(); active.clear(); } });
}
export function registerNativeFacade(api: OpenClawPluginApi, config: WeChatNativeConfig): () => void {
  if (!config.gatewayFacade?.enabled) return () => {};
  const handler = createNativeFacade(config);
  api.registerHttpRoute({ path: facadePrefix(config), match: "prefix", auth: "gateway", gatewayRuntimeScopeSurface: "trusted-operator", handler });
  return handler.stop;
}
