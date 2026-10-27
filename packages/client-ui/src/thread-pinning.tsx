import { useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import type { ThreadPinning } from "./thread-pinning.ts";
import { Icon } from "./ui.tsx";

export function PinFeedback({ model }: { model: ThreadPinning }) {
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  return (
    <>
      <span className="sr-only" role="status" aria-live="polite">
        {state.announcement}
      </span>
      {(state.uncertain || state.error) && (
        <div className="pin-feedback" role={state.error ? "alert" : undefined}>
          <span>
            {state.uncertain ? "Couldn’t confirm pin change." : state.error}
          </span>
          <button
            className="text-button"
            disabled={state.busy}
            onClick={() => void model.check()}
          >
            {state.uncertain ? "Check status" : "Refresh"}
          </button>
        </div>
      )}
    </>
  );
}

export function ThreadPinSwitch({
  connection,
  account,
  threadId,
}: {
  connection: Connection;
  account: string;
  threadId: string;
}) {
  const model = connection.threadPinning(account, threadId);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  if (!state.supported) return null;
  const known = state.pinned !== null;
  return (
    <div className="detail-section">
      <div className="pin-detail-setting">
        <button
          className="group-toggle"
          role={known ? "switch" : undefined}
          aria-checked={known ? state.pinned! : undefined}
          aria-label={known ? "Pin chat" : "Pin chat: state unavailable"}
          aria-busy={state.busy}
          disabled={state.disabled}
          title={known ? "Keep this chat at the top" : "Pin state unavailable"}
          onClick={() => void model.toggle()}
        >
          <span>Pin chat</span>
          {state.busy && <span className="pin-spinner" aria-hidden="true" />}
          <span
            className={`toggle-track ${state.pinned ? "on" : ""} ${
              known ? "" : "unknown"
            }`}
            aria-hidden="true"
          >
            {known ? <i /> : <span>−</span>}
          </span>
        </button>
        {!known && (
          <button
            className="icon-btn"
            aria-label="Refresh pin state"
            title="Refresh pin state"
            disabled={state.busy}
            onClick={() => void model.check()}
          >
            <Icon name="refresh" />
          </button>
        )}
      </div>
      <PinFeedback model={model} />
    </div>
  );
}
