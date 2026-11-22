/**
 * Plugin-wide diagnostics service. M0 scope: lifecycle + account status only.
 * M1+ attaches the native bridge (inbound monitor), M3 the outbound queue,
 * M5 the calling runtime — all owned by the effect created in apply().
 */
import type { Context } from '@deepseek-ai/cordis';

export type WechatAccountStatus = 'unconfigured' | 'disconnected' | 'connecting' | 'online';

export interface WechatServiceState {
  readonly mode: 'external' | 'managed';
  readonly accountId: string;
  status: WechatAccountStatus;
  startedAt: number | undefined;
  generation: number;
}

interface NativeConfigView {
  mode?: 'external' | 'managed';
  accountId?: string;
}

export class WechatService {
  readonly state: WechatServiceState;

  #running = false;

  private readonly ctx: Context;
  constructor(ctx: Context, rawConfig: unknown) {
    this.ctx = ctx;
    const cfg = (rawConfig ?? {}) as { native?: NativeConfigView };
    this.state = {
      mode: cfg.native?.mode ?? 'external',
      accountId: cfg.native?.accountId ?? 'default',
      status: 'unconfigured',
      startedAt: undefined,
      generation: 0,
    };
  }

  /** Idempotent start; the owning effect guarantees stop on dispose. */
  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.state.startedAt = Date.now();
    // M1: launch/connect native daemon (external attach or managed spawn),
    // then hand the connection to native-monitor port. Nothing to connect
    // yet, so the account stays `unconfigured` for external mode.
  }

  /** Idempotent stop; must abort loops before returning. */
  stop(): void {
    if (!this.#running) return;
    this.#running = false;
    // M1+: abort inbound monitor, outbound queue, calling runtime here.
  }
}
