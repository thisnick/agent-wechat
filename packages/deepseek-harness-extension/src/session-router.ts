/**
 * Per-thread Agent lifecycle manager (DESIGN.md §3.2).
 *
 * Pure module: no host (`@deepseek-ai/*`) imports and no live bot. Every
 * external effect is injected through `SessionRouterDeps`, so the whole
 * lifecycle (create / resume / followup / evict / refresh) can be exercised
 * with in-memory fakes.
 *
 * Import note: sibling modules use explicit `.ts` specifiers because this
 * package runs via `node --test --experimental-strip-types src/*.test.ts`
 * (no JS emit). Type-only imports are erased at runtime, so importing the
 * port/table types here never loads the host-backed `storage.ts` module.
 */

import type {
  FollowupMessage,
  RouteBinding,
  RouteKey,
  SessionId,
} from './contracts.ts';
import {
  asAccountId,
  asChatJid,
  asRouteKey,
  asSessionId,
  parseRouteKey,
} from './contracts.ts';
import type { SessionRouterPort } from './ports.ts';
import type { ThreadRecord, WechatTables } from './storage.ts';

/** Setup callback shape shared with `SessionRouterPort.ensure`. */
export type SetupFn = (ctx: unknown, agent: unknown) => void | Promise<void>;

/** Minimal live Agent handle the router owns (maps to DSH `LiveAgent`). */
export interface LiveAgent {
  readonly id: string;
  followup(message: unknown): void;
  dispose(): Promise<void>;
}

/** Options passed to the injected factory for a fresh thread Agent. */
export interface AgentCreateOptions {
  readonly sessionId: string;
  readonly setup?: SetupFn;
  readonly cwd?: string;
  readonly agentOptions?: Record<string, unknown>;
}

/** Options passed to the injected factory for an evicted thread Agent. */
export interface AgentResumeOptions {
  readonly resumeSessionId: string;
  readonly setup?: SetupFn;
  readonly cwd?: string;
  readonly agentOptions?: Record<string, unknown>;
}

/** Injected Agent factory — an adapter binds this to `ctx.agents` in M3. */
export interface AgentFactory {
  create(options: AgentCreateOptions): Promise<LiveAgent>;
  resume(options: AgentResumeOptions): Promise<LiveAgent>;
}

/** `ensure` options. `policyFingerprint` is stored on first create only. */
export interface EnsureOptions {
  readonly cwd?: string;
  readonly agentOptions?: Record<string, unknown>;
  readonly setup?: SetupFn;
  /** Durable policy version fingerprint; defaults to `''` when omitted. */
  readonly policyFingerprint?: string;
}

/** All effects the router needs. Callers keep this module pure by injecting. */
export interface SessionRouterDeps {
  readonly tables: Pick<WechatTables, 'threads'>;
  readonly agents: AgentFactory;
  /** Stable session id derivation, e.g. `wht-<slug>-<stableHash32(routeKey)>`. */
  readonly sessionIdFor: (routeKey: RouteKey) => SessionId;
  /** Adapter from the port's minimal followup message to a host UserMessage. */
  readonly toUserMessage: (message: FollowupMessage, binding: RouteBinding) => unknown;
  /** Injectable clock for deterministic last-activity timestamps. */
  readonly now?: () => number;
}

/** The port plus one extra helper for the future idle-timer wiring. */
export interface SessionRouter extends SessionRouterPort {
  /** `ensure` extended with the durable policy fingerprint option. */
  ensure(routeKey: RouteKey, opts: EnsureOptions): Promise<RouteBinding>;
  /** Dispose every live Agent whose last activity predates the threshold. */
  evictIdle(maxIdleMs: number): Promise<string[]>;
}

interface LiveEntry {
  readonly agent: LiveAgent;
  readonly binding: RouteBinding;
}

/** Widen a durable record (plain strings) to the branded port contract. */
function recordToBinding(record: ThreadRecord): RouteBinding {
  return {
    routeKey: asRouteKey(record.routeKey),
    sessionId: asSessionId(record.sessionId),
    accountId: asAccountId(record.accountId),
    chatJid: asChatJid(record.chatJid),
    policyFingerprint: record.policyFingerprint,
    revision: record.revision,
    lastActivityAt: record.lastActivityAt,
  };
}

/** Narrow a port binding back to the plain-string durable record. */
function bindingToRecord(binding: RouteBinding): ThreadRecord {
  return {
    routeKey: binding.routeKey,
    sessionId: binding.sessionId,
    accountId: binding.accountId,
    chatJid: binding.chatJid,
    policyFingerprint: binding.policyFingerprint,
    revision: binding.revision,
    lastActivityAt: binding.lastActivityAt,
  };
}

/**
 * Build a SessionRouter. No I/O happens here; all state is local to the
 * returned object and every async effect goes through `deps`.
 */
export function createSessionRouter(deps: SessionRouterDeps): SessionRouter {
  const threads = deps.tables.threads;
  const now = deps.now ?? Date.now;

  const live = new Map<string, LiveEntry>();
  const agentToRoute = new Map<string, string>();
  const inflight = new Map<string, Promise<RouteBinding>>();

  function timestamp(): string {
    return new Date(now()).toISOString();
  }

  /** `exactOptionalPropertyTypes`-safe spread of the non-setup passthrough. */
  function passthrough(opts: EnsureOptions): {
    cwd?: string;
    agentOptions?: Record<string, unknown>;
  } {
    return {
      ...(opts.cwd === undefined ? {} : { cwd: opts.cwd }),
      ...(opts.agentOptions === undefined ? {} : { agentOptions: opts.agentOptions }),
    };
  }

  /** The actual create/resume work; only called once per route at a time. */
  async function doEnsure(routeKey: RouteKey, opts: EnsureOptions): Promise<RouteBinding> {
    const stored = threads.get(routeKey);
    const shared = passthrough(opts);
    const setupPart = opts.setup === undefined ? {} : { setup: opts.setup };
    let agent: LiveAgent | undefined;
    let binding: RouteBinding | undefined;

    let resumed = false;
    if (stored !== undefined) {
      // Evicted/restarted route: resume the durable session, never re-create.
      try {
        agent = await deps.agents.resume({
          resumeSessionId: stored.sessionId,
          ...shared,
          ...setupPart,
        });
        // Bump via update so a concurrent writer cannot clobber the revision.
        const updated = await threads.update(routeKey, (current) => ({
          ...current,
          revision: current.revision + 1,
          lastActivityAt: timestamp(),
        }));
        binding = recordToBinding(updated);
        resumed = true;
      } catch {
        // The durable session vanished (deleted or unreadable). Drop the
        // stale binding and start a fresh session so the channel keeps
        // flowing instead of crash-looping on resume.
        await threads.delete(routeKey);
      }
    }
    if (!resumed) {
      const sessionId = deps.sessionIdFor(routeKey);
      const { accountId, chatJid } = parseRouteKey(routeKey);
      agent = await deps.agents.create({
        sessionId,
        ...shared,
        ...setupPart,
      });
      binding = {
        routeKey,
        sessionId,
        accountId,
        chatJid,
        policyFingerprint: opts.policyFingerprint ?? '',
        revision: 1,
        lastActivityAt: timestamp(),
      };
      // Persist only after the Agent was actually created (fail-closed).
      await threads.put(routeKey, bindingToRecord(binding));
    }

    if (agent === undefined || binding === undefined) {
      throw new Error('session router: ensure produced no Agent');
    }
    live.set(routeKey, { agent, binding });
    agentToRoute.set(agent.id, routeKey);
    return binding;
  }

  function ensure(routeKey: RouteKey, opts: EnsureOptions): Promise<RouteBinding> {
    const current = live.get(routeKey);
    if (current !== undefined) return Promise.resolve(current.binding);
    const pending = inflight.get(routeKey);
    if (pending !== undefined) return pending;

    const task = (async () => {
      try {
        return await doEnsure(routeKey, opts);
      } finally {
        inflight.delete(routeKey);
      }
    })();
    inflight.set(routeKey, task);
    return task;
  }

  async function binding(routeKey: RouteKey): Promise<RouteBinding | undefined> {
    const record = threads.get(routeKey);
    return record === undefined ? undefined : recordToBinding(record);
  }

  async function followup(routeKey: RouteKey, message: FollowupMessage): Promise<void> {
    const binding = await ensure(routeKey, {});
    const entry = live.get(routeKey);
    if (entry === undefined) throw new Error(`No live agent for route ${routeKey}`);
    entry.agent.followup(deps.toUserMessage(message, binding));
  }

  async function bindingForAgent(agentId: string): Promise<RouteBinding | undefined> {
    const routeKey = agentToRoute.get(agentId);
    if (routeKey === undefined) return undefined;
    return live.get(routeKey)?.binding;
  }

  async function evict(routeKey: RouteKey): Promise<void> {
    const entry = live.get(routeKey);
    if (entry === undefined) return;
    // Drop the reverse map first so no tool identity can resolve mid-dispose.
    live.delete(routeKey);
    agentToRoute.delete(entry.agent.id);
    await entry.agent.dispose();
    // The durable `threads` record is intentionally kept for later resume.
  }

  async function refresh(routeKey: RouteKey): Promise<RouteBinding> {
    const updated = await threads.update(routeKey, (current) => ({
      ...current,
      revision: current.revision + 1,
      lastActivityAt: timestamp(),
    }));
    const next = recordToBinding(updated);
    const entry = live.get(routeKey);
    if (entry !== undefined) live.set(routeKey, { ...entry, binding: next });
    return next;
  }

  async function evictIdle(maxIdleMs: number): Promise<string[]> {
    const nowMs = now();
    const evicted: string[] = [];
    for (const [key, record] of [...threads.entries()]) {
      const last = Date.parse(record.lastActivityAt);
      if (Number.isNaN(last) || nowMs - last <= maxIdleMs) continue;
      const routeKey = asRouteKey(key);
      if (!live.has(routeKey)) continue;
      await evict(routeKey);
      evicted.push(key);
    }
    return evicted;
  }

  return { binding, ensure, followup, bindingForAgent, evict, refresh, evictIdle };
}
