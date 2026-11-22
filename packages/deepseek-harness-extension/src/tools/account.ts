/**
 * M3 account tools (owner scope) — stickers + login — DESIGN.md §4 rows 3–4.
 *
 * Sticker mutations and login/logout touch the whole account, so both tools
 * require the owner principal (double-checked: trusted context + explicit
 * owner assertion). Every mutation carries an idempotency key and reports
 * submitted operations honestly — a submitted job is never claimed as done.
 */

import type { ToolPrincipal } from '../contracts.ts';
import { ToolAccessError } from '../tool-context.ts';
import {
  defineWechatTool,
  mutationKey,
  pageItems,
  requirePrincipal,
  revalidateForMutation,
  type ToolDeps,
  type WechatToolDefinition,
} from './shared.ts';

/** Non-empty string field validation shared by both tools. */
const requireId = (value: unknown, field: string): string => {
  if (typeof value !== 'string' || value.length === 0 || value.length > 4096) {
    throw new Error(`${field} is required`);
  }
  return value;
};

/** Resolve the principal and assert owner authority for account-wide tools. */
async function requireOwner(
  deps: ToolDeps,
  exec: { readonly agent?: { readonly id: string } | undefined; readonly signal: AbortSignal },
  toolName: string,
): Promise<ToolPrincipal> {
  const principal = await requirePrincipal(deps, exec, toolName);
  if (!principal.isOwner) {
    throw new ToolAccessError(`${toolName}: owner authority required`, toolName);
  }
  return principal;
}

const STICKER_ACTIONS = [
  'list',
  'get',
  'download',
  'prepare',
  'add',
  'move',
  'remove',
  'preview',
] as const;
type StickerAction = (typeof STICKER_ACTIONS)[number];

const LOGIN_ACTIONS = ['status', 'start', 'wait', 'disconnect', 'logout', 'forget'] as const;
type LoginAction = (typeof LOGIN_ACTIONS)[number];

const isTerminalOperation = (state: unknown): boolean =>
  state === 'done' || state === 'failed' || state === 'cancelled';

const sleep = (ms: number, signal: AbortSignal): Promise<void> =>
  new Promise<void>((resolve) => {
    if (signal.aborted) return resolve();
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = (): void => {
      clearTimeout(timer);
      resolve();
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });

// ---------------------------------------------------------------------------
// wechat_stickers
// ---------------------------------------------------------------------------

export function wechatStickersTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_stickers',
    description:
      'Manage this WeChat account sticker library: list packs and custom stickers, prepare images or videos into stickers, add/move/remove custom stickers. Owner only; mutations require an idempotency key and report submitted jobs, not finished results.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: [...STICKER_ACTIONS],
          description: 'Which sticker operation to run.',
        },
        stickerId: { type: 'string', description: 'Custom sticker id (get/download/move/remove).' },
        packId: { type: 'string', description: 'Sticker pack id (future pack actions).' },
        file: { type: 'string', description: 'Path or ref of the source media (prepare).' },
        startMs: { type: 'integer', minimum: 0, description: 'Clip start in ms (prepare).' },
        durationMs: { type: 'integer', minimum: 1, description: 'Clip duration in ms (prepare).' },
        maxEdge: { type: 'integer', minimum: 1, description: 'Max edge in px (prepare).' },
        fps: { type: 'integer', minimum: 1, description: 'Frames per second (prepare).' },
        preparationId: { type: 'string', description: 'Prepared-media job id (add).' },
        messageId: { type: 'string', description: 'Message carrying the source media (add).' },
        revision: { type: 'string', description: 'List revision for move/remove.' },
        afterId: { type: 'string', description: 'Move after this sticker; null moves to first.' },
        idempotencyKey: {
          type: 'string',
          description: 'Stable key for mutations; a retried call never mutates twice.',
        },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_stickers');
      const account = String(principal.accountId);
      const action = String(args['action']) as StickerAction;
      if (!STICKER_ACTIONS.includes(action)) {
        throw new Error(`unknown action: ${String(args['action'])}`);
      }

      switch (action) {
        case 'list': {
          const page = await deps.client.stickerCollections(account, exec.signal);
          return { collections: pageItems(page).items };
        }
        case 'get': {
          const stickerId = requireId(args['stickerId'], 'stickerId');
          const sticker = await deps.client.customSticker(account, stickerId, exec.signal);
          return { sticker };
        }
        case 'download': {
          const stickerId = requireId(args['stickerId'], 'stickerId');
          const sticker = await deps.client.customSticker(account, stickerId, exec.signal);
          // Original bytes are not wired in this milestone; never fake them.
          return {
            sticker,
            status: 'not_staged',
            note: 'Original bytes are not available through this tool yet.',
          };
        }
        case 'prepare': {
          const file = requireId(args['file'], 'file');
          const key = requireId(args['idempotencyKey'], 'idempotencyKey');
          await revalidateForMutation(deps, principal, 'wechat_stickers');
          const input: Record<string, unknown> = { source: file };
          if (args['startMs'] !== undefined) input['startMs'] = args['startMs'];
          if (args['durationMs'] !== undefined) input['durationMs'] = args['durationMs'];
          if (args['maxEdge'] !== undefined) input['maxEdge'] = args['maxEdge'];
          if (args['fps'] !== undefined) input['fps'] = args['fps'];
          const job = await deps.client.prepareSticker(account, input, key, exec.signal);
          return { status: 'submitted', job, note: 'Submitted; poll add/prepare status separately.' };
        }
        case 'add': {
          const key = requireId(args['idempotencyKey'], 'idempotencyKey');
          const preparationId = args['preparationId'];
          const messageId = args['messageId'];
          if ((typeof preparationId === 'string') === (typeof messageId === 'string')) {
            throw new Error('provide exactly one of preparationId or messageId');
          }
          await revalidateForMutation(deps, principal, 'wechat_stickers');
          const input: Record<string, unknown> =
            typeof preparationId === 'string'
              ? { preparationId }
              : { messageId };
          const job = await deps.client.addCustomSticker(account, input, key, exec.signal);
          return { status: 'submitted', job, note: 'Submitted; the sticker may take a moment to appear.' };
        }
        case 'move': {
          const stickerId = requireId(args['stickerId'], 'stickerId');
          const revision = requireId(args['revision'], 'revision');
          const key = requireId(args['idempotencyKey'], 'idempotencyKey');
          await revalidateForMutation(deps, principal, 'wechat_stickers');
          const input = { afterId: args['afterId'] === undefined ? null : args['afterId'] };
          const job = await deps.client.moveCustomSticker(
            account,
            stickerId,
            input,
            revision,
            key,
            exec.signal,
          );
          return { status: 'submitted', job };
        }
        case 'remove': {
          const stickerId = requireId(args['stickerId'], 'stickerId');
          const revision = requireId(args['revision'], 'revision');
          const key = requireId(args['idempotencyKey'], 'idempotencyKey');
          await revalidateForMutation(deps, principal, 'wechat_stickers');
          const job = await deps.client.removeCustomSticker(account, stickerId, revision, key, exec.signal);
          return { status: 'submitted', job };
        }
        case 'preview': {
          const stickerId = requireId(args['stickerId'], 'stickerId');
          const sticker = await deps.client.customSticker(account, stickerId, exec.signal);
          return {
            sticker,
            status: 'unavailable',
            note: 'Preview rendering is not wired yet; use get/list metadata.',
          };
        }
      }
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_login
// ---------------------------------------------------------------------------

export function wechatLoginTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_login',
    description:
      'Inspect and change this WeChat account login state: list accounts, start a login, wait for a login operation, disconnect locally, log out, or forget stored credentials (requires confirm). Owner only; never fabricates QR codes or success.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: [...LOGIN_ACTIONS],
          description: 'Which login operation to run.',
        },
        operationId: { type: 'string', description: 'Login operation id (wait).' },
        timeoutMs: {
          type: 'integer',
          minimum: 1000,
          maximum: 300000,
          description: 'How long to wait for the operation (wait, default 30000).',
        },
        force: { type: 'boolean', description: 'Force a fresh login even if one exists (start).' },
        confirm: {
          type: 'boolean',
          description: 'Must be exactly true for forget.',
        },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_login');
      const account = String(principal.accountId);
      const action = String(args['action']) as LoginAction;
      if (!LOGIN_ACTIONS.includes(action)) {
        throw new Error(`unknown action: ${String(args['action'])}`);
      }

      switch (action) {
        case 'status': {
          const page = await deps.client.accounts();
          return { accounts: pageItems(page).items };
        }
        case 'start': {
          await revalidateForMutation(deps, principal, 'wechat_login');
          const key = mutationKey('login-start', [account]);
          const options = args['force'] === true ? { force: true } : {};
          const operation = await deps.client.login(account, options, key);
          return { status: 'submitted', operation, note: 'Login started; use wait with the operation id.' };
        }
        case 'wait': {
          const operationId = requireId(args['operationId'], 'operationId');
          const timeoutMs =
            args['timeoutMs'] === undefined ? 30_000 : Number(args['timeoutMs']);
          if (!Number.isInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 300_000) {
            throw new Error('timeoutMs must be an integer between 1000 and 300000');
          }
          const deadline = deps.now() + timeoutMs;
          let operation: Record<string, unknown> | undefined;
          let timedOut = false;
          for (;;) {
            operation = (await deps.client.operation(account, operationId, exec.signal)) as
              | Record<string, unknown>
              | undefined;
            if (isTerminalOperation(operation?.['state'])) break;
            const remaining = deadline - deps.now();
            if (remaining <= 0) {
              timedOut = true;
              break;
            }
            await sleep(Math.min(1000, remaining), exec.signal);
          }
          return {
            operation: operation ?? null,
            state: operation?.['state'] ?? null,
            timedOut,
          };
        }
        case 'disconnect': {
          await revalidateForMutation(deps, principal, 'wechat_login');
          await deps.client.disconnect(account);
          return {
            status: 'disconnected_local_trust_kept',
            note: 'Local session dropped; stored trust remains until logout/forget.',
          };
        }
        case 'logout': {
          await revalidateForMutation(deps, principal, 'wechat_login');
          await deps.client.logoutLocal(account);
          return { status: 'logged_out_local', note: 'Local credentials removed for this account.' };
        }
        case 'forget': {
          if (args['confirm'] !== true) {
            throw new Error('forget requires confirm: true');
          }
          await revalidateForMutation(deps, principal, 'wechat_login');
          const key = mutationKey('login-forget', [account]);
          const operation = await deps.client.forgetCredentials(account, key, true);
          return { status: 'submitted', operation, note: 'Credential forget submitted.' };
        }
      }
    },
  });
}

/** All account-scoped owner tools, in registration order. */
export function accountTools(deps: ToolDeps): WechatToolDefinition[] {
  return [wechatStickersTool(deps), wechatLoginTool(deps)];
}
