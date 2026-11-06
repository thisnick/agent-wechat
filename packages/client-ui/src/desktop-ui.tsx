import { DesktopUpdates } from "./desktop-updates.tsx";
import { useEffect, useState, type FormEvent } from "react";
import {
  desktopConnect,
  desktopInvoke,
  type DesktopSession,
  type DesktopSettings,
} from "./desktop.ts";
import type { Connection } from "./connection.ts";
import { syncDesktopLocale, t, useLocale, type Locale } from "./i18n.ts";
import { Welcome } from "./ui.tsx";
const message = (error: unknown) =>
  typeof error === "string"
    ? error
    : error instanceof Error
    ? error.message
    : t("The operation could not be completed.", "无法完成该操作。");
function translateNotificationStatus(status?: string): string {
  switch (status) {
    case "Listening for new messages":
      return t("Listening for new messages", "正在监听新消息");
    case "Waiting for the hub":
      return t("Waiting for the hub", "正在等待 Hub");
    case "Connect to a hub to receive notifications":
      return t(
        "Connect to a hub to receive notifications",
        "连接 Hub 后即可接收通知",
      );
    case "Notifications off":
      return t("Notifications off", "通知已关闭");
    default:
      return (
        status || t("New messages in unmuted chats.", "未静音会话中的新消息。")
      );
  }
}
export function DesktopWelcome({
  onConnect,
  error: initialError,
}: {
  onConnect: (connection: Connection) => void;
  error: string;
}) {
  useLocale();
  const [settings, setSettings] = useState<DesktopSettings | null>(null);
  const [mode, setMode] = useState<"choose" | "remote" | "settings">("choose");
  const [error, setError] = useState(initialError),
    [busy, setBusy] = useState(false);
  const [name, setName] = useState(""),
    [endpoint, setEndpoint] = useState("http://127.0.0.1:6174"),
    [key, setKey] = useState("");
  const [certificate, setCertificate] = useState("");
  const https = endpoint.trim().toLowerCase().startsWith("https://");
  useEffect(() => {
    if (mode !== "choose") return;
    let cancelled = false;
    void desktopInvoke<DesktopSettings>("desktop_settings")
      .then((value) => {
        syncDesktopLocale(value.language);
        if (!cancelled) setSettings(value);
      })
      .catch((error) => {
        if (!cancelled) setError(message(error));
      });
    return () => {
      cancelled = true;
    };
  }, [mode]);
  async function open(command: string, args?: Record<string, unknown>) {
    setBusy(true);
    setError("");
    try {
      const session = await desktopInvoke<DesktopSession>(command, args);
      setKey("");
      onConnect(await desktopConnect(session));
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
    }
  }
  async function remote(event: FormEvent) {
    event.preventDefault();
    await open("desktop_remote", {
      name,
      endpoint,
      key,
      certificate: https ? certificate.trim() || null : null,
    });
  }
  if (mode === "settings")
    return (
      <Welcome simulator={false}>
        <div className="welcome-heading">
          <h1>{t("Desktop settings", "桌面设置")}</h1>
        </div>
        <DesktopPreferences onConnection={onConnect} manageLocal />
        <button className="secondary" onClick={() => setMode("choose")}>
          {t("Back", "返回")}
        </button>
      </Welcome>
    );
  return (
    <Welcome simulator={false}>
      <div className="welcome-heading">
        <h1>
          {mode === "remote"
            ? t("Connect to a hub", "连接到 Hub")
            : t("Welcome to AgentWechat", "欢迎使用 AgentWechat")}
        </h1>
        <p>
          {mode === "remote"
            ? t(
                "Save this connection in your system keyring.",
                "将此连接保存在系统密钥环中。",
              )
            : t(
                "Use this device, or connect to an existing agent-wechat hub.",
                "使用此设备，或连接到现有的 agent-wechat Hub。",
              )}
        </p>
      </div>
      {mode === "choose" ? (
        <div className="desktop-choices">
          <button
            className="continue-button"
            disabled={busy}
            onClick={() => void open("desktop_local")}
          >
            {busy ? t("Opening…", "正在打开…") : t("Use this device", "使用此设备")}
          </button>
          <button
            className="secondary"
            disabled={busy}
            onClick={() => setMode("remote")}
          >
            {t("Connect to another hub", "连接到其他 Hub")}
          </button>
          {settings?.connections
            .filter((connection) => !connection.local)
            .map((connection) => (
              <button
                key={connection.id}
                className="secondary"
                disabled={busy}
                onClick={() =>
                  void open("desktop_select", { id: connection.id })
                }
              >
                {connection.name}
                <small>{connection.endpoint}</small>
              </button>
            ))}
          <button className="text-button" onClick={() => setMode("settings")}>
            {t("Desktop settings", "桌面设置")}
          </button>
        </div>
      ) : (
        <form onSubmit={remote} className="desktop-form">
          <label>
            {t("Connection name", "连接名称")}
            <input
              required
              value={name}
              onChange={(event) => setName(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            {t("Hub address", "Hub 地址")}
            <input
              required
              type="url"
              value={endpoint}
              onChange={(event) => setEndpoint(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            {t("Access key", "访问密钥")}
            <input
              required
              type="password"
              value={key}
              onChange={(event) => setKey(event.target.value)}
              autoComplete="off"
            />
          </label>
          {https && (
            <details className="desktop-certificate">
              <summary>{t("Advanced", "高级选项")}</summary>
              <p>
                {t(
                  "Most hubs need no extra certificate. For a hub using a private certificate, paste the PEM certificate supplied by its owner.",
                  "大多数 Hub 无需额外证书。若 Hub 使用私有证书，请粘贴其管理员提供的 PEM 证书。",
                )}
              </p>
              <label>
                {t("Private certificate (optional)", "私有证书（可选）")}
                <textarea
                  value={certificate}
                  onChange={(event) => setCertificate(event.target.value)}
                  placeholder="-----BEGIN CERTIFICATE-----"
                  spellCheck={false}
                />
              </label>
            </details>
          )}
          <button className="continue-button" disabled={busy}>
            {busy ? t("Connecting…", "正在连接…") : t("Connect", "连接")}
          </button>
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() => {
              setKey("");
              setCertificate("");
              setMode("choose");
            }}
          >
            {t("Back", "返回")}
          </button>
        </form>
      )}
      {error && (
        <p role="alert" className="connection-error">
          {error}
        </p>
      )}
    </Welcome>
  );
}
export function DesktopPreferences({
  onConnection,
  onDisconnect,
  manageLocal = false,
}: {
  onConnection?: (connection: Connection) => void;
  onDisconnect?: () => void;
  manageLocal?: boolean;
}) {
  const [locale, setLocale] = useLocale();
  const [settings, setSettings] = useState<DesktopSettings | null>(null),
    [error, setError] = useState("");
  const [busy, setBusy] = useState(false),
    [notice, setNotice] = useState("");
  const [localPort, setLocalPort] = useState(6174),
    [networkPort, setNetworkPort] = useState(6175),
    [network, setNetwork] = useState(false);
  const [revealed, setRevealed] = useState<string | null>(null),
    [showCertificate, setShowCertificate] = useState(false),
    [autostart, setAutostart] = useState(false);
  async function refresh() {
    const value = await desktopInvoke<DesktopSettings>("desktop_settings");
    syncDesktopLocale(value.language);
    setSettings(value);
    setLocalPort(value.localPort);
    setNetworkPort(value.networkPort);
    setNetwork(value.networkEnabled);
  }
  useEffect(() => {
    void refresh().catch((error) => setError(message(error)));
    void desktopInvoke<boolean>("plugin:autostart|is_enabled")
      .then(setAutostart)
      .catch(() => {});
  }, []);
  async function action(operation: () => Promise<void>) {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await operation();
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
    }
  }
  async function save(event: FormEvent) {
    event.preventDefault();
    await action(async () => {
      const session = await desktopInvoke<DesktopSession | null>(
        "desktop_save_local",
        { settings: { localPort, networkPort, networkEnabled: network } },
      );
      await refresh();
      setNotice(t("Saved", "已保存"));
      if (session && onConnection) onConnection(await desktopConnect(session));
    });
  }
  const localSettings =
    manageLocal || !settings?.active || settings.active === "local";
  const credentialId = manageLocal
    ? settings?.connections.find((connection) => connection.local)?.id
    : settings?.active;
  return (
    <div className="desktop-preferences">
      {manageLocal && (
        <div className="setting">
          <span>{t("Language", "语言")}</span>
          <select
            aria-label={t("Language", "语言")}
            value={locale}
            onChange={(event) => setLocale(event.target.value as Locale)}
          >
            <option value="en">English</option>
            <option value="zh-CN">简体中文</option>
          </select>
        </div>
      )}
      <DesktopUpdates />
      <div className="setting">
        <span>
          {t("Message notifications", "消息通知")}
          <small>
            {translateNotificationStatus(settings?.notificationStatus)}
          </small>
        </span>
        <input
          type="checkbox"
          aria-label={t("Message notifications", "消息通知")}
          checked={settings?.notificationsEnabled ?? true}
          disabled={busy || !settings}
          onChange={(event) => {
            const enabled = event.target.checked;
            void action(async () => {
              await desktopInvoke("notifications_enabled", { enabled });
              window.dispatchEvent(new Event("desktop-notifications-changed"));
              await refresh();
            });
          }}
        />
      </div>
      <div className="setting">
        <span>{t("Notification sound", "提示音")}</span>
        <input
          type="checkbox"
          aria-label={t("Notification sound", "提示音")}
          checked={settings?.notificationSoundEnabled ?? true}
          disabled={busy || !settings}
          onChange={(event) => {
            const enabled = event.target.checked;
            void action(async () => {
              await desktopInvoke("notifications_sound_enabled", { enabled });
              await refresh();
            });
          }}
        />
      </div>
      <div className="setting">
        <span>{t("Open at login", "开机自启")}</span>
        <input
          type="checkbox"
          checked={autostart}
          disabled={busy}
          onChange={(event) => {
            const enabled = event.target.checked;
            void action(async () => {
              await desktopInvoke(
                `plugin:autostart|${enabled ? "enable" : "disable"}`,
              );
              setAutostart(enabled);
            });
          }}
        />
      </div>
      {localSettings && (
        <form className="desktop-form" onSubmit={save}>
          <div className="setting">
            <span>
              {t("Allow network connections", "允许局域网连接")}
              <small>
                {t(
                  "HTTPS access for other devices using an access key.",
                  "允许其他设备使用访问密钥通过 HTTPS 连接。",
                )}
              </small>
            </span>
            <input
              type="checkbox"
              checked={network}
              disabled={busy}
              onChange={(event) => setNetwork(event.target.checked)}
            />
          </div>
          <details>
            <summary>{t("Connection ports", "连接端口")}</summary>
            <div className="desktop-port-fields">
              <label>
                {t("Local", "本地")}
                <input
                  type="number"
                  min={1024}
                  max={65535}
                  required
                  value={localPort}
                  onChange={(event) => setLocalPort(Number(event.target.value))}
                />
              </label>
              <label>
                {t("Network", "局域网")}
                <input
                  type="number"
                  min={1024}
                  max={65535}
                  required
                  value={networkPort}
                  onChange={(event) =>
                    setNetworkPort(Number(event.target.value))
                  }
                />
              </label>
            </div>
          </details>
          <button className="secondary" disabled={busy}>
            {busy ? t("Applying…", "正在应用…") : t("Apply", "应用")}
          </button>
        </form>
      )}
      {localSettings && settings?.networkEnabled && (
        <div className="desktop-connection-details">
          {settings.networkAddresses.map((address) => (
            <code key={address}>{address}</code>
          ))}
          <button
            className="text-button"
            onClick={() => setShowCertificate(!showCertificate)}
          >
            {showCertificate
              ? t("Hide certificate", "隐藏证书")
              : t("Show certificate", "显示证书")}
          </button>
          {showCertificate && (
            <textarea
              readOnly
              value={settings.certificate ?? ""}
              aria-label={t("Hub certificate", "Hub 证书")}
            />
          )}
        </div>
      )}
      {credentialId && (
        <div className="setting desktop-access-key">
          <span>{t("Access key", "访问密钥")}</span>
          <button
            className="secondary"
            disabled={busy}
            onClick={() => {
              if (revealed) setRevealed(null);
              else
                void action(async () =>
                  setRevealed(
                    await desktopInvoke<string>("desktop_reveal", {
                      id: credentialId,
                    }),
                  ),
                );
            }}
          >
            {revealed ? t("Hide", "隐藏") : t("Show", "显示")}
          </button>
          {revealed && (
            <>
              <input
                readOnly
                value={revealed}
                aria-label={t("Access key", "访问密钥")}
                autoComplete="off"
              />
              <button
                className="secondary"
                onClick={() =>
                  void action(async () => {
                    await desktopInvoke("desktop_copy_key", {
                      id: credentialId,
                    });
                    setNotice(t("Copied", "已复制"));
                  })
                }
              >
                {t("Copy", "复制")}
              </button>
            </>
          )}
        </div>
      )}
      {credentialId && (
        <button
          className="secondary"
          disabled={busy}
          onClick={() =>
            void action(async () => {
              if (
                await desktopInvoke<boolean>("desktop_export_key", {
                  id: credentialId,
                })
              )
                setNotice(
                  t("Exported a protected token file", "已导出受保护的密钥文件"),
                );
            })
          }
        >
          {t("Export key for CLI", "导出 CLI 密钥")}
        </button>
      )}
      <div className="setting">
        <span>
          {t("Command-line tool", "命令行工具")}
          <small>{settings?.cliPath}</small>
        </span>
        <button
          className="secondary"
          disabled={busy}
          onClick={() =>
            void action(async () =>
              setNotice(await desktopInvoke<string>("desktop_install_cli")),
            )
          }
        >
          {t("Install wx", "安装 wx")}
        </button>
      </div>
      <p className="desktop-help">
        {t(
          "Independent CLI connections use a protected token file. Copy an access key and configure the server with ",
          "独立 CLI 连接使用受保护的密钥文件。复制访问密钥并通过 ",
        )}
        <code>wx config server add</code>
        {t(".", " 配置服务器。")}
      </p>
      {settings?.connections
        .filter((connection) => !connection.local)
        .map((connection) => (
          <div className="setting" key={connection.id}>
            <span>
              {connection.name}
              <small>{connection.endpoint}</small>
            </span>
            <button
              className="text-button"
              disabled={busy}
              onClick={() =>
                void action(async () => {
                  await desktopInvoke("desktop_forget", { id: connection.id });
                  await refresh();
                  if (connection.id === settings.active) onDisconnect?.();
                })
              }
            >
              {t("Forget", "移除")}
            </button>
          </div>
        ))}
      {notice && <p role="status">{notice}</p>}
      {error && (
        <p role="alert" className="connection-error">
          {error}
        </p>
      )}
    </div>
  );
}
