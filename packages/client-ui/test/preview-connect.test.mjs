import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { connect as netConnect } from "node:net";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

function rawUpgrade(port, path, extraHeaders = {}) {
  return new Promise((resolve, reject) => {
    const socket = netConnect(port, "127.0.0.1");
    let response = "";
    const headers = {
      Host: `127.0.0.1:${port}`,
      Upgrade: "websocket",
      Connection: "Upgrade",
      "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
      "Sec-WebSocket-Version": "13",
      ...extraHeaders,
    };
    socket.once("error", reject);
    socket.on("data", (chunk) => {
      response += chunk.toString("latin1");
      if (response.includes("\r\n\r\n")) {
        const statusLine = response.split("\r\n")[0] ?? "";
        const match = statusLine.match(/^HTTP\/\d\.\d (\d{3})/);
        socket.destroy();
        resolve({ status: match ? Number(match[1]) : 0, raw: response });
      }
    });
    socket.on("end", () => {
      const statusLine = response.split("\r\n")[0] ?? "";
      const match = statusLine.match(/^HTTP\/\d\.\d (\d{3})/);
      resolve({ status: match ? Number(match[1]) : 0, raw: response });
    });
    const req = [
      `GET ${path} HTTP/1.1`,
      ...Object.entries(headers).map(([k, v]) => `${k}: ${v}`),
      "",
      "",
    ].join("\r\n");
    socket.write(req);
  });
}

test("two private preview hubs keep credentials separate and remain selectable when one is offline", { timeout: 25000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), "preview-hubs-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const requests = [];
  const upgrades = [];
  const servers = [];
  const targets = [];
  for (const id of ["one", "two"]) {
    const token = `synthetic-${id}-key`;
    const tokenFile = join(directory, `${id}.key`);
    await writeFile(tokenFile, token, { mode: 0o600 });
    const server = createServer(async (req, res) => {
      requests.push({ id, path: req.url, auth: req.headers.authorization, key: req.headers["idempotency-key"] });
      assert.equal(req.headers.authorization, `Bearer ${token}`);
      res.setHeader("Content-Type", "application/json");
      if (req.url.includes("events/stream")) { res.setHeader("Content-Type", "text/event-stream"); return res.end('data: {"cursor":"1"}\n\n'); }
      if (req.headers.range) { res.statusCode = 206; res.setHeader("Content-Range", "bytes 0-3/4"); return res.end("data"); }
      if (req.method === "POST") {
        let body = ""; for await (const chunk of req) body += chunk;
        return res.end(JSON.stringify({ id, body: JSON.parse(body) }));
      }
      res.end(JSON.stringify({ serverId: id, version: { backend: "native" } }));
    });
    server.on("upgrade", (req, socket) => {
      upgrades.push({ id, url: req.url, auth: req.headers.authorization, origin: req.headers.origin });
      socket.end(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n",
      );
    });
    await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
    servers.push(server);
    targets.push({ id, name: `Hub ${id}`, server: `http://127.0.0.1:${server.address().port}`, endpoint: `https://${id}.example`, tokenFile });
    t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  }
  const config = join(directory, "connections.json");
  await writeFile(config, JSON.stringify(targets), { mode: 0o600 });
  const reservation = createServer(); await new Promise(resolve => reservation.listen(0, "127.0.0.1", resolve));
  const port = reservation.address().port; await new Promise(resolve => reservation.close(resolve));
  const child = spawn(process.execPath, [fileURLToPath(new URL("../scripts/connect.mjs", import.meta.url)), "--connections-file", config, "--state-dir", directory], {
    env: { PATH: process.env.PATH, HOME: directory, CLIENT_UI_PORT: String(port), CLIENT_UI_ORIGIN: "" }, stdio: ["ignore", "pipe", "pipe"],
  });
  t.after(async () => {
    if (child.exitCode !== null || child.signalCode !== null) return;
    const exited = new Promise(resolve => child.once("exit", resolve));
    child.kill("SIGTERM");
    // A failed assertion may leave a proxy connection open. Teardown must still
    // finish so the test runner can report the original failure instead of hanging.
    const forceStop = setTimeout(() => child.kill("SIGKILL"), 2000);
    try { await exited; } finally { clearTimeout(forceStop); }
  });
  let output = "", diagnostics = "";
  child.stderr.on("data", chunk => { diagnostics += chunk; });
  const ready = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(Error(`Preview startup failed: ${diagnostics.slice(-1000)}`)), 15000);
    child.once("exit", () => { clearTimeout(timeout); reject(Error(`Preview exited: ${diagnostics.slice(-1000)}`)); });
    child.stdout.on("data", chunk => { output += chunk; const match = output.match(/(http:\/\/127\.0\.0\.1:\d+)\/#dev=([a-f0-9]{64})/); if (match) { clearTimeout(timeout); resolve({ origin: match[1], key: match[2] }); } });
  });
  const headers = { Authorization: `Bearer ${ready.key}` };
  assert.equal((await fetch(`${ready.origin}/__dev/connections`)).status, 403);
  const hubs = await (await fetch(`${ready.origin}/__dev/connections`, { headers })).json();
  assert.deepEqual(hubs, targets.map(({ id, name, endpoint }) => ({ id, name, endpoint })));
  assert.ok(!JSON.stringify(hubs).includes("token"));
  for (const id of ["one", "two"]) {
    const session = await fetch(`${ready.origin}/__dev/session?hub=${id}`, { headers });
    const data = await session.json();
    assert.equal(data.endpoint, `https://${id}.example`); assert.equal(data.proxyPath, `/__hubs/${id}`);
    const prefix = `${ready.origin}${data.proxyPath}`;
    assert.equal((await (await fetch(`${prefix}/v1/status`, { headers })).json()).serverId, id);
    const sent = await (await fetch(`${prefix}/v1/accounts/a/sends`, { method: "POST", headers: { ...headers, "Idempotency-Key": "stable-key" }, body: '{"text":"fixture"}' })).json();
    assert.deepEqual(sent, { id, body: { text: "fixture" } });
    assert.equal((await fetch(`${prefix}/v1/accounts/a/events/stream`, { headers })).headers.get("content-type"), "text/event-stream");
    assert.equal((await fetch(`${prefix}/v1/accounts/a/media/m/content`, { headers: { ...headers, Range: "bytes=0-3" } })).status, 206);
    const before = requests.length;
    for (const [path, options] of [["/v1/tokens", { headers }], ["/v1/daemon/shutdown", { method: "POST", headers }],
      ["/v1/status", { headers: { ...headers, Origin: "https://untrusted.example" } }], ["/v1/status", {}]]) {
      assert.ok([401, 403].includes((await fetch(prefix + path, options)).status));
    }
    assert.equal(requests.length, before);
  }
  assert.equal((await fetch(`${ready.origin}/__hubs/unknown/v1/status`, { headers })).status, 404);
  assert.ok(requests.filter(item => item.key).every(item => item.key === "stable-key"));

  // WebSocket upgrades allow only ticket-authenticated calls sockets from the preview origin.
  const ticket = "a".repeat(64);
  const okControl = await rawUpgrade(port, `/__hubs/one/v1/accounts/a/calls/socket?ticket=${ticket}`, { Origin: ready.origin });
  assert.equal(okControl.status, 101);
  const okMedia = await rawUpgrade(port, `/__hubs/two/v1/accounts/a/calls/call-1/media?ticket=${ticket}`, { Origin: ready.origin });
  assert.equal(okMedia.status, 101);
  assert.equal(upgrades.length, 2);
  assert.equal(upgrades[0].id, "one");
  assert.equal(upgrades[0].url, `/v1/accounts/a/calls/socket?ticket=${ticket}`);
  assert.equal(upgrades[0].auth, undefined);
  assert.equal(upgrades[0].origin, undefined);
  assert.equal(upgrades[1].id, "two");
  assert.equal(upgrades[1].url, `/v1/accounts/a/calls/call-1/media?ticket=${ticket}`);

  const beforeBlocked = upgrades.length;
  for (const [badPath, badHeaders, expectedStatus] of [
    [`/__hubs/one/v1/accounts/a/calls/socket`, { Origin: ready.origin }, 403],
    [`/__hubs/one/v1/accounts/a/calls/socket?ticket=short`, { Origin: ready.origin }, 403],
    [`/__hubs/one/v1/accounts/a/calls/socket?ticket=${ticket}`, { Origin: "https://untrusted.example" }, 403],
    [`/__hubs/one/v1/status?ticket=${ticket}`, { Origin: ready.origin }, 403],
    [`/__hubs/unknown/v1/accounts/a/calls/socket?ticket=${ticket}`, { Origin: ready.origin }, 404],
  ]) {
    const res = await rawUpgrade(port, badPath, badHeaders);
    assert.equal(res.status, expectedStatus, `expected ${expectedStatus} for ${badPath}`);
  }
  assert.equal(upgrades.length, beforeBlocked);

  servers[1].closeAllConnections(); await new Promise(resolve => servers[1].close(resolve));
  assert.equal((await fetch(`${ready.origin}/__hubs/two/v1/status`, { headers })).status, 500);
  assert.equal((await (await fetch(`${ready.origin}/__hubs/one/v1/status`, { headers })).json()).serverId, "one");
});
