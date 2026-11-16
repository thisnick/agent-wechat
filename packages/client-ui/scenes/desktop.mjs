// Stands in for the desktop app's native side so scenes can show desktop-only
// screens in a plain browser. The UI detects the desktop app through
// `globalThis.isTauri` and calls native commands through
// `window.__TAURI_INTERNALS__.invoke`; this answers those calls from fixed data.

/**
 * Desktop settings as `desktop_settings` returns them after an earlier run of the
 * app left its hub running on the local port.
 */
export const LEFTOVER_HUB_SETTINGS = {
  connections: [
    { id: "local", name: "This device", endpoint: "http://127.0.0.1:6174", local: true },
    { id: "lab", name: "Lab", endpoint: "https://lab.example:8448", local: false },
  ],
  active: "local",
  localPort: 6174,
  networkPort: 6175,
  networkEnabled: false,
  networkAddresses: [],
  certificate: null,
  cliPath: "/Applications/AgentWechat.app/Contents/MacOS/wx",
  notificationsEnabled: true,
  notificationSoundEnabled: true,
  notificationStatus: "Connect to a hub to receive notifications",
  keepLocalHubRunning: false,
  localHubError: "An earlier AgentWechat session left its hub running on port 6174",
  leftoverHub: true,
};

/**
 * Makes the page look like the desktop app before it loads. `bootstrapError`, when
 * set, is how opening the selected hub fails; `settings` answers `desktop_settings`.
 * Any other command resolves to `null`.
 */
export async function desktopBridge(page, { bootstrapError = null, settings }) {
  await page.addInitScript(
    ({ bootstrapError, settings }) => {
      let callbacks = 0;
      globalThis.isTauri = true;
      window.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
        transformCallback: () => ++callbacks,
        unregisterCallback: () => {},
        convertFileSrc: (path) => path,
        invoke: async (command) => {
          if (command === "desktop_bootstrap") {
            if (bootstrapError) throw bootstrapError;
            return null;
          }
          if (command === "desktop_settings") return settings;
          if (command === "plugin:autostart|is_enabled") return false;
          return null;
        },
      };
    },
    { bootstrapError, settings },
  );
}
