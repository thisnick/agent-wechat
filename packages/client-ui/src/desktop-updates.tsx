import { useEffect, useState } from "react";
import { desktopInvoke } from "./desktop.ts";
import { t, useLocale } from "./i18n.ts";
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
  useLocale();
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
      ? t("Checking…", "正在检查…")
      : status.phase === "downloading"
      ? `${t("Downloading", "正在下载")}${
          status.total
            ? ` · ${Math.min(
                100,
                Math.round((status.downloaded / status.total) * 100)
              )}%`
            : "…"
        }`
      : status.phase === "installing"
      ? t("Installing…", "正在安装…")
      : available
      ? t(`Version ${status.version} is ready`, `版本 ${status.version} 已就绪`)
      : status.phase === "current"
      ? t("You’re up to date", "已是最新版本")
      : t(`Version ${settings.currentVersion}`, `版本 ${settings.currentVersion}`);
  async function action(command: string, args?: Record<string, unknown>) {
    setError("");
    try {
      await desktopInvoke(command, args);
    } catch (e) {
      setError(
        typeof e === "string"
          ? e
          : t("Could not complete the update.", "无法完成更新。"),
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
        aria-label={t("App update", "应用更新")}
      >
        <span>{description}</span>
        <button className="secondary" disabled={busy} onClick={install}>
          {busy ? t("Updating…", "正在更新…") : t("Install and restart", "安装并重启")}
        </button>
        {available && (
          <button
            className="text-button"
            aria-label={t("Dismiss update notice", "忽略更新提示")}
            onClick={() => setDismissed(status.version || "")}
          >
            {t("Later", "稍后")}
          </button>
        )}
        <small>{t("The local hub will restart.", "本地 Hub 将重启。")}</small>
        {error && <small role="alert">{error}</small>}
      </aside>
    );
  }
  return (
    <section
      className="desktop-update-settings"
      aria-label={t("App updates", "应用更新")}
    >
      <div className="setting">
        <span>
          {t("App updates", "应用更新")}
          <small>{description}</small>
        </span>
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void action("updates_check")}
        >
          {t("Check for updates", "检查更新")}
        </button>
      </div>
      <div className="setting">
        <label htmlFor="automatic-updates">
          {t("Check automatically", "自动检查更新")}
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
          {t("Include prereleases", "包含预发布版本")}
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
              {t(
                "The app and local hub will restart. Profiles stay on this device.",
                "应用和本地 Hub 将重启，本地配置保留在此设备上。",
              )}
            </small>
          </span>
          <button className="secondary" onClick={install}>
            {t("Install and restart", "安装并重启")}
          </button>
        </div>
      )}
      {status.phase === "downloading" && (
        <progress
          aria-label={t("Update download", "更新下载")}
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
