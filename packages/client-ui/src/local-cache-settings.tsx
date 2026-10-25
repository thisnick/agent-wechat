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
    <div className="local-cache-setting">
      <div className="setting">
        <span>
          Local cache
          <small>
            {usage?.available === false
              ? "Browser storage unavailable"
              : usage
              ? `${(usage.bytes / 1024 / 1024).toFixed(1)} MB · ${
                  usage.chats
                } chats · ${usage.messages} messages`
              : "Checking storage…"}
          </small>
        </span>
        <button
          className="secondary"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await connection.clearCache(account);
              setUsage(await connection.cache.stats(account));
              setNotice("Cache cleared. Drafts and pending sends kept.");
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Clearing…" : "Clear cache"}
        </button>
      </div>
      <small className="muted">Media cache limit: 256 MB</small>
      {notice && <p role="status">{notice}</p>}
    </div>
  );
}
