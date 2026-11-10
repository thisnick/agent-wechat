import { DesktopUpdates } from "./desktop-updates.tsx";
import { useEffect, useState, type FormEvent } from "react";
import {
  desktopConnect,
  desktopInvoke,
  type DesktopSession,
  type DesktopSettings,
} from "./desktop.ts";
import type { Connection } from "./connection.ts";
import { richText, t } from "./i18n.ts";
import { Welcome } from "./ui.tsx";
const message = (error: unknown) =>
  typeof error === "string"
    ? error
    : error instanceof Error
    ? error.message
    : t("desktopUi.operationCouldNotCompleted");
function translateNotificationStatus(status?: string): string {
  switch (status) {
    case "Listening for new messages":
      return t("desktopUi.listeningNewMessages");
    case "Waiting for the hub":
      return t("desktopUi.waitingHub");
    case "Connect to a hub to receive notifications":
      return t("desktopUi.connectHubReceiveNotifications");
    case "Notifications off":
      return t("desktopUi.notificationsOff");
    default:
      return (
        status || t("desktopUi.newMessagesUnmutedChats")
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
          <h1>{t("desktopUi.desktopSettings")}</h1>
        </div>
        <DesktopPreferences onConnection={onConnect} manageLocal />
        <button className="secondary" onClick={() => setMode("choose")}>
          {t("desktopUi.back")}
        </button>
      </Welcome>
    );
  return (
    <Welcome simulator={false}>
      <div className="welcome-heading">
        <h1>
          {mode === "remote"
            ? t("desktopUi.connectHub")
            : t("desktopUi.welcomeAgentwechat")}
        </h1>
        <p>
          {mode === "remote"
            ? t("desktopUi.saveConnectionSystemKeyring")
            : t("desktopUi.useDeviceConnectExistingAgent")}
        </p>
      </div>
      {mode === "choose" ? (
        <div className="desktop-choices">
          <button
            className="continue-button"
            disabled={busy}
            onClick={() => void open("desktop_local")}
          >
            {busy ? t("desktopUi.opening") : t("desktopUi.useDevice")}
          </button>
          <button
            className="secondary"
            disabled={busy}
            onClick={() => setMode("remote")}
          >
            {t("desktopUi.connectAnotherHub")}
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
            {t("desktopUi.desktopSettings")}
          </button>
        </div>
      ) : (
        <form onSubmit={remote} className="desktop-form">
          <label>
            {t("desktopUi.connectionName")}
            <input
              required
              value={name}
              onChange={(event) => setName(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            {t("desktopUi.hubAddress")}
            <input
              required
              type="url"
              value={endpoint}
              onChange={(event) => setEndpoint(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            {t("desktopUi.accessKey")}
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
              <summary>{t("desktopUi.advanced")}</summary>
              <p>
                {t("desktopUi.mostHubsNeedNoExtra")}
              </p>
              <label>
                {t("desktopUi.privateCertificateOptional")}
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
            {busy ? t("desktopUi.connecting") : t("desktopUi.connect")}
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
            {t("desktopUi.back")}
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
      setNotice(t("desktopUi.saved"));
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
      <DesktopUpdates />
      <div className="setting">
        <span>
          {t("desktopUi.messageNotifications")}
          <small>
            {translateNotificationStatus(settings?.notificationStatus)}
          </small>
        </span>
        <input
          type="checkbox"
          aria-label={t("desktopUi.messageNotifications")}
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
        <span>{t("desktopUi.notificationSound")}</span>
        <input
          type="checkbox"
          aria-label={t("desktopUi.notificationSound")}
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
        <span>{t("desktopUi.openLogin")}</span>
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
              {t("desktopUi.allowNetworkConnections")}
              <small>
                {t("desktopUi.httpsAccessOtherDevicesUsing")}
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
            <summary>{t("desktopUi.connectionPorts")}</summary>
            <div className="desktop-port-fields">
              <label>
                {t("desktopUi.local")}
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
                {t("desktopUi.network")}
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
            {busy ? t("desktopUi.applying") : t("desktopUi.apply")}
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
              ? t("desktopUi.hideCertificate")
              : t("desktopUi.showCertificate")}
          </button>
          {showCertificate && (
            <textarea
              readOnly
              value={settings.certificate ?? ""}
              aria-label={t("desktopUi.hubCertificate")}
            />
          )}
        </div>
      )}
      {credentialId && (
        <div className="setting desktop-access-key">
          <span>{t("desktopUi.accessKey")}</span>
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
            {revealed ? t("desktopUi.hide") : t("desktopUi.show")}
          </button>
          {revealed && (
            <>
              <input
                readOnly
                value={revealed}
                aria-label={t("desktopUi.accessKey")}
                autoComplete="off"
              />
              <button
                className="secondary"
                onClick={() =>
                  void action(async () => {
                    await desktopInvoke("desktop_copy_key", {
                      id: credentialId,
                    });
                    setNotice(t("desktopUi.copied"));
                  })
                }
              >
                {t("desktopUi.copy")}
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
                  t("desktopUi.exportedProtectedTokenFile"),
                );
            })
          }
        >
          {t("desktopUi.exportKeyCli")}
        </button>
      )}
      <div className="setting">
        <span>
          {t("desktopUi.commandLineTool")}
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
          {t("desktopUi.installWx")}
        </button>
      </div>
      <p className="desktop-help">
        {richText("desktopUi.cliConnectionsHelp", {
          command: <code>wx config server add</code>,
        })}
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
              {t("desktopUi.forget")}
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
