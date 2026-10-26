import { useEffect, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { groupEnded } from "./group-creation.ts";
export function ThreadNotifications({
  connection,
  account,
  threadId,
}: {
  connection: Connection;
  account: string;
  threadId: string;
}) {
  const state = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot
  );
  const model = connection.groupCreation(account, threadId);
  const operation = useSyncExternalStore(model.subscribe, model.snapshot);
  const receipt = operation.receipt;
  const current = receipt?.operation;
  useEffect(() => {
    if (current?.state === "succeeded")
      void connection.session.refresh(account).catch(() => {});
  }, [connection, account, current?.id, current?.state]);
  const supported =
    Array.isArray(state.capabilities.threadNotificationTargets) &&
    state.capabilities.threadNotificationTargets.includes("direct");
  if (!supported || threadId === "filehelper" || threadId.endsWith("@chatroom"))
    return null;
  const muted = state.threads.find((thread) => thread.id === threadId)?.muted;
  const unresolved =
    receipt &&
    !receipt.notAdmitted &&
    (!groupEnded(current) || current?.state === "needs_review");
  return (
    <div className="detail-section">
      <button
        className="group-toggle"
        role="switch"
        aria-checked={muted === true}
        disabled={operation.busy || !!unresolved || typeof muted !== "boolean"}
        onClick={() => {
          model.clear();
          void model.run({ action: "mute", groupId: threadId, muted: !muted });
        }}
      >
        <span>
          Mute notifications
          <small>
            {typeof muted !== "boolean"
              ? "Setting unavailable"
              : muted
              ? "On"
              : "Off"}
          </small>
        </span>
        <span className={`toggle-track ${muted ? "on" : ""}`}>
          <i />
        </span>
      </button>
      {unresolved && (
        <p className="field-help">
          {current?.state === "needs_review"
            ? "Couldn’t confirm this change."
            : "Updating…"}{" "}
          <button className="text-button" onClick={() => void model.check()}>
            Check status
          </button>
        </p>
      )}
      {operation.error && (
        <p className="attachment-error" role="alert">
          {operation.error}
        </p>
      )}
    </div>
  );
}
