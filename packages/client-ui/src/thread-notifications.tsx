import { useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
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
  useLocale();
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
            known
              ? t("Mute notifications", "静音通知")
              : t("Notification state unavailable", "通知状态不可用")
          }
          aria-checked={known ? state.muted! : undefined}
          aria-busy={state.busy}
          disabled={state.disabled}
          onClick={() => void model.toggle()}
        >
          <span>{t("Mute notifications", "静音通知")}</span>
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
            aria-label={t("Refresh notification state", "刷新通知状态")}
            title={t("Refresh notification state", "刷新通知状态")}
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
