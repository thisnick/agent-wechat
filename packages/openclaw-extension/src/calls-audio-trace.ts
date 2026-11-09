import { monitorEventLoopDelay } from "node:perf_hooks";
import type { NativeCallPcmObserver } from "@agent-wechat/shared";

type Counter = "wsRxChunks" | "wsRxSamples" | "wsRxDroppedChunks" | "wsRxDroppedSamples"
  | "inputConsumedChunks" | "inputConsumedSamples" | "realtimeInputChunks" | "realtimeInputSamples"
  | "providerOutputChunks" | "providerOutputSamples" | "daemonOutputChunks" | "daemonOutputSamples"
  | "outputDiscardedSamples" | "outputOverflowSamples" | "outputBudgetInterruptions"
  | "outputStaleChunks" | "outputFailures" | "outputSilenceSamples" | "outputSkippedSilenceSamples";
type Gap = "wsRxGapMs" | "inputConsumeGapMs" | "realtimeInputGapMs" | "providerOutputGapMs" | "daemonOutputGapMs";
type Measure = Gap | "sdkRxQueueDepth" | "sdkRxWaitMs" | "inputResampleMs" | "realtimeForwardMs"
  | "providerOutputChunkMs" | "outputResampleMs" | "outputQueuedAudioMs" | "outputLeadMs" | "daemonForwardMs";
type Distribution = { count: number; total: number; min: number; max: number };
const WINDOW_MS = 5_000;
const rounded = (n: number) => Math.round(n * 100) / 100;

/** Per-call timing windows with fixed keys and no audio, identities or errors. */
export class CallAudioTrace {
  private readonly loop = monitorEventLoopDelay({ resolution: 10 });
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly previous: Partial<Record<Gap, number>> = {};
  private counts: Partial<Record<Counter, number>> = {};
  private measures: Partial<Record<Measure, Distribution>> = {};
  private started = performance.now();
  private startedUnixMs = Date.now();
  private closed = false;
  private readonly log: (line: string) => void;
  private readonly rates: { daemonHz: number; realtimeHz: number };

  constructor(log: (line: string) => void, rates: { daemonHz: number; realtimeHz: number }) {
    this.log = log;
    this.rates = rates;
    this.loop.enable();
    this.timer = setInterval(() => this.report(false), WINDOW_MS);
    this.timer.unref();
  }

  readonly observePcm: NativeCallPcmObserver = arrival => {
    this.add("wsRxChunks");
    this.add("wsRxSamples", arrival.samples);
    this.gap("wsRxGapMs", arrival.receivedAtMs);
    this.measure("sdkRxQueueDepth", arrival.queueDepth);
    if (arrival.droppedSamples) {
      this.add("wsRxDroppedChunks");
      this.add("wsRxDroppedSamples", arrival.droppedSamples);
    }
  };

  add(key: Counter, count = 1): void {
    if (!this.closed) this.counts[key] = (this.counts[key] ?? 0) + count;
  }

  gap(key: Gap, now: number): void {
    if (this.closed) return;
    const last = this.previous[key];
    if (last !== undefined) this.measure(key, now - last);
    this.previous[key] = now;
  }

  measure(key: Measure, value: number): void {
    if (this.closed) return;
    const current = this.measures[key];
    if (current) {
      current.count++;
      current.total += value;
      current.min = Math.min(current.min, value);
      current.max = Math.max(current.max, value);
    } else this.measures[key] = { count: 1, total: value, min: value, max: value };
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    clearInterval(this.timer);
    this.loop.disable();
    this.report(true);
  }

  private report(final: boolean): void {
    const now = performance.now();
    const nowUnixMs = Date.now();
    const measures = Object.fromEntries(Object.entries(this.measures).map(([key, values]) =>
      [key, { count: values.count, min: rounded(values.min), mean: rounded(values.total / values.count), max: rounded(values.max) }]));
    const eventLoopDelayMs = this.loop.count
      ? { count: this.loop.count, p95: rounded(this.loop.percentile(95) / 1e6), max: rounded(this.loop.max / 1e6) }
      : { count: 0 };
    const report = { startedUnixMs: this.startedUnixMs, windowMs: rounded(now - this.started), final,
      rates: this.rates, counts: this.counts, measures, eventLoopDelayMs };
    this.started = now;
    this.startedUnixMs = nowUnixMs;
    this.counts = {};
    this.measures = {};
    this.loop.reset();
    try { this.log(`[wechat-call-audio] plugin ${JSON.stringify(report)}`); } catch { /* Observability cannot end a call. */ }
  }
}
