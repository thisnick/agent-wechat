import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

test("two private preview hubs keep credentials separate and remain selectable when one is offline", { timeout: 25000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), "preview-hubs-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const requests = [];
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
  servers[1].closeAllConnections(); await new Promise(resolve => servers[1].close(resolve));
  assert.equal((await fetch(`${ready.origin}/__hubs/two/v1/status`, { headers })).status, 500);
  assert.equal((await (await fetch(`${ready.origin}/__hubs/one/v1/status`, { headers })).json()).serverId, "one");
});
