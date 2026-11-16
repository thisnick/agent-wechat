import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { managedServeArgs, nativeChildEnvironment, nativeExecutableFormat, nativeHostTarget, resolveNativeExecutable, startNativeHost } from "./native-host.ts";
async function fixture(t: any) {
  const dir = await mkdtemp(join(tmpdir(), "native-host-")); t.after(() => rm(dir, { recursive: true, force: true }));
  return { dir, token: "fixture-".repeat(8) };
}
test("managed child environment excludes token and unrelated OpenClaw credentials", () => {
  const token = "fixture-".repeat(8);
  assert.deepEqual(nativeChildEnvironment(token, { PATH: "/usr/bin", HOME: "/fixture", AGENT_WECHAT_TOKEN: token,
    OTHER_PROVIDER_TOKEN: "other", OPENCLAW_CONFIG_PATH: "/private/config", LANG: token }), { PATH: "/usr/bin", HOME: "/fixture" });
});
test("trusted PATH ignores current and relative directories and never installs a runtime", async t => {
  await assert.rejects(resolveNativeExecutable(undefined, ":.:relative"), /native_binary_missing/);
  await assert.rejects(resolveNativeExecutable("wx"), /absolute/);
  const { dir } = await fixture(t); const script = join(dir, "wx"); await writeFile(script, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  await assert.rejects(resolveNativeExecutable(script), /native_binary_required/);
});
test("external mode only authenticates existing daemon and stop never logs out or spawns", async t => {
  const { dir, token } = await fixture(t); const requests: string[] = [];
  const server = createServer((req, res) => { requests.push(req.url!); assert.equal(req.headers.authorization, `Bearer ${"fixture-".repeat(8)}`);
    res.setHeader("content-type", "application/json"); res.end(JSON.stringify({ version: { product: "agent-wechat", backend: "native", version: "0.16.0", apiVersion: 1, capabilities: ["serve", "events", "profile-read", "text-send", "native-login"] }, serverId: "server" })); });
  server.listen(0, "127.0.0.1"); await once(server, "listening"); t.after(() => new Promise<void>(done => { server.closeAllConnections(); server.close(() => done()); }));
  const endpoint = `http://127.0.0.1:${(server.address() as any).port}`;
  const host = await startNativeHost({ mode: "external", endpoint, accountId: "a", token, consumerStateDir: dir });
  await host.forward("v1/status", { headers: { Authorization: "Bearer gateway-fixture" } });
  await host.stop(); assert.deepEqual(requests, ["/v1/status", "/v1/status"]);
  assert.throws(() => host.forward("https://foreign.invalid/", {}), /Invalid native forwarding/);
  await assert.rejects(startNativeHost({ mode: "external", endpoint, executable: "/missing/wx", accountId: "a", token, consumerStateDir: dir }), /never executable/);
});
test("managed mode rejects an existing listener without inspecting or terminating its owner", async t => {
  const { dir, token } = await fixture(t);
  const server = createServer((_req, res) => res.end("existing")); server.listen(0, "127.0.0.1"); await once(server, "listening");
  t.after(() => new Promise<void>(done => { server.closeAllConnections(); server.close(() => done()); }));
  await assert.rejects(startNativeHost({ mode: "managed", listen: `127.0.0.1:${(server.address() as any).port}`, executable: "/missing/wx", dataDir: dir, accountId: "a", token, consumerStateDir: dir }), /native_owner_conflict/);
  assert.equal(server.listening, true);
});

test("managed serve stops with the Gateway only when wx reports it can", () => {
  const base = ["serve", "--data-dir", "/data", "--token-stdin", "--listen", "127.0.0.1:6174"];
  assert.deepEqual(managedServeArgs("/data", "127.0.0.1:6174", undefined, ["serve", "events"]), base);
  assert.deepEqual(managedServeArgs("/data", "127.0.0.1:6174", "/server.toml", ["serve", "serve-exit-with-parent"]),
    [...base, "--exit-with-parent", "--server-config", "/server.toml"]);
});

test("a managed daemon stops when its Gateway process is killed", { skip: !process.env.WX_BINARY && "requires WX_BINARY, a freshly built native wx" }, async t => {
  const executable = process.env.WX_BINARY!;
  assert.ok((await resolveNativeExecutable(executable)).capabilities.includes("serve-exit-with-parent"));
  const { dir, token } = await fixture(t);
  const port = await new Promise<number>(resolve => { const probe = createServer(); probe.listen(0, "127.0.0.1", () => {
    const { port } = probe.address() as any; probe.close(() => resolve(port)); }); });
  // A separate Node process stands in for the Gateway, so the test can kill it outright.
  const gateway = join(dir, "gateway.mjs");
  await writeFile(gateway, `import { startNativeHost } from ${JSON.stringify(fileURLToPath(new URL("./native-host.ts", import.meta.url)))};
const host = await startNativeHost(${JSON.stringify({ mode: "managed", executable, listen: `127.0.0.1:${port}`, dataDir: join(dir, "hub"), accountId: "a", token, consumerStateDir: dir })});
console.log((await host.client.status()).pid);
setInterval(() => {}, 1000);
`);
  const child = spawn(process.execPath, ["--experimental-strip-types", gateway], { stdio: ["ignore", "pipe", "inherit"] });
  t.after(() => { if (child.exitCode === null) child.kill("SIGKILL"); });
  const [line] = await once(createInterface({ input: child.stdout! }), "line");
  const daemon = Number(line);
  const running = () => { try { process.kill(daemon, 0); return true; } catch { return false; } };
  assert.ok(running());
  child.kill("SIGKILL");
  const deadline = Date.now() + 30_000;
  while (running() && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 100));
  if (running()) process.kill(daemon, "SIGTERM");
  assert.equal(running(), false, "the daemon kept running after its Gateway was killed");
});

test("managed executable selection binds the host architecture while external mode stays portable", () => {
  assert.equal(nativeHostTarget("linux", "arm64").rustTarget, "aarch64-unknown-linux-gnu");
  assert.equal(nativeHostTarget("linux", "x64").rustTarget, "x86_64-unknown-linux-gnu");
  assert.equal(nativeHostTarget("darwin", "arm64").rustTarget, "aarch64-apple-darwin");
  assert.throws(() => nativeHostTarget("darwin", "x64"), /unsupported host platform/);
  assert.equal(nativeHostTarget("win32", "arm64").rustTarget, "aarch64-pc-windows-msvc");
  assert.equal(nativeHostTarget("win32", "x64").executable, "wx.exe");
  assert.throws(() => nativeHostTarget("freebsd", "arm64"), /external mode/);
});

test("native executable validation recognizes ELF, Mach-O and bounded PE headers", () => {
  assert.ok(nativeExecutableFormat(Buffer.from([0x7f, 0x45, 0x4c, 0x46]), "linux"));
  assert.ok(nativeExecutableFormat(Buffer.from([0xcf, 0xfa, 0xed, 0xfe]), "darwin"));
  const pe = Buffer.alloc(128); pe.write("MZ"); pe.writeUInt32LE(64, 60); pe.write("PE\0\0", 64);
  assert.ok(nativeExecutableFormat(pe, "win32"));
  pe.writeUInt32LE(0xffffffff, 60); assert.equal(nativeExecutableFormat(pe, "win32"), false);
  assert.equal(nativeExecutableFormat(Buffer.from("#!/bin/sh"), "darwin"), false);
});
