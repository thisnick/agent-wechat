import { useEffect, useState } from "react";
import { desktopInvoke } from "./desktop.ts";
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
      ? "Checking…"
      : status.phase === "downloading"
      ? `Downloading${
          status.total
            ? ` · ${Math.min(
                100,
                Math.round((status.downloaded / status.total) * 100)
              )}%`
            : "…"
        }`
      : status.phase === "installing"
      ? "Installing…"
      : available
      ? `Version ${status.version} is ready`
      : status.phase === "current"
      ? "You’re up to date"
      : `Version ${settings.currentVersion}`;
  async function action(command: string, args?: Record<string, unknown>) {
    setError("");
    try {
      await desktopInvoke(command, args);
    } catch (e) {
      setError(typeof e === "string" ? e : "Could not complete the update.");
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
      <aside className="desktop-update-notice" aria-label="App update">
        <span>{description}</span>
        <button className="secondary" disabled={busy} onClick={install}>
          {busy ? "Updating…" : "Install and restart"}
        </button>
        {available && (
          <button
            className="text-button"
            aria-label="Dismiss update notice"
            onClick={() => setDismissed(status.version || "")}
          >
            Later
          </button>
        )}
        <small>The local hub will restart.</small>
        {error && <small role="alert">{error}</small>}
      </aside>
    );
  }
  return (
    <section className="desktop-update-settings" aria-label="App updates">
      <div className="setting">
        <span>
          App updates<small>{description}</small>
        </span>
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void action("updates_check")}
        >
          Check for updates
        </button>
      </div>
      <div className="setting">
        <label htmlFor="automatic-updates">Check automatically</label>
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
        <label htmlFor="prerelease-updates">Include prereleases</label>
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
              The app and local hub will restart. Profiles stay on this device.
            </small>
          </span>
          <button className="secondary" onClick={install}>
            Install and restart
          </button>
        </div>
      )}
      {status.phase === "downloading" && (
        <progress
          aria-label="Update download"
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
