// Native-window acceptance against an isolated desktop HOME and fresh local hub.
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import https from "node:https";
import { notificationsAcceptance } from "./notifications.mjs";
import { interfaceAcceptance } from "./interface.mjs";
const application = process.env.DESKTOP_BINARY;
if (!application) throw new Error("DESKTOP_BINARY is required");
const driver = spawn(
  process.env.TAURI_DRIVER ?? "tauri-driver",
  ["--port", "4444", "--native-port", "4445"],
  {
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, TAURI_WEBVIEW_AUTOMATION: "true" },
  }
);
let log = "";
driver.stdout.on("data", (bytes) => (log += bytes));
driver.stderr.on("data", (bytes) => (log += bytes));
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let session;
async function call(path, body, method = body === undefined ? "GET" : "POST") {
  if (process.env.DESKTOP_WEBDRIVER_TRACE) console.log(method, path);
  const response = await fetch("http://127.0.0.1:4444" + path, {
    method,
    headers: { "Content-Type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(60000),
  });
  const json = await response.json();
  if (!response.ok || json.value?.error) throw new Error(JSON.stringify(json));
  return json.value;
}
async function invokeNative(command, args = {}) {
  const result = await call("/session/" + session + "/execute/async", {
    script:
      "const done = arguments[arguments.length - 1]; window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(value => done({value}), error => done({error: String(error)}));",
    args: [command, args],
  });
  if (result.error) throw new Error(result.error);
  return result.value;
}
async function updatePreferences(expected) {
  for (let attempt = 0; attempt < 100; attempt++) {
    const value = await invokeNative("updates_status");
    if (
      value.automatic === expected.automatic &&
      value.prereleases === expected.prereleases &&
      await script(`
        const automatic = document.querySelector('#automatic-updates');
        const prereleases = document.querySelector('#prerelease-updates');
        if (!automatic && !prereleases) return true;
        return automatic?.checked === ${expected.automatic} &&
          prereleases?.checked === ${expected.prereleases} &&
          !automatic.disabled && !prereleases.disabled;
      `)
    )
      return value;
    await pause(100);
  }
  throw new Error("Update preferences were not saved");
}
async function health(expected) {
  const deadline = Date.now() + 40000;
  while (Date.now() < deadline) {
    let running = false;
    try {
      running = (
        await fetch("http://127.0.0.1:6174/health", {
          signal: AbortSignal.timeout(500),
        })
      ).ok;
    } catch {}
    if (running === expected) return;
    await pause(100);
  }
  throw new Error("The owned hub did not " + (expected ? "start" : "stop"));
}
async function script(source) {
  return call(`/session/${session}/execute/sync`, { script: source, args: [] });
}
async function until(source, expected, timeout = 20000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await script(source);
    if (expected(value)) return value;
    await pause(200);
  }
  throw new Error(
    "Native UI did not reach the expected state: " +
      (await script("return document.body.innerText"))
  );
}
try {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      await call("/status");
      break;
    } catch {
      if (attempt === 99) throw new Error(log);
      await pause(100);
    }
  }
  console.log("WebDriver ready; launching the native window");
  const created = await call("/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application } } },
  });
  session = created.sessionId;
  console.log("Native WebView session established");
  await until("return document.body.innerText", (value) =>
    value.includes("Use this device")
  );
  assert.equal(
    await script("return Boolean(window.__TAURI_INTERNALS__)"),
    true
  );
  await script(
    "[...document.querySelectorAll('button')].find(button => button.textContent.includes('Use this device')).click(); return true"
  );
  await until(
    "return document.body.innerText",
    (value) => value.includes("Add WeChat account"),
    40000
  );
  // No account is created and no WeChat endpoint is contacted.
  const text = await script("return document.body.innerText");
  assert.ok(text.includes("Sign in to WeChat"));
  assert.ok(!text.includes("access key"));

  await script(
    "[...document.querySelectorAll('button')].find(button => button.textContent.includes('Connect to a different')).click(); return true"
  );
  await until("return document.body.innerText", (value) =>
    value.includes("Desktop settings")
  );
  await script(
    "[...document.querySelectorAll('button')].find(button => button.textContent === 'Desktop settings').click(); return true"
  );
  await until("return document.body.innerText", (value) =>
    value.includes("Export key for CLI")
  );
  await until("return document.body.innerText", (value) =>
    value.includes("App updates")
  );
  const updates = await invokeNative("updates_status");
  assert.equal(updates.automatic, true);
  assert.equal(updates.prereleases, updates.currentVersion.includes("-"));
  await script(
    "document.querySelector('#automatic-updates').click(); return true"
  );
  await updatePreferences({
    automatic: false,
    prereleases: updates.prereleases,
  });
  if (updates.prereleases)
    await script(
      "document.querySelector('#prerelease-updates').click(); return true"
    );
  await updatePreferences({ automatic: false, prereleases: false });
  // A key appears only after a deliberate reveal action in this isolated fixture.
  assert.equal(
    await script(
      "return document.querySelector('input[aria-label=\"Access key\"]') === null"
    ),
    true
  );
  await script(
    "document.querySelector('.desktop-access-key button').click(); return true"
  );
  const fixtureKey = await until(
    "return document.querySelector('input[aria-label=\"Access key\"]')?.value",
    (value) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value)
  );
  await script(
    "[...document.querySelectorAll('.desktop-access-key button')].find(button => button.textContent === 'Copy').click(); return true"
  );
  await until("return document.body.innerText", (value) =>
    value.includes("Copied")
  );
  await script(
    "document.querySelector('.desktop-access-key button').click(); return true"
  );
  await script(
    "[...document.querySelectorAll('button')].find(button => button.textContent === 'Install wx').click(); return true"
  );
  await until("return document.body.innerText", (value) =>
    value.includes("Installed in ~/.local/bin")
  );
  const tokenFile = join(process.env.HOME, "desktop-cli-token");
  const clientConfig = join(process.env.HOME, "desktop-cli.toml");
  await writeFile(tokenFile, fixtureKey, { mode: 0o600, flag: "wx" });
  const command = join(process.env.HOME, ".local/bin/wx");
  const runCli = (args) =>
    JSON.parse(
      execFileSync(command, ["--json", "--config", clientConfig, ...args], {
        encoding: "utf8",
        timeout: 10000,
      })
    );
  runCli([
    "config",
    "server",
    "add",
    "desktop",
    "--url",
    "http://127.0.0.1:6174",
    "--token-file",
    tokenFile,
  ]);
  const cliStatus = runCli(["--server-name", "desktop", "status"]);
  assert.equal(cliStatus.version.backend, "native");
  console.log(
    "Installed CLI connects with its own saved server and protected token file"
  );
  await script(
    "[...document.querySelectorAll('.setting')].find(row => row.textContent.includes('Allow network connections')).querySelector('input').click(); [...document.querySelectorAll('button')].find(button => button.textContent === 'Apply').click(); return true"
  );
  await until(
    "return document.body.innerText",
    (value) => value.includes("Add WeChat account"),
    40000
  );
  const settings = await call(`/session/${session}/execute/async`, {
    script:
      "const done = arguments[arguments.length - 1]; window.__TAURI_INTERNALS__.invoke('desktop_settings').then(done, error => done({error: String(error)}));",
    args: [],
  });
  assert.equal(settings.networkEnabled, true);
  await new Promise((resolve, reject) => {
    https
      .get(
        "https://localhost:6175/health",
        { ca: settings.certificate },
        (response) => {
          response.resume();
          response.statusCode === 200
            ? resolve()
            : reject(new Error("HTTPS health check failed"));
        }
      )
      .on("error", reject);
  });
  if (process.platform === "linux") await notificationsAcceptance(invokeNative);
  try {
    await script(
      "window.__TAURI_INTERNALS__.invoke('desktop_quit'); return true"
    );
  } catch {}
  await health(false);
  try {
    await call(`/session/${session}`, undefined, "DELETE");
  } catch {}
  session = undefined;
  const reopened = await call("/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application } } },
  });
  session = reopened.sessionId;
  await until(
    "return document.body.innerText",
    (value) => value.includes("Add WeChat account"),
    40000
  );
  await health(true);
  await updatePreferences({ automatic: false, prereleases: false });
  await invokeNative("updates_preferences", {
    preferences: { automatic: true, prereleases: true },
  });
  console.log(
    "Native update settings and release channel persist across restart"
  );
  await interfaceAcceptance({
    script,
    invoke: invokeNative,
    until,
    resize: (width, height) =>
      call(`/session/${session}/window/rect`, { width, height }),
    capture: process.env.DESKTOP_INTERFACE_SCREENSHOT
      ? async () =>
          writeFile(
            process.env.DESKTOP_INTERFACE_SCREENSHOT,
            Buffer.from(await call(`/session/${session}/screenshot`), "base64"),
          )
      : undefined,
  });
  try {
    await script(
      "window.__TAURI_INTERNALS__.invoke('desktop_quit'); return true"
    );
  } catch {}
  await health(false);
  console.log(
    "Native desktop: keyring setup and persistence, local hub startup, UI transport, HTTPS settings, restart and graceful quit passed"
  );
} finally {
  if (session) {
    try {
      await script(
        "window.__TAURI_INTERNALS__.invoke('desktop_quit'); return true"
      );
    } catch {}
    try {
      await call(`/session/${session}`, undefined, "DELETE");
    } catch {}
  }
  driver.kill("SIGTERM");
  if (log && process.env.DESKTOP_DRIVER_LOG)
    await writeFile(process.env.DESKTOP_DRIVER_LOG, log);
}
