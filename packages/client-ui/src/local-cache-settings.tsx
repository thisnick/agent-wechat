import { useEffect, useState } from "react";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";

export function LocalCacheSettings({
  connection,
  account,
}: {
  connection: Connection;
  account: string;
}) {
  useLocale();
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
        {t("On this device", "在此设备上")}
        <small>
          {usage?.available === false
            ? t("Browser storage unavailable", "浏览器存储不可用")
            : usage
              ? t(
                  `${(usage.bytes / 1024 / 1024).toFixed(1)} MB · ${usage.chats} chats · ${usage.messages} messages`,
                  `${(usage.bytes / 1024 / 1024).toFixed(1)} MB · ${usage.chats} 个会话 · ${usage.messages} 条消息`,
                )
              : t("Checking storage…", "正在检查存储…")}
        </small>
        <small>{t("Media limit: 256 MB", "媒体缓存上限：256 MB")}</small>
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
              t(
                "Temporary data cleared. Drafts and pending sends kept.",
                "临时数据已清除，草稿与待发送项已保留。",
              ),
            );
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy
          ? t("Clearing…", "正在清除…")
          : t("Clear temporary data", "清除临时数据")}
      </button>
      {notice && (
        <small className="local-cache-notice" role="status">
          {notice}
        </small>
      )}
    </div>
  );
}
