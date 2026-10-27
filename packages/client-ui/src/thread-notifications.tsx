import { useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { PinFeedback } from "./thread-pinning.tsx";
import { Icon } from "./ui.tsx";
export function ThreadNotifications({
  connection,
  account,
  threadId,
}: {
  connection: Connection;
  account: string;
  threadId: string;
}) {
  const model = connection.threadMuting(account, threadId);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  if (!state.supported) return null;
  const known = state.muted !== null;
  return (
    <div className="detail-section">
      <div className="pin-detail-setting">
        <button
          className="group-toggle"
          role={known ? "switch" : undefined}
          aria-label={
            known ? "Mute notifications" : "Notification state unavailable"
          }
          aria-checked={known ? state.muted! : undefined}
          aria-busy={state.busy}
          disabled={state.disabled}
          onClick={() => void model.toggle()}
        >
          <span>Mute notifications</span>
          {state.saving && <span className="pin-spinner" aria-hidden="true" />}
          <span
            className={`toggle-track ${state.muted ? "on" : ""} ${
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
            aria-label="Refresh notification state"
            title="Refresh notification state"
            disabled={state.busy}
            onClick={() => void model.check()}
          >
            <Icon name="refresh" />
          </button>
        )}
      </div>
      <PinFeedback model={model} kind="notification" />
    </div>
  );
}
