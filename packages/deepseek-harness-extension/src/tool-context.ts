/**
 * M3 trusted tool context — DESIGN.md §0 invariant 7 and §4.
 *
 * Every tool execution must derive its identity from the host-supplied
 * `exec.agent.id`, never from model arguments. This module resolves an Agent
 * id to the durable route binding created by the session router, derives the
 * owner flag from configuration (not from the model), and revalidates the
 * grant before each mutation so a stale async gap cannot widen authority.
 *
 * Pure module: the route lookup and config are injected, so this is fully
 * testable without host services.
 */

import {
  asAccountId,
  asChatJid,
  asRouteKey,
  type AccountId,
  type ChatJid,
  type RouteBinding,
  type ToolGrant,
  type ToolPrincipal,
  type ToolScope,
} from './contracts.ts';

/** Which tools a principal may use. Defaults are closed. */
export interface ToolGrants {
  /** Tools usable in the thread that triggered the call (always on). */
  readonly currentChat: readonly string[];
  /** Account-wide tools for an owner-bound DM (default: empty). */
  readonly owner: readonly string[];
  /** Cross-account archive tools (default: empty). */
  readonly archive: readonly string[];
  /** Call-control tools (default: empty). */
  readonly call: readonly string[];
}

/** Default grants mirror OpenClaw: only current-chat tools ship enabled. */
export const DEFAULT_GRANTS: ToolGrants = {
  currentChat: [
    'wechat_history',
    'wechat_search',
    'wechat_message',
    'wechat_media',
    'wechat_recall',
    'wechat_send_share',
  ],
  // Owner-scope tools are armed by the owner binding itself (ownerAllowFrom in
  // config is the explicit authorization; default empty → nobody is owner).
  owner: [
    'wechat_stickers',
    'wechat_login',
    'wechat_contacts',
    'wechat_contact_request',
    'wechat_group_info',
  ],
  // Cross-account archive and call tools stay default-deny (DESIGN.md §4).
  archive: [],
  call: [],
};

export interface ToolContextConfig {
  /** Exact wxid values that denote the account owner. */
  readonly ownerAllowFrom: readonly string[];
  readonly grants: ToolGrants;
}

export interface ToolContextDeps {
  /** Resolve the durable route binding for a live Agent id. */
  readonly bindingForAgent: (agentId: string) => Promise<RouteBinding | undefined>;
  readonly config: ToolContextConfig;
  /** Monotonic config generation; snapshotted per resolution. */
  readonly generation: () => number;
}

export interface TrustedToolContext {
  /** Resolve a principal, or undefined when the Agent has no WeChat route. */
  forAgent(agentId: string): Promise<ToolPrincipal | undefined>;
  /** Resolve or throw `ToolAccessError` (tool not available for this Agent). */
  require(agentId: string, toolName: string): Promise<ToolPrincipal>;
  /**
   * Re-check the grant right before a mutation. Throws if the route binding
   * disappeared or the config generation moved in a way that revokes access.
   */
  revalidate(principal: ToolPrincipal, toolName: string): Promise<ToolPrincipal>;
  /** Whether a tool name is granted to the principal's scope. */
  allows(principal: ToolPrincipal, toolName: string): boolean;
}

/** Raised when a tool is called without the authority to do so. */
export class ToolAccessError extends Error {
  readonly toolName: string;
  constructor(message: string, toolName: string) {
    super(message);
    this.name = 'ToolAccessError';
    this.toolName = toolName;
  }
}

const isGroupChat = (chatJid: ChatJid): boolean => chatJid.endsWith('@chatroom');

/** Owner binding: the thread must be the owner's DM (ChatJid is their wxid). */
const isOwnerChat = (binding: RouteBinding, ownerAllowFrom: readonly string[]): boolean =>
  !isGroupChat(binding.chatJid) && ownerAllowFrom.includes(binding.chatJid);

/** Build the complete grant list for one principal. */
function grantsFor(principalOwner: boolean, grants: ToolGrants): readonly ToolGrant[] {
  const list: ToolGrant[] = [{ scope: 'current-chat', tools: [...grants.currentChat] }];
  if (principalOwner) {
    list.push({ scope: 'owner', tools: [...grants.owner] });
    list.push({ scope: 'archive', tools: [...grants.archive] });
    list.push({ scope: 'call', tools: [...grants.call] });
  }
  return list;
}

const scopeTools = (grants: readonly ToolGrant[], scope: ToolScope): readonly string[] =>
  grants.find((grant) => grant.scope === scope)?.tools ?? [];

export function createTrustedToolContext(deps: ToolContextDeps): TrustedToolContext {
  const generation = deps.generation;

  async function forAgent(agentId: string): Promise<ToolPrincipal | undefined> {
    const binding = await deps.bindingForAgent(agentId);
    if (binding === undefined) return undefined;
    const owner = isOwnerChat(binding, deps.config.ownerAllowFrom);
    return {
      agentId,
      routeKey: asRouteKey(binding.routeKey),
      accountId: asAccountId(String(binding.accountId)),
      chatJid: asChatJid(String(binding.chatJid)),
      isOwner: owner,
      grants: grantsFor(owner, deps.config.grants),
      generation: generation(),
    };
  }

  function allows(principal: ToolPrincipal, toolName: string): boolean {
    const scopes: readonly ToolScope[] = principal.isOwner
      ? ['current-chat', 'owner', 'archive', 'call']
      : ['current-chat'];
    for (const scope of scopes) {
      if (scopeTools(principal.grants, scope).includes(toolName)) return true;
    }
    return false;
  }

  async function require(agentId: string, toolName: string): Promise<ToolPrincipal> {
    const principal = await forAgent(agentId);
    if (principal === undefined) {
      throw new ToolAccessError(
        `${toolName}: this Agent is not bound to a WeChat chat`,
        toolName,
      );
    }
    if (!allows(principal, toolName)) {
      throw new ToolAccessError(`${toolName}: not granted to this Agent`, toolName);
    }
    return principal;
  }

  async function revalidate(principal: ToolPrincipal, toolName: string): Promise<ToolPrincipal> {
    const current = await forAgent(principal.agentId);
    if (current === undefined) {
      throw new ToolAccessError(`${toolName}: the WeChat route was removed`, toolName);
    }
    if (!allows(current, toolName)) {
      throw new ToolAccessError(`${toolName}: access was revoked`, toolName);
    }
    // Owner status must still hold (e.g. config changed while awaiting I/O).
    if (principal.isOwner && !current.isOwner) {
      throw new ToolAccessError(`${toolName}: owner binding no longer holds`, toolName);
    }
    return current;
  }

  return { forAgent, require, revalidate, allows };
}
