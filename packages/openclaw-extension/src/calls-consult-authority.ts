import type { OpenClawPluginApi, PluginRuntime } from "openclaw/plugin-sdk/core";

type Grant = { runtime: PluginRuntime; sessionKey: string; assertCurrent(): void; abort(reason: unknown): void };
const installed = new WeakSet<PluginRuntime>();
const active = new Map<string, Grant>();
const prefix = "wechat-conference-";

/** A denial-only hook: host tool restrictions and approvals always still apply. */
export function installCallConsultAuthority(api: OpenClawPluginApi): void {
  if (typeof api.registerTrustedToolPolicy !== "function") return;
  const runtime = api.runtime;
  api.registerTrustedToolPolicy({ id: "wechat-conference-consultation", description: "Recheck the exact conference consultation's current group permission before each tool.",
    evaluate(event, context) {
      const runId = context.runId ?? event.runId;
      if (!runId?.startsWith(prefix)) return;
      const grant = active.get(runId);
      try {
        if (!grant || grant.runtime !== runtime || context.sessionKey !== grant.sessionKey
          || (context.runId && event.runId && context.runId !== event.runId)) throw Error("Conference consultation is no longer active");
        grant.assertCurrent();
      } catch (error) {
        grant?.abort(error);
        return { block: true, blockReason: "Conference consultation authority changed" };
      }
      // Never return an allow decision or broaden the host's normal policy.
      return;
    },
  });
  installed.add(runtime);
}
export function hasCallConsultAuthority(runtime: PluginRuntime): boolean { return installed.has(runtime); }

/** This grant is in-memory, bound to one host instance/run/session and never persisted. */
export function beginCallConsultAuthority(runtime: PluginRuntime, runId: string, sessionKey: string,
  assertCurrent: () => void, abort: (reason: unknown) => void): () => void {
  if (!installed.has(runtime) || !runId.startsWith(prefix) || active.has(runId) || active.size >= 128) throw Error("Conference consultation authority unavailable");
  assertCurrent();
  active.set(runId, { runtime, sessionKey, assertCurrent, abort });
  const timer = setInterval(() => { try { assertCurrent(); } catch (error) { abort(error); } }, 100);
  timer.unref();
  return () => { clearInterval(timer); active.delete(runId); };
}
