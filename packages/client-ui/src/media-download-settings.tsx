import { useEffect, useId, useRef, useState } from "react";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
import { readableError } from "./session.ts";

const MB = 1024 * 1024;

export function MediaDownloadSettings({
  connection,
  account,
}: {
  connection: Connection;
  account: string;
}) {
  useLocale();
  const id = useId();
  const active = useRef(new AbortController());
  const [saved, setSaved] = useState<number>();
  const [limit, setLimit] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  useEffect(() => {
    const lifetime = new AbortController();
    active.current = lifetime;
    setBusy(false);
    setSaved(undefined);
    setLimit("");
    setNotice("");
    void connection.session.client
      .accountSettings(account, lifetime.signal)
      .then((settings) => {
        if (lifetime.signal.aborted) return;
        setSaved(settings.autoDownloadMaxBytes);
        setLimit(String(settings.autoDownloadMaxBytes / MB));
      })
      .catch((error) => {
        if (!lifetime.signal.aborted) setNotice(readableError(error));
      });
    return () => lifetime.abort();
  }, [connection, account]);
  const bytes = Number(limit) * MB;
  const valid =
    limit !== "" &&
    Number.isSafeInteger(bytes) &&
    bytes >= 0 &&
    bytes <= 512 * MB;
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (busy || !valid || saved === undefined) return;
        setBusy(true);
        setNotice("");
        const lifetime = active.current;
        void connection.session.client
          .updateAccountSettings(
            account,
            { autoDownloadMaxBytes: bytes },
            lifetime.signal,
          )
          .then((settings) => {
            if (lifetime.signal.aborted) return;
            setSaved(settings.autoDownloadMaxBytes);
            setLimit(String(settings.autoDownloadMaxBytes / MB));
            setNotice(t("Saved.", "已保存。"));
            connection.incomingMedia.refreshAccount(account);
          })
          .catch((error) => {
            if (!lifetime.signal.aborted) setNotice(readableError(error));
          })
          .finally(() => {
            if (!lifetime.signal.aborted) setBusy(false);
          });
      }}
    >
      <label htmlFor={id}>{t("Automatic downloads", "自动下载")}</label>
      <div className="media-limit-setting">
        <span>{t("Up to", "上限")}</span>
        <input
          id={id}
          type="number"
          min="0"
          max="512"
          step="any"
          value={limit}
          onChange={(event) => setLimit(event.target.value)}
          disabled={busy || saved === undefined}
          required
          aria-describedby={`${id}-help`}
        />
        <span>MB</span>
        <button
          className="secondary"
          disabled={busy || !valid || saved === undefined || bytes === saved}
        >
          {busy ? t("Saving…", "正在保存…") : t("Save", "保存")}
        </button>
      </div>
      <small id={`${id}-help`}>
        {t(
          "On agent-wechat hub. Thumbnails and voice notes always download.",
          "在 agent-wechat Hub 上生效。缩略图和语音消息始终自动下载。",
        )}
      </small>
      {notice && <small role="status">{notice}</small>}
    </form>
  );
}
