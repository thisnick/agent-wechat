// Real session model + SDK + owned Rust simulator processes; never loads native client defaults.
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { spawn } from "node:child_process";
import { readFile, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { NativeWeChatClient } from "../packages/shared/dist/index.js";
import { HubSession } from "../packages/client-ui/src/session.ts";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary =
  process.env.WX_DEV_BINARY ??
  join(
    root,
    "packages/native-client-rust/target/debug",
    process.platform === "win32" ? "wx-dev.exe" : "wx-dev",
  );
const data = await mkdtemp(join(tmpdir(), "agentwechat-client-test-"));
const children = new Set(),
  sessions = [];
let upstream, hub, token, controlToken, client;
async function wait(label, condition, timeout = 30000) {
  const deadline = Date.now() + timeout;
  let last;
  while (Date.now() < deadline) {
    try {
      const result = await condition();
      if (result) return result;
    } catch (e) {
      last = e;
    }
    await new Promise((done) => setTimeout(done, 40));
  }
  throw new Error(`Timed out: ${label}`, { cause: last });
}
async function start(command, args = []) {
  const child = spawn(binary, [command, "--data-dir", data, ...args], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.add(child);
  child.once("exit", () => children.delete(child));
  let diagnostic = "";
  child.stderr.on(
    "data",
    (data) => (diagnostic = (diagnostic + data).slice(-2000)),
  );
  return new Promise((done, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error("Startup timeout")), 30000);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`${command} stopped (${code}): ${diagnostic}`));
    });
    child.stdout.on("data", (data) => {
      output += data;
      if (output.includes("\n")) {
        clearTimeout(timer);
        try {
          done({ child, ...JSON.parse(output.split("\n")[0]) });
        } catch (e) {
          reject(e);
        }
      }
    });
  });
}
async function control(body) {
  const result = await fetch(`http://${upstream.upstream}/control`, {
    method: "POST",
    signal: AbortSignal.timeout(5000),
    headers: {
      Authorization: `Bearer ${controlToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  assert.equal(result.status, 200);
  return result.json();
}
async function state() {
  return (
    await fetch(`http://${upstream.upstream}/state`, {
      headers: { Authorization: `Bearer ${controlToken}` },
    })
  ).json();
}
async function login(account) {
  await client.createAccount(account);
  const op = await client.login(
    account,
    { strategy: "qr", qrRefresh: { mode: "manual", maxRefreshes: 0 } },
    `login-${account}`,
  );
  await wait(
    "QR",
    async () =>
      (await client.operation(account, op.id)).state === "awaiting_scan",
  );
  await control({ op: "login", account, status: "approved" });
  await wait(
    "login",
    async () => (await client.operation(account, op.id)).state === "succeeded",
  );
  await wait(
    "baseline",
    async () => (await client.messages(account, "sim_mei")).items.length === 1,
  );
}
function session(api = client) {
  const result = new HubSession(api);
  sessions.push(result);
  return result;
}
async function stopHub() {
  const status = await client.status();
  await client.request("v1/daemon/shutdown", {
    method: "POST",
    body: JSON.stringify({
      pid: status.pid,
      serverId: status.serverId,
      ownershipGeneration: status.ownershipGeneration,
    }),
  });
  await wait(
    "owned hub stopped",
    () => hub.child.exitCode !== null || hub.child.signalCode !== null,
  );
}
async function launcherCheck() {
  const reservation = createServer();
  await new Promise((done) => reservation.listen(0, "127.0.0.1", done));
  const port = reservation.address().port;
  await new Promise((done) => reservation.close(done));
  const child = spawn(
    process.execPath,
    [join(root, "packages/client-ui/scripts/dev.mjs")],
    {
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        WX_DEV_BINARY: binary,
        CLIENT_UI_PORT: String(port),
        CLIENT_UI_ORIGIN: "",
      },
    },
  );
  let diagnostics = "",
    output = "";
  child.stderr.on(
    "data",
    (chunk) => (diagnostics = (diagnostics + chunk).slice(-2000)),
  );
  try {
    const ready = await new Promise((done, reject) => {
      const timer = setTimeout(
        () => reject(new Error("UI launcher timeout: " + diagnostics)),
        30000,
      );
      child.once("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.once("exit", () => {
        clearTimeout(timer);
        reject(new Error("UI launcher stopped: " + diagnostics));
      });
      child.stdout.on("data", (chunk) => {
        output += chunk;
        const match = output.match(
          /(http:\/\/127\.0\.0\.1:\d+)\/#dev=([0-9a-f]{64})/,
        );
        if (match) {
          clearTimeout(timer);
          done({ endpoint: match[1], key: match[2] });
        }
      });
    });
    assert.equal((await fetch(ready.endpoint + "/__dev/session")).status, 403);
    assert.equal(
      (
        await fetch(ready.endpoint + "/__dev/control", {
          method: "POST",
          body: "{}",
        })
      ).status,
      403,
    );
    const authorization = { Authorization: `Bearer ${ready.key}` };
    assert.equal(
      (
        await fetch(ready.endpoint + "/__dev/session", {
          headers: { ...authorization, Origin: "https://unrelated.example" },
        })
      ).status,
      403,
    );
    const response = await fetch(ready.endpoint + "/__dev/session", {
      headers: authorization,
    });
    assert.equal(response.status, 200);
    assert.match(
      response.headers.get("set-cookie"),
      /HttpOnly; SameSite=Strict/,
    );
    const configuration = await response.json();
    assert.equal(configuration.simulator, true);
    const api = new NativeWeChatClient({
      baseUrl: configuration.endpoint,
      token: configuration.token,
    });
    const status = await api.status();
    assert.equal(status.version.backend, "simulator");
    assert.deepEqual((await api.accounts()).items, []);
    const directory = output.match(/Simulator data: (.+)/)?.[1];
    assert.ok(directory);
    child.kill("SIGTERM");
    await wait(
      "UI launcher shutdown",
      () => child.exitCode !== null || child.signalCode !== null,
    );
    assert.equal(child.exitCode, 0);
    await assert.rejects(stat(directory), (error) => error.code === "ENOENT");
    assert.throws(
      () => process.kill(status.pid, 0),
      (error) => error.code === "ESRCH",
    );
    await assert.rejects(fetch(ready.endpoint + "/v1/status"));
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await new Promise((done) => child.once("exit", done));
    }
  }
}

try {
  upstream = await start("upstream");
  controlToken = (await readFile(upstream.controlTokenFile, "utf8")).trim();
  hub = await start("hub", ["--upstream", upstream.upstream]);
  token = (await readFile(hub.tokenFile, "utf8")).trim();
  client = new NativeWeChatClient({ baseUrl: hub.endpoint, token });
  assert.equal((await client.status()).version.backend, "simulator");
  await login("personal");
  await login("work");
  const a = session(),
    b = session();
  await Promise.all([a.activate("personal"), b.activate("personal")]);
  await a.send("A real session-model send");
  await wait("two session clients render accepted text", () =>
    [a, b].every((s) =>
      s
        .snapshot()
        .messages.some(
          (m) =>
            m.body === "A real session-model send" &&
            m.direction === "outgoing",
        ),
    ),
  );
  assert.equal(a.snapshot().sends.length, 0);
  await control({
    op: "inbound",
    account: "personal",
    peer: "sim_mei",
    body: "A real incoming push",
  });
  await wait("two session clients render inbound push", () =>
    [a, b].every((s) =>
      s.snapshot().messages.some((m) => m.body === "A real incoming push"),
    ),
  );
  await control({
    op: "inbound",
    account: "personal",
    peer: "sim_mei",
    body: "A phone self-send",
    self_sent: true,
  });
  await wait("outgoing live self-send", () =>
    [a, b].every((s) =>
      s
        .snapshot()
        .messages.some(
          (m) => m.body === "A phone self-send" && m.direction === "outgoing",
        ),
    ),
  );

  await control({
    op: "send_fault",
    account: "personal",
    fault: "lost_acknowledgment",
  });
  await a.send("Accepted upstream, acknowledgment lost");
  await wait("uncertain receipts in both clients", () =>
    [a, b].every((s) =>
      s.snapshot().sends.some((send) => send.intent?.state === "needs_review"),
    ),
  );
  const uncertain = a
    .snapshot()
    .sends.find((send) => send.intent?.state === "needs_review");
  const accepted = (await state()).accounts.find(
    (x) => x.id === "personal",
  ).acceptedSends;
  await a.checkSend(uncertain.key);
  assert.equal(
    (await state()).accounts.find((x) => x.id === "personal").acceptedSends,
    accepted,
  );
  assert.equal(
    a.snapshot().sends.find((s) => s.key === uncertain.key).intent.state,
    "needs_review",
  );

  await control({ op: "send_fault", account: "personal", fault: "reject" });
  await a.send("A rejected send");
  await wait("rejection remains visible", () =>
    a
      .snapshot()
      .sends.some(
        (s) =>
          s.text === "A rejected send" &&
          ["rejected", "failed"].includes(s.intent?.state),
      ),
  );
  await control({
    op: "advertise_image",
    account: "personal",
    peer: "sim_mei",
  });
  const image = await wait("media message", () =>
    a.snapshot().messages.find((m) => m.type === 3),
  );
  assert.ok(
    (await client.mediaAssets("personal", image.id)).items.every(
      (asset) => asset.state !== "available",
    ),
  );

  let release, requested;
  const held = new Promise((done) => {
    release = done;
  });
  const started = new Promise((done) => {
    requested = done;
  });
  let hold = true;
  const delayed = new NativeWeChatClient({
    baseUrl: hub.endpoint,
    token,
    fetch: async (input, init) => {
      const response = await fetch(input, init);
      if (hold && new URL(input).pathname === "/v1/accounts/personal/threads") {
        hold = false;
        requested();
        await held;
      }
      return response;
    },
  });
  const switching = session(delayed);
  const old = switching.activate("personal");
  await started;
  await switching.activate("work");
  release();
  await old;
  assert.equal(switching.snapshot().account, "work");
  assert.ok(switching.snapshot().messages.every((m) => m.accountId === "work"));
  assert.ok(
    !switching
      .snapshot()
      .messages.some((m) => m.body === "A real incoming push"),
  );

  const originalServer = (await client.status()).serverId;
  const listen = new URL(hub.endpoint).host;
  await stopHub();
  await control({
    op: "inbound",
    account: "personal",
    peer: "sim_mei",
    body: "Catch-up after hub restart",
    notify: false,
  });
  hub = await start("hub", [
    "--upstream",
    upstream.upstream,
    "--listen",
    listen,
  ]);
  assert.equal((await client.status()).serverId, originalServer);
  await wait(
    "both clients replay after restart",
    () =>
      [a, b].every((s) =>
        s
          .snapshot()
          .messages.some((m) => m.body === "Catch-up after hub restart"),
      ),
    60000,
  );
  assert.equal(
    a.snapshot().messages.filter((m) => m.body === "A real session-model send")
      .length,
    1,
  );
  await launcherCheck();
  console.log(
    JSON.stringify({
      ok: true,
      checks: [
        "real session model with two hub clients",
        "canonical incoming and outgoing content",
        "phone-originated self-send",
        "accepted/rejected/uncertain send states",
        "status lookup without resubmission",
        "advertised media unavailable",
        "late real HTTP responses across account switch",
        "SSE reconnect and replay across hub restart",
        "UI launcher authentication and origin checks",
        "UI launcher owned-child and disposable-state cleanup",
      ],
    }),
  );
} finally {
  for (const s of sessions) s.close();
  if (client && hub) await stopHub().catch(() => {});
  if (upstream && controlToken)
    await fetch(`http://${upstream.upstream}/shutdown`, {
      method: "POST",
      headers: { Authorization: `Bearer ${controlToken}` },
    }).catch(() => {});
  for (const child of children) child.kill();
  await Promise.all(
    [...children].map(
      (child) =>
        new Promise((done) => {
          if (child.exitCode !== null || child.signalCode !== null) done();
          else child.once("exit", done);
        }),
    ),
  );
  await rm(data, { recursive: true, force: true });
}
