/**
 * @agent-wechat/deepseek-harness-extension — DSH WeChat channel plugin.
 *
 * Entry contract: `inject` + `Config` + `apply(ctx, config)`. The real runtime
 * is assembled by the host binding (DESIGN.md §2 layering): storage, reply
 * projection, credentials, Agent routing, native bridge and the durable
 * outbox. Everything is owned by the plugin fiber's effect so hot-unload
 * closes the domain, unregisters the projection, stops the consumers and
 * removes listeners. All defaults are closed (no inbound sends until a token
 * credential and at least one DM/group policy are configured).
 */
import z from '@deepseek-ai/schemastery';
import type { Context } from '@deepseek-ai/cordis';
import { debugTrace } from './src/debug-trace.ts';
import { unwrapVolatileConfig } from './src/config-unwrap.ts';
import {
  bindWechatHost,
  type HostBindingResult,
  type WechatConfigView,
} from './src/host-binding.ts';

export const inject = [
  'storageDomain',
  'sessionProjections',
  'credentials',
  'agents',
  'sessions',
  'tools',
  'attachments',
  'agentDefaultModel',
] as const;

// `.volatile()` marks a field editable from the host settings UI (settings
// forms project ONLY volatile subtrees; an entry with no volatile field never
// appears in `settings.describe` and its form reads as unavailable).
export const Config = z
  .object({
    native: z
      .object({
        mode: z.union(['external', 'managed']).default('external').volatile(),
        accountId: z.string().default('default').volatile(),
        endpoint: z.string().default('http://127.0.0.1:6790').volatile(),
        executable: z.string(),
        serverConfig: z.string(),
        dataDir: z.string(),
        consumerStateDir: z.string(),
        tokenCredentialRef: z.string().default('WECHAT_DAEMON_TOKEN').volatile(),
        readinessTimeoutMs: z.number().default(15000),
        stopTimeoutMs: z.number().default(8000),
      })
      .default({}),
    dmPolicy: z.union(['disabled', 'allowlist', 'open']).default('disabled').volatile(),
    groupPolicy: z.union(['disabled', 'allowlist']).default('disabled').volatile(),
    dmAllowFrom: z.array(z.string()).default([]).volatile(),
    groupAllowFrom: z.array(z.string()).default([]).volatile(),
    groupSenderAllowFrom: z.array(z.string()).default([]).volatile(),
    requireMentionInGroups: z.boolean().default(true).volatile(),
    readPolicy: z.union(['on_inbound', 'never', 'on_model_dispatch']).default('on_inbound'),
    historyLimit: z.number().default(50),
    tools: z
      .object({
        /** Exact wxids that mark a DM as the account owner's (enables owner/archive tools). */
        ownerAllowFrom: z.array(z.string()).default([]).volatile(),
        /** Per-scope tool grant overrides; an omitted scope keeps its default. */
        grants: z
          .object({
            currentChat: z.array(z.string()).volatile(),
            owner: z.array(z.string()).volatile(),
            archive: z.array(z.string()).volatile(),
            call: z.array(z.string()).volatile(),
          })
          .default({}),
      })
      .default({}),
    /**
     * Message push (one-way notifications). Arming `enabled` with at least
     * one target registers the `wechat_push` tool for EVERY DSH session —
     * the configured whitelist is the authorization boundary. Off by default.
     */
    push: z
      .object({
        enabled: z.boolean().default(false).volatile(),
        targets: z.array(z.string()).default([]).volatile(),
      })
      .default({}),
    /**
     * Calls configuration (M5). Validated by resolveWeChatCallsConfig — see
     * README for the full shape (calls.voice.* mirrors the OpenClaw voice
     * calling config; `realtime.apiKeyCredentialRef` names a harness
     * credential that holds the speech provider API key).
     */
    calls: z.any().volatile(),
    gatewayFacade: z.boolean().default(false),
  })
  .description('agent-wechat channel configuration for DeepSeek Harness');

/** Diagnostics hook used by tests and the interface listing. */
let activeBinding: HostBindingResult | undefined;

/**
 * Plugin entry. Fail-soft by contract: a missing native daemon, an unresolved
 * credential or any other activation error parks the channel and retries in the
 * background; it must never throw into the host's plugin loader.
 */
export function apply(ctx: Context, config: unknown): void {
  const cfgView = unwrapVolatileConfig((config ?? {}) as WechatConfigView);
  debugTrace(
    'apply',
    `apply endpoint=${cfgView.native?.endpoint ?? '(default)'} account=${cfgView.native?.accountId ?? '(default)'}`,
  );
  try {
    // The plugin renders its own curated settings page (client/client.js);
    // suppress the host's generic auto-generated form for this entry. Soft-get:
    // an older profile without the settings service must not block loading.
    const settings = (ctx as unknown as { get?: (key: string) => unknown }).get?.('settings') as
      | { configure?: (presentation: { auto: boolean }) => () => void }
      | undefined;
    settings?.configure?.({ auto: false });
  } catch {
    // Presentation is cosmetic; never fail activation for it.
  }
  try {
    activeBinding = bindWechatHost(
      ctx as unknown as Parameters<typeof bindWechatHost>[0],
      config as WechatConfigView,
    );
  } catch (error) {
    // Even a synchronous binding failure must not abort host startup.
    activeBinding = undefined;
    const message = error instanceof Error ? error.message : String(error);
    try {
      (ctx as unknown as { logger?: { warn?: (m: string) => void } }).logger?.warn?.(
        `wechat: plugin disabled (${message})`,
      );
    } catch {
      // Logging must never throw here.
    }
  }
}

/** Diagnostics: current host binding state (safe before async assembly finishes). */
export function getServiceState():
  | { running: boolean; serverId?: string; error?: string; attempts: number }
  | undefined {
  return activeBinding?.getState();
}

/** Test/diagnostic seam returning the binding's readiness promise (never rejects). */
export function getBindingReady(): Promise<void> | undefined {
  return activeBinding?.ready;
}

/** Static tool + config surface for `pnpm interfaces` (see src/tools/catalog.ts). */
export { wechatToolCatalog, dshConfigShape } from './src/tools/catalog.ts';
