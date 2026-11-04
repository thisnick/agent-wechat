type ConsultHandle = { id: string; question: string };
type ConsultCoordinator = {
  prepare(question: string): ConsultHandle | undefined;
  schedule(handle: ConsultHandle, delayMs: number, run: (handle: ConsultHandle) => void): void;
  clearPending(): void;
  recordNativeConsult(args: { question: string }, callId?: string): {
    kind: "none" | "pending" | "in_flight" | "already_delivered";
    handle?: ConsultHandle;
  };
  hasRecentNativeConsult(question: string): boolean;
  markStarted(handle: ConsultHandle): void;
  markDelivered(handle: ConsultHandle): void;
  remove(handle: ConsultHandle): void;
  clear(): void;
};
type ForcedConsultSdk = {
  createRealtimeVoiceForcedConsultCoordinator: () => ConsultCoordinator;
  classifySkippableRealtimeVoiceConsultTranscript: (text: string) => string | undefined;
};
type ConsultResult = { text: string } | { error: string; retryable?: boolean; started?: boolean };
type ConsultHooks = {
  consult(question: string, signal?: AbortSignal): Promise<{ text: string }>;
  active(): boolean;
  clearAudio(): void;
  speak(text: string): void;
};

const FALLBACK_DELAY_MS = 200;

/** Stock-style fallback for a final caller turn the provider did not delegate.
 * Only the current question is sent to the consult run. */
export function createCallForcedConsult(sdk: ForcedConsultSdk, hooks: ConsultHooks) {
  const coordinator = sdk.createRealtimeVoiceForcedConsultCoordinator();
  let closed = false;
  let nativeRunning = false;
  let fallback: { handle: ConsultHandle; result: Promise<ConsultResult>; deliverViaTool: boolean } | undefined;
  const active = () => !closed && hooks.active();
  const busy = () => ({ error: "A consultation is in progress. Wait for its result before retrying.", retryable: true, started: false });

  async function run(question: string, signal?: AbortSignal): Promise<ConsultResult> {
    try {
      const { text } = await hooks.consult(question, signal);
      return { text: text.slice(0, 8000) };
    } catch {
      return { error: "Call consultation failed" };
    }
  }

  async function runFallback(handle: ConsultHandle): Promise<void> {
    if (!active() || nativeRunning || fallback) { coordinator.remove(handle); return; }
    coordinator.markStarted(handle);
    hooks.clearAudio();
    const current = { handle, result: run(handle.question), deliverViaTool: false };
    fallback = current;
    try {
      const result = await current.result;
      if (!active()) return;
      if (current.deliverViaTool) {
        coordinator.markDelivered(handle);
      } else if ("text" in result && result.text.trim()) {
        hooks.clearAudio();
        hooks.speak(result.text);
        coordinator.markDelivered(handle);
      } else {
        coordinator.remove(handle);
      }
    } finally {
      fallback = undefined;
    }
  }

  return {
    onTranscript(role: string, text: string, isFinal: boolean): void {
      if (role !== "user" || !isFinal || !active()) return;
      const question = text.trim();
      if (question.length > 4000 || sdk.classifySkippableRealtimeVoiceConsultTranscript(question)
        || coordinator.hasRecentNativeConsult(question)) return;
      coordinator.clearPending();
      const handle = coordinator.prepare(question);
      if (handle) coordinator.schedule(handle, FALLBACK_DELAY_MS, item => {
        void runFallback(item).catch(() => coordinator.remove(item));
      });
    },
    async native(question: string, callId?: string, signal?: AbortSignal): Promise<ConsultResult> {
      if (!active()) return { error: "Calling consultation unavailable" };
      if (!question.trim() || question.length > 4000) return { error: "Question required" };
      const match = coordinator.recordNativeConsult({ question }, callId);
      if (match.kind === "already_delivered") return { text: "The caller already received this answer. Do not repeat it." };
      if (fallback && match.kind === "in_flight" && fallback.handle === match.handle) {
        fallback.deliverViaTool = true;
        return fallback.result;
      }
      if (fallback || nativeRunning) return busy();
      nativeRunning = true;
      try { return await run(question, signal); }
      finally { nativeRunning = false; }
    },
    close(): void {
      closed = true;
      coordinator.clear();
    },
  };
}
