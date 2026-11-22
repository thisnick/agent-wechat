/**
 * Static plugin surface for interface listings (`pnpm interfaces`).
 *
 * Mirrors the OpenClaw listing's syntheticSetup pattern: tool factories are
 * constructed with a synthetic deps object that is never invoked — name,
 * description and parameters are declared statically on every definition, so
 * no native client, tool context or media sink is touched. Guarded by
 * `src/tools/catalog.test.ts` so a factory that starts reading deps eagerly
 * breaks here instead of in CI's listing job.
 */

import { DEFAULT_GRANTS } from '../tool-context.ts';
import { accountTools } from './account.ts';
import { callTools } from './call.ts';
import { currentChatTools } from './chat.ts';
import { ownerTools } from './owner.ts';
import type { ToolDeps } from './shared.ts';

export interface CatalogToolEntry {
  readonly name: string;
  readonly description: string;
  readonly parameters: Record<string, unknown>;
  /** Visibility rule surfaced in the listing (who can see the tool model-side). */
  readonly visibility:
    | 'enabled by default'
    | 'owner DM only'
    | 'opt-in via tools.grants.archive'
    | 'opt-in via calls config';
}

export function wechatToolCatalog(): CatalogToolEntry[] {
  const syntheticDeps = undefined as unknown as ToolDeps;
  const currentChat = new Set(DEFAULT_GRANTS.currentChat);
  const owner = new Set(DEFAULT_GRANTS.owner);
  return [
    ...currentChatTools(syntheticDeps),
    ...accountTools(syntheticDeps),
    ...ownerTools(syntheticDeps),
    // The call tool appears in the catalog even when calls are unconfigured;
    // its row documents that it only arms with a calls config.
    ...callTools(syntheticDeps),
  ].map((tool) => ({
    name: tool.name,
    description: tool.description,
    parameters: tool.parameters,
    visibility: tool.name === 'wechat_call'
      ? 'opt-in via calls config'
      : currentChat.has(tool.name)
        ? 'enabled by default'
        : owner.has(tool.name)
          ? 'owner DM only'
          : 'opt-in via tools.grants.archive',
  }));
}

/**
 * Plain-data description of the plugin configuration surface. Hand-maintained
 * next to the schemastery `Config` in index.ts; the interface listing renders
 * it into `interfaces/dsh/config.txt`.
 */
export interface CatalogConfigEntry {
  readonly path: string;
  readonly attributes: readonly string[];
  readonly description: string;
}

export const dshConfigShape: readonly CatalogConfigEntry[] = [
  { path: 'native.mode', attributes: ['values external|managed', 'default external'], description: 'Connect to an external daemon or spawn a managed one' },
  { path: 'native.accountId', attributes: ['string', 'default default'], description: 'Native account id this plugin instance serves' },
  { path: 'native.endpoint', attributes: ['string', 'default http://127.0.0.1:6790'], description: 'Native daemon HTTP endpoint (loopback http or https)' },
  { path: 'native.executable', attributes: ['string'], description: 'Daemon executable path for managed mode' },
  { path: 'native.serverConfig', attributes: ['string'], description: 'Daemon server config path for managed mode' },
  { path: 'native.dataDir', attributes: ['string'], description: 'Daemon data directory for managed mode' },
  { path: 'native.consumerStateDir', attributes: ['string'], description: 'Directory for consumer cursor state' },
  { path: 'native.tokenCredentialRef', attributes: ['string', 'default WECHAT_DAEMON_TOKEN'], description: 'Credential reference holding the native daemon token' },
  { path: 'native.readinessTimeoutMs', attributes: ['number', 'default 15000'], description: 'Managed daemon readiness timeout' },
  { path: 'native.stopTimeoutMs', attributes: ['number', 'default 8000'], description: 'Managed daemon stop timeout' },
  { path: 'dmPolicy', attributes: ['values disabled|allowlist|open', 'default disabled'], description: 'Direct-message admission policy' },
  { path: 'groupPolicy', attributes: ['values disabled|allowlist', 'default disabled'], description: 'Group-chat admission policy' },
  { path: 'dmAllowFrom', attributes: ['string[]', 'default []'], description: 'Allowed DM sender ids (supports *)' },
  { path: 'groupAllowFrom', attributes: ['string[]', 'default []'], description: 'Allowed group chat ids (supports *)' },
  { path: 'groupSenderAllowFrom', attributes: ['string[]', 'default []'], description: 'Allowed group sender ids (supports *)' },
  { path: 'requireMentionInGroups', attributes: ['boolean', 'default true'], description: 'Require a real mention before answering in groups' },
  { path: 'readPolicy', attributes: ['values on_inbound|never|on_model_dispatch', 'default on_inbound'], description: 'Reserved read-receipt policy; the native daemon exposes no read API today, so this has no effect yet' },
  { path: 'historyLimit', attributes: ['number', 'default 50'], description: 'Maximum catch-up messages per reconnect window' },
  { path: 'tools.ownerAllowFrom', attributes: ['string[]', 'default []'], description: 'Exact DM wxids treated as the account owner (enables owner tools; archive stays deny-by-default)' },
  { path: 'calls', attributes: ['object'], description: 'Calling configuration; absent or voice.enabled!==true keeps calls fail-closed offline' },
  { path: 'calls.voice.mode', attributes: ['values bidi|agent', 'default bidi'], description: 'agent runs consultation turns per utterance; bidi needs realtime' },
  { path: 'calls.voice.agentId', attributes: ['string', 'required'], description: 'Harness Agent id that carries call consultations' },
  { path: 'calls.voice.inboundPolicy', attributes: ['values disabled|allowlist', 'default disabled'], description: 'Inbound call admission; allowlist answers only listed callers' },
  { path: 'calls.voice.inboundAllowFrom', attributes: ['string[]', 'default []'], description: 'Exact caller wxids that may call in' },
  { path: 'calls.voice.outboundPolicy', attributes: ['values disabled|allowlist', 'default disabled'], description: 'Coordinator-dialed calls require allowlist' },
  { path: 'calls.voice.outboundAllowTo', attributes: ['string[]', 'default []'], description: 'Exact callees the coordinator may dial' },
  { path: 'calls.voice.outboundToolAllowFrom', attributes: ['string[]', 'default []'], description: 'Owner DMs allowed to invoke wechat_call mutations' },
  { path: 'calls.voice.toolPolicy', attributes: ['values none|safe-read-only|owner', 'default none'], description: 'Tool scope for call consultation sessions (owner needs an exact owner match)' },
  { path: 'calls.voice.allowConsultSessionPersistence', attributes: ['boolean', 'default false'], description: 'Required true for any non-none tool policy' },
  { path: 'calls.voice.sessionScope', attributes: ['values per-contact|per-call', 'default per-contact'], description: 'Call session key scope' },
  { path: 'calls.voice.maxDurationSeconds', attributes: ['number 30..3600', 'default 3600'], description: 'Hard call duration cap' },
  { path: 'calls.voice.consultation', attributes: ['object'], description: '{ timeoutMs 1000..120000 (30000), summary boolean (true), recentMessages 0..50 (10) }' },
  { path: 'calls.voice.realtime', attributes: ['object'], description: 'OpenAI realtime: provider/model/voice/instructions/apiKeyCredentialRef/agentContext/recentContext/consultPolicy/fastContext' },
  { path: 'calls.voice.transcription', attributes: ['object'], description: 'STT provider/model/language (openai)' },
  { path: 'calls.voice.conferences.groups', attributes: ['object'], description: 'Per-group rules keyed by @chatroom id or *: autoJoin/autoJoinAllowFrom/toolPolicy/ownerInviterToolPolicy/sessionKey' },
  { path: 'gatewayFacade', attributes: ['boolean', 'default false'], description: 'Expose the local gateway facade (M6; default off)' },
];
