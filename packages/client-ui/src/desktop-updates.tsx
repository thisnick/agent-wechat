import { useEffect, useState } from "react";
import { desktopInvoke } from "./desktop.ts";
import { t } from "./i18n.ts";
type UpdateSettings = {
  currentVersion: string;
  automatic: boolean;
  prereleases: boolean;
  status: {
    phase: string;
    version: string | null;
    downloaded: number;
    total: number | null;
    error: string | null;
  };
};
export function DesktopUpdates({ notice = false }: { notice?: boolean }) {
  const [settings, setSettings] = useState<UpdateSettings | null>(null);
  const [error, setError] = useState("");
  const [dismissed, setDismissed] = useState("");
  const refresh = async () =>
    setSettings(await desktopInvoke<UpdateSettings>("updates_status"));
  useEffect(() => {
    void refresh().catch(() => {});
    const timer = window.setInterval(
      () => {
        void refresh().catch(() => {});
      },
      notice ? 15000 : 1000
    );
    return () => window.clearInterval(timer);
  }, [notice]);
  if (!settings) return null;
  const { status } = settings;
  const busy = ["checking", "downloading", "installing"].includes(status.phase);
  const available = status.phase === "available";
  const description =
    status.phase === "checking"
      ? t("desktopUpdates.checking")
      : status.phase === "downloading"
      ? `${t("desktopUpdates.downloading")}${
          status.total
            ? ` · ${Math.min(
                100,
                Math.round((status.downloaded / status.total) * 100)
              )}%`
            : "…"
        }`
      : status.phase === "installing"
      ? t("desktopUpdates.installing")
      : available
      ? t("desktopUpdates.versionReady", { version: status.version })
      : status.phase === "current"
      ? t("desktopUpdates.reUpDate")
      : t("desktopUpdates.version", { currentVersion: settings.currentVersion });
  async function action(command: string, args?: Record<string, unknown>) {
    setError("");
    try {
      await desktopInvoke(command, args);
    } catch (e) {
      setError(
        typeof e === "string"
          ? e
          : t("desktopUpdates.couldNotCompleteUpdate"),
      );
    }
    await refresh().catch(() => {});
  }
  const install = () => void action("updates_install");
  if (notice) {
    if (
      (!available && !["downloading", "installing"].includes(status.phase)) ||
      (available && dismissed === status.version)
    )
      return null;
    return (
      <aside
        className="desktop-update-notice"
        aria-label={t("desktopUpdates.appUpdate")}
      >
        <span>{description}</span>
        <button className="secondary" disabled={busy} onClick={install}>
          {busy ? t("desktopUpdates.updating") : t("desktopUpdates.installRestart")}
        </button>
        {available && (
          <button
            className="text-button"
            aria-label={t("desktopUpdates.dismissUpdateNotice")}
            onClick={() => setDismissed(status.version || "")}
          >
            {t("desktopUpdates.later")}
          </button>
        )}
        <small>{t("desktopUpdates.localHubWillRestart")}</small>
        {error && <small role="alert">{error}</small>}
      </aside>
    );
  }
  return (
    <section
      className="desktop-update-settings"
      aria-label={t("desktopUpdates.appUpdates")}
    >
      <div className="setting">
        <span>
          {t("desktopUpdates.appUpdates")}
          <small>{description}</small>
        </span>
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void action("updates_check")}
        >
          {t("desktopUpdates.checkUpdates")}
        </button>
      </div>
      <div className="setting">
        <label htmlFor="automatic-updates">
          {t("desktopUpdates.checkAutomatically")}
        </label>
        <input
          id="automatic-updates"
          type="checkbox"
          checked={settings.automatic}
          disabled={busy}
          onChange={(event) =>
            void action("updates_preferences", {
              preferences: {
                automatic: event.target.checked,
                prereleases: settings.prereleases,
              },
            })
          }
        />
      </div>
      <div className="setting">
        <label htmlFor="prerelease-updates">
          {t("desktopUpdates.includePrereleases")}
        </label>
        <input
          id="prerelease-updates"
          type="checkbox"
          checked={settings.prereleases}
          disabled={busy}
          onChange={(event) =>
            void action("updates_preferences", {
              preferences: {
                automatic: settings.automatic,
                prereleases: event.target.checked,
              },
            })
          }
        />
      </div>
      {available && (
        <div className="setting">
          <span>
            <small>
              {t("desktopUpdates.appLocalHubWillRestart")}
            </small>
          </span>
          <button className="secondary" onClick={install}>
            {t("desktopUpdates.installRestart")}
          </button>
        </div>
      )}
      {status.phase === "downloading" && (
        <progress
          aria-label={t("desktopUpdates.updateDownload")}
          value={status.total ? status.downloaded : undefined}
          max={status.total || undefined}
        />
      )}
      {(error || status.error) && (
        <p role="alert" className="connection-error">
          {error || status.error}
        </p>
      )}
    </section>
  );
}
