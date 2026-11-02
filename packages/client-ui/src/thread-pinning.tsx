import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { NativeThread } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
import type { ThreadPreference } from "./thread-pinning.ts";
import { Icon } from "./ui.tsx";

/** Pending receipt intent affects presentation only; cached threads stay canonical. */
export function useThreadPinPreview(
  connection: Connection,
  account: string,
  threads: NativeThread[],
) {
  const models = useMemo(
    () => threads.map((thread) => connection.threadPinning(account, thread.id)),
    [connection, account, threads],
  );
  const subscribe = useCallback(
    (listener: () => void) => {
      const stops = models.map((model) => model.subscribe(listener));
      return () => stops.forEach((stop) => stop());
    },
    [models],
  );
  const snapshot = useCallback(
    () =>
      JSON.stringify(models.map((model) => model.snapshot().preview ?? null)),
    [models],
  );
  const values = JSON.parse(useSyncExternalStore(subscribe, snapshot)) as (
    | boolean
    | null
  )[];
  if (values.every((value) => value === null)) return threads;
  const preview = threads.map((thread, i) =>
    values[i] === null ? thread : { ...thread, pinned: values[i] },
  );
  return [
    ...preview.filter((thread) => thread.pinned === true),
    ...preview.filter((thread) => thread.pinned !== true),
  ];
}

export function PinFeedback<K extends "pinned" | "muted">({
  model,
  kind = "pin",
}: {
  model: ThreadPreference<K>;
  kind?: "pin" | "notification";
}) {
  useLocale();
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  return (
    <>
      <span className="sr-only" role="status" aria-live="polite">
        {state.announcement}
      </span>
      {(state.uncertain || state.error) && (
        <div className="pin-feedback" role={state.error ? "alert" : undefined}>
          <span>
            {state.uncertain
              ? t(
                  `Couldn’t confirm ${kind} change.`,
                  kind === "pin"
                    ? "无法确认置顶更改。"
                    : "无法确认通知更改。",
                )
              : state.error}
          </span>
          <button
            className="text-button"
            disabled={state.busy}
            onClick={() => void model.check()}
          >
            {state.uncertain
              ? t("Check status", "检查状态")
              : t("Refresh", "刷新")}
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
  useLocale();
  const model = connection.threadPinning(account, threadId);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  if (!state.supported) return null;
  const known = state.pinned !== null;
  const displayed = state.preview ?? state.pinned;
  return (
    <div className="detail-section">
      <div className="pin-detail-setting">
        <button
          className="group-toggle"
          role={known ? "switch" : undefined}
          aria-checked={known ? displayed! : undefined}
          aria-label={
            known
              ? t("Pin chat", "置顶聊天")
              : t("Pin chat: state unavailable", "置顶聊天：状态不可用")
          }
          aria-busy={state.busy}
          disabled={state.disabled}
          title={
            known
              ? t("Keep this chat at the top", "将此聊天保持在顶部")
              : t("Pin state unavailable", "置顶状态不可用")
          }
          onClick={() => void model.toggle()}
        >
          <span>{t("Pin chat", "置顶聊天")}</span>
          {state.saving && <span className="pin-spinner" aria-hidden="true" />}
          <span
            className={`toggle-track ${displayed ? "on" : ""} ${
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
            aria-label={t("Refresh pin state", "刷新置顶状态")}
            title={t("Refresh pin state", "刷新置顶状态")}
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
