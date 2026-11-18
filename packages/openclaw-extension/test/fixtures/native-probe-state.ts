// Shared by the smoke-test probe plugins in this directory.
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
import type { NativeConsumerState, NativeDispatchManifest } from "../../src/native-consumer-state.ts";

/**
 * The consumer state removes a turn's record when the turn finishes, so a
 * probe keeps its own note of each turn's manifest and confirmed run for the
 * smoke harness to check afterwards.
 */
export function observeTurns(state: () => NativeConsumerState) {
  const turns = new Map<string, { runId?: string; manifest?: NativeDispatchManifest }>();
  const observed = new WeakSet<NativeConsumerState>();
  const watch = (current: NativeConsumerState) => {
    if (observed.has(current)) return current;
    observed.add(current);
    const begin = current.beginDispatch.bind(current);
    current.beginDispatch = (id, manifest, options) => { begin(id, manifest, options); turns.set(id, { ...turns.get(id), manifest }); };
    const prepared = current.preparedCallback.bind(current);
    current.preparedCallback = (ids, policy, previous) => {
      const callback = prepared(ids, policy, previous);
      return (...args: any[]) => {
        const result = callback(...args);
        const runId = typeof args[0] === "string" ? args[0] : args[0]?.runId;
        for (const id of ids) turns.set(id, { ...turns.get(id), runId });
        return result;
      };
    };
    return current;
  };
  return {
    /** Call with each new state object before it is used. */
    watch,
    item: (id: string) => {
      const open = state().get(id);
      return { id, runId: turns.get(id)?.runId, manifest: turns.get(id)?.manifest, open: open?.state, handoffUnconfirmed: open?.handoffUnconfirmed ?? false };
    },
    pendingReads: () => state().pendingReads().map(read => read.threadId),
  };
}

/** The probe's stand-in for the daemon's saved messages; the smoke harness serves this file. */
export function savedMessages(directory: string) {
  const saved: Record<string, NativeMessage> = {};
  return (message: NativeMessage) => { saved[message.id] = message; writeFileSync(join(directory, "daemon-messages.json"), JSON.stringify(saved)); return message; };
}
