import { useEffect, useState, type FormEvent } from "react";
import {
  desktopConnect,
  desktopInvoke,
  type DesktopSession,
  type DesktopSettings,
} from "./desktop.ts";
import type { Connection } from "./connection.ts";
import { Welcome } from "./ui.tsx";
const message = (error: unknown) =>
  typeof error === "string"
    ? error
    : error instanceof Error
      ? error.message
      : "The operation could not be completed.";
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
  useEffect(() => {
    void desktopInvoke<DesktopSettings>("desktop_settings")
      .then(setSettings)
      .catch((error) => setError(message(error)));
  }, []);
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
      certificate: certificate.trim() || null,
    });
  }
  if (mode === "settings")
    return (
      <Welcome simulator={false}>
        <h1>Desktop settings</h1>
        <DesktopPreferences onConnection={onConnect} manageLocal />
        <button className="secondary" onClick={() => setMode("choose")}>
          Back
        </button>
      </Welcome>
    );
  return (
    <Welcome simulator={false}>
      <div className="welcome-heading">
        <h1>
          {mode === "remote" ? "Connect to a hub" : "Welcome to AgentWechat"}
        </h1>
        <p>
          {mode === "remote"
            ? "Save this connection in your system keyring."
            : "Use this device, or connect to an existing agent-wechat hub."}
        </p>
      </div>
      {mode === "choose" ? (
        <div className="desktop-choices">
          <button
            className="continue-button"
            disabled={busy}
            onClick={() => void open("desktop_local")}
          >
            {busy ? "Opening…" : "Use this device"}
          </button>
          <button
            className="secondary"
            disabled={busy}
            onClick={() => setMode("remote")}
          >
            Connect to another hub
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
            Desktop settings
          </button>
        </div>
      ) : (
        <form onSubmit={remote} className="desktop-form">
          <label>
            Connection name
            <input
              required
              value={name}
              onChange={(event) => setName(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            Hub address
            <input
              required
              type="url"
              value={endpoint}
              onChange={(event) => setEndpoint(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            Access key
            <input
              required
              type="password"
              value={key}
              onChange={(event) => setKey(event.target.value)}
              autoComplete="off"
            />
          </label>
          <details>
            <summary>Trust a hub certificate</summary>
            <p>
              For a hub with its own certificate, paste the certificate supplied
              by its owner.
            </p>
            <textarea
              value={certificate}
              onChange={(event) => setCertificate(event.target.value)}
              placeholder="-----BEGIN CERTIFICATE-----"
              aria-label="Trusted hub certificate"
            />
          </details>
          <button className="continue-button" disabled={busy}>
            {busy ? "Connecting…" : "Connect"}
          </button>
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() => {
              setKey("");
              setMode("choose");
            }}
          >
            Back
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
      setNotice("Saved");
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
      <div className="setting">
        <span>Open at login</span>
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
              Allow network connections
              <small>HTTPS access for other devices using an access key.</small>
            </span>
            <input
              type="checkbox"
              checked={network}
              disabled={busy}
              onChange={(event) => setNetwork(event.target.checked)}
            />
          </div>
          <details>
            <summary>Connection ports</summary>
            <div className="desktop-port-fields">
              <label>
                Local
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
                Network
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
            {busy ? "Applying…" : "Apply"}
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
            {showCertificate ? "Hide certificate" : "Show certificate"}
          </button>
          {showCertificate && (
            <textarea
              readOnly
              value={settings.certificate ?? ""}
              aria-label="Hub certificate"
            />
          )}
        </div>
      )}
      {credentialId && (
        <div className="setting desktop-access-key">
          <span>Access key</span>
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
            {revealed ? "Hide" : "Show"}
          </button>
          {revealed && (
            <>
              <input
                readOnly
                value={revealed}
                aria-label="Access key"
                autoComplete="off"
              />
              <button
                className="secondary"
                onClick={() =>
                  void action(async () => {
                    await navigator.clipboard.writeText(revealed);
                    setNotice("Copied");
                  })
                }
              >
                Copy
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
                setNotice("Exported a protected token file");
            })
          }
        >
          Export key for CLI
        </button>
      )}
      <div className="setting">
        <span>
          Command-line tool<small>{settings?.cliPath}</small>
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
          Install wx
        </button>
      </div>
      <p className="desktop-help">
        Independent CLI connections use a protected token file. Copy an access
        key and configure the server with <code>wx config server add</code>.
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
              Forget
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
