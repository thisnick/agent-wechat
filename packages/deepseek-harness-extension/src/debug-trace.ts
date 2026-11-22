/**
 * File-backed activation trace.
 *
 * The host logger can be hard to observe (the `dsh web` terminal is not always
 * at hand), so activation attempts append one line per step to
 * `~/.dsh/wechat-channel.log`. Every write is best-effort: tracing must never
 * affect plugin behaviour.
 */
import { appendFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export function debugTrace(scope: string, message: string): void {
  // The node test runner executes assemble flows against fake hosts; their
  // trace lines must not pollute the real host log.
  if (process.env.NODE_TEST_CONTEXT !== undefined) return;
  try {
    appendFileSync(
      join(homedir(), '.dsh', 'wechat-channel.log'),
      `${new Date().toISOString()} [${scope}] ${message}\n`,
    );
  } catch {
    // Diagnostics only — never fail the channel for a trace write.
  }
}

/** Trace an awaited step, reporting the elapsed time on completion. */
export async function traceStep<T>(scope: string, label: string, step: () => Promise<T>): Promise<T> {
  debugTrace(scope, `${label} …`);
  const startedAt = Date.now();
  try {
    const value = await step();
    debugTrace(scope, `${label} ok (${Date.now() - startedAt}ms)`);
    return value;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    debugTrace(scope, `${label} FAILED after ${Date.now() - startedAt}ms: ${message}`);
    throw error;
  }
}
