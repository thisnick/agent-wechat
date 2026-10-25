import { useEffect, useState } from "react";
import type { Connection } from "./connection.ts";

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
        On this device
        <small>
          {usage?.available === false
            ? "Browser storage unavailable"
            : usage
              ? `${(usage.bytes / 1024 / 1024).toFixed(1)} MB · ${
                  usage.chats
                } chats · ${usage.messages} messages`
              : "Checking storage…"}
        </small>
        <small>Media limit: 256 MB</small>
      </span>
      <button
        className="secondary"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await connection.clearCache(account);
            setUsage(await connection.cache.stats(account));
            setNotice("Temporary data cleared. Drafts and pending sends kept.");
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? "Clearing…" : "Clear temporary data"}
      </button>
      {notice && (
        <small className="local-cache-notice" role="status">
          {notice}
        </small>
      )}
    </div>
  );
}
