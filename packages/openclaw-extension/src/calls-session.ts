import { randomUUID } from "node:crypto";
import type { OpenClawConfig, PluginRuntime } from "openclaw/plugin-sdk/core";
import type { CallSessionScope } from "./calls-config.ts";

export const CALL_SESSION_KEY_PATTERN = "^agent:[a-z0-9][a-z0-9_-]{0,63}:[^:\\s][^\\s]*$";
export const MAX_CALL_SESSION_KEY_LENGTH = 1024;
const keyPattern = new RegExp(CALL_SESSION_KEY_PATTERN);

export type CallParentSession = { key: string; expectedSessionId?: string };
type BoundCallParentSession = { key: string; sessionId: string };

export function isCallSessionKey(value: unknown): value is string {
  return typeof value === "string" && value.length <= MAX_CALL_SESSION_KEY_LENGTH && keyPattern.test(value)
    && !/[\u0000-\u001f\u007f]/.test(value);
}

export async function canonicalCallSessionKey(cfg: OpenClawConfig, agentId: string, key: string): Promise<string | undefined> {
  if (!isCallSessionKey(key)) return;
  const routingPath: string = "openclaw/plugin-sdk/routing";
  const storePath: string = "openclaw/plugin-sdk/session-store-runtime";
  const [routing, store] = await Promise.all([import(routingPath), import(storePath)]) as [
    typeof import("openclaw/plugin-sdk/routing"), typeof import("openclaw/plugin-sdk/session-store-runtime"),
  ];
  const owner = routing.normalizeAgentIdStrict(agentId);
  const parsed = routing.parseAgentSessionKey(key);
  if (!owner.ok || !parsed || parsed.agentId !== owner.value || parsed.rest === "global"
    || parsed.rest.startsWith("agent:") || routing.isIncognitoSessionKey(key)) return;
  // Check ownership before canonicalizing main aliases, which can remap a
  // legacy main alias to another agent or the shared global session.
  const canonical = store.canonicalizeMainSessionAlias({
    cfg, agentId: owner.value, sessionKey: `agent:${parsed.agentId}:${parsed.rest}`,
  });
  const scoped = routing.parseAgentSessionKey(canonical);
  if (isCallSessionKey(canonical) && scoped?.agentId === owner.value
    && scoped.rest !== "global" && !scoped.rest.startsWith("agent:") && !routing.isIncognitoSessionKey(canonical)) return canonical;
}

const CONTACT_ID = /^[A-Za-z0-9_.@-]{1,200}$/;

/** The session a call's consultations run in, decided as OpenClaw's voice-call
 * plugin decides it. per-contact keeps one session for each contact, so the
 * agent remembers earlier calls; read-only callers get a separate session from
 * the owner's, so a later loss of owner status never exposes an owner session.
 * per-call, or a contact ID unfit for a key, gets a new session for each call. */
export function callSessionKey(agentId: string, scope: CallSessionScope, contactId: string, owner: boolean): string {
  if (scope !== "per-call" && CONTACT_ID.test(contactId) && !contactId.endsWith("@chatroom")) {
    return `agent:${agentId}:wechat-call:${contactId.toLowerCase()}${owner ? "" : ":read-only"}`;
  }
  return `agent:${agentId}:wechat-call:${randomUUID()}`;
}

/**
 * The caller's WeChat chat session, which `chat` scope shares with calls: the
 * session their direct messages route to. Only a session of the calling
 * agent qualifies, and for anyone but the owner only one specific to them:
 * with dmScope main, every chat routes to the owner's main session.
 */
export function chatSessionKey(cfg: OpenClawConfig, runtime: PluginRuntime, agentId: string, accountId: string, contactId: string, owner: boolean): string | undefined {
  if (!CONTACT_ID.test(contactId) || contactId.endsWith("@chatroom")) return;
  try {
    const route = runtime.channel?.routing?.resolveAgentRoute?.({ cfg, channel: "agent-wechat", accountId, peer: { kind: "direct", id: contactId } });
    const key = route?.sessionKey;
    if (!key || !isCallSessionKey(key) || route.agentId?.toLowerCase() !== agentId.toLowerCase()) return;
    if (!owner && !key.toLowerCase().includes(contactId.toLowerCase())) return;
    return key;
  } catch { /* Routing failure keeps the call in its own session. */ }
}

/** Whether a call session already exists, i.e. an earlier call created it. */
export function callSessionExists(cfg: OpenClawConfig, runtime: PluginRuntime, agentId: string, key: string): boolean {
  return Boolean(readParentSession(cfg, runtime, agentId, key)?.sessionId);
}

function readParentSession(cfg: OpenClawConfig, runtime: PluginRuntime, agentId: string, key: string) {
  try {
    const session = runtime.agent.session;
    const storePath = session.resolveStorePath(cfg.session?.store, { agentId });
    return session.getSessionEntry({ agentId, storePath, sessionKey: key, readConsistency: "latest" });
  } catch { /* A missing or unavailable parent leaves the new call session isolated. */ }
}

export async function resolveCallParentSession(cfg: OpenClawConfig, runtime: PluginRuntime,
  agentId: string, requested: CallParentSession): Promise<BoundCallParentSession | undefined> {
  const key = await canonicalCallSessionKey(cfg, agentId, requested.key);
  if (!key) return;
  const sessionId = readParentSession(cfg, runtime, agentId, key)?.sessionId;
  if (sessionId && (requested.expectedSessionId === undefined || requested.expectedSessionId === sessionId)) return { key, sessionId };
}

export function currentCallParentKey(cfg: OpenClawConfig, runtime: PluginRuntime, agentId: string,
  bound: BoundCallParentSession | undefined, ownerPeer?: { senderId: string; accountId: string }): string | undefined {
  if (!bound) return;
  const entry = readParentSession(cfg, runtime, agentId, bound.key);
  if (!entry || entry.sessionId !== bound.sessionId) return;
  if (ownerPeer) {
    // The SDK inherits delivery context from a fork. Require a positive
    // same-peer route so it cannot fall through to a foreign thread's route.
    const route = entry.delivery?.kind === "external" ? entry.delivery.context : undefined;
    const { senderId, accountId } = ownerPeer;
    if (route?.channel !== "agent-wechat" || (route.accountId ?? "default") !== accountId
      || route.threadId !== undefined || ![senderId, `wechat:${senderId}`, `agent-wechat:${senderId}`].includes(route.to ?? "")) return;
  }
  return bound.key;
}
