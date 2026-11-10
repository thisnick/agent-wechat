import { useEffect, useId, useRef, useState } from "react";
import type { Connection } from "./connection.ts";
import { desktop, desktopInvoke } from "./desktop.ts";
import { t } from "./i18n.ts";

type Preference = { enabled: boolean; desktopEnabled: boolean };

export function AccountNotifications({ connection, account }: { connection: Connection; account: string }) {
  const id = useId();
  const [saved, setSaved] = useState<Preference>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const lifetime = useRef<AbortController>(new AbortController());
  const request = useRef(0);
  const scope = { connectionId: connection.desktopId, serverId: connection.cache.namespace[1], accountId: account };
  useEffect(() => {
    const current = new AbortController();
    lifetime.current = current;
    setSaved(undefined); setBusy(false); setError("");
    const refresh = () => {
      const version = ++request.current;
      if (desktop && scope.connectionId) void desktopInvoke<Preference>("notifications_account_settings", scope)
        .then(value => { if (!current.signal.aborted && version === request.current) setSaved(value); })
        .catch(() => { if (!current.signal.aborted && version === request.current) setError(t("accountNotifications.couldnTLoadNotificationSettings")); });
    };
    refresh();
    window.addEventListener("desktop-notifications-changed", refresh);
    return () => { current.abort(); window.removeEventListener("desktop-notifications-changed", refresh); };
  }, [connection, account]);
  if (!desktop || !connection.desktopId) return null;
  return <div>
    <div className="setting">
      <label htmlFor={id}>{t("accountNotifications.messageNotifications")}<small>{t("accountNotifications.accountDevice")}</small></label>
      <input id={id} type="checkbox" checked={saved?.enabled ?? false} disabled={!saved || busy}
        onChange={event => {
          const current = lifetime.current;
          const version = ++request.current;
          setBusy(true); setError("");
          void desktopInvoke<Preference>("notifications_account_settings", { ...scope, enabled: event.target.checked })
            .then(value => { if (!current.signal.aborted && version === request.current) setSaved(value); })
            .catch(() => { if (!current.signal.aborted && version === request.current) setError(t("accountNotifications.couldnTSaveTryAgain")); })
            .finally(() => { if (!current.signal.aborted) setBusy(false); });
        }} />
    </div>
    {saved && !saved.desktopEnabled && <small>{t("accountNotifications.desktopNotificationsTurnedOff")}</small>}
    {error && <small role="alert">{error}</small>}
  </div>;
}
