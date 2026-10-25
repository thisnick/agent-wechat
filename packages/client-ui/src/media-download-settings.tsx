import { useEffect, useId, useRef, useState } from "react";
import type { Connection } from "./connection.ts";
import { readableError } from "./session.ts";

const MB = 1024 * 1024;

export function MediaDownloadSettings({
  connection,
  account,
}: {
  connection: Connection;
  account: string;
}) {
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
            setNotice("Saved.");
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
      <label htmlFor={id}>Automatic downloads</label>
      <div className="media-limit-setting">
        <span>Up to</span>
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
          {busy ? "Saving…" : "Save"}
        </button>
      </div>
      <small id={`${id}-help`}>
        On agent-wechat hub. Thumbnails and voice notes always download.
      </small>
      {notice && <small role="status">{notice}</small>}
    </form>
  );
}
