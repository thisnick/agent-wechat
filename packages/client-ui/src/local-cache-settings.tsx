import { useEffect, useState } from "react";
import type { Connection } from "./connection.ts";
import { t } from "./i18n.ts";

export function LocalCacheSettings({
  connection,
  account,
}: {
  connection: Connection;
  account: string;
}) {
  const [usage, setUsage] = useState<{
    bytes: number;
    messages: number;
    chats: number;
    available: boolean;
  }>();
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  useEffect(() => {
    let cancelled = false;
    void connection.cache.stats(account).then((value) => {
      if (!cancelled) setUsage(value);
    });
    return () => {
      cancelled = true;
    };
  }, [connection, account]);
  return (
    <div className="setting local-cache-setting">
      <span>
        {t("localCacheSettings.device")}
        <small className="cache-usage">
          {usage?.available === false
            ? t("localCacheSettings.browserStorageUnavailable")
            : usage
              ? t("localCacheSettings.mbChatsMessages", { size: (usage.bytes / 1024 / 1024).toFixed(1), chats: usage.chats, messages: usage.messages })
              : t("localCacheSettings.checkingStorage")}
        </small>
        <small>{t("localCacheSettings.mediaLimit256Mb")}</small>
      </span>
      <button
        className="secondary"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await connection.clearCache(account);
            setUsage(await connection.cache.stats(account));
            setNotice(
              t("localCacheSettings.temporaryDataClearedDraftsPending"),
            );
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy
          ? t("localCacheSettings.clearing")
          : t("localCacheSettings.clearTemporaryData")}
      </button>
      {notice && (
        <small className="local-cache-notice" role="status">
          {notice}
        </small>
      )}
    </div>
  );
}
