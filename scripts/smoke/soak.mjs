// Repeats ordinary activity against the local upstream simulator and checks that
// the hub's own storage stops growing. All accounts, data and the upstream are
// synthetic; nothing reaches WeChat.
//
// At each checkpoint the hub is stopped and `wx-dev audit` reads its data folder:
// - every runtime note has a registered kind and a bounded key;
// - work queues are empty once activity stops;
// - notes kept forever stop growing once the contacts and groups are fixed;
// - notes that grow with traffic grow at a steady rate per operation;
// - no `.sqlite-admission-*` copy is left after a start.
// Before each checkpoint the hub must also stay quiet: while idle it sends the
// simulator nothing but its notification polls and heartbeats.
//
// SOAK_ROUNDS (default 6), SOAK_CHECKPOINT_EVERY (2), SOAK_IDLE_SECONDS (12) and
// SOAK_SEED (1) size the run. SOAK_DATA_DIR keeps the data folder, and
// SOAK_REPORT writes the checkpoint measurements as JSON.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { NativeWeChatClient } from "../../packages/shared/src/native-client.ts";

const repository = resolve(import.meta.dirname, "../..");
const binary = process.env.WX_DEV_BINARY ?? join(repository, "packages/native-client-rust/target/debug", process.platform === "win32" ? "wx-dev.exe" : "wx-dev");
const rounds = setting("SOAK_ROUNDS", 6);
const checkpointEvery = setting("SOAK_CHECKPOINT_EVERY", 2);
const idleSeconds = setting("SOAK_IDLE_SECONDS", 12);
const seed = setting("SOAK_SEED", 1);
const execute = promisify(execFile);
const temporary = process.env.SOAK_DATA_DIR ? null : await mkdtemp(join(tmpdir(), "agent-wechat-soak-"));
const data = process.env.SOAK_DATA_DIR ? resolve(process.env.SOAK_DATA_DIR) : join(temporary, "soak");
const account = "personal";
const peers = ["sim_mei", "sim_daniel"];
// Requests a hub makes while idle: notification polls and heartbeats.
const IDLE_REQUESTS = new Set(["poll", "heartbeat"]);
const processes = new Set();
const checkpoints = [];
const totals = { operations: 0, groupChanges: 0, races: 0, crashes: 0, restarts: 0 };
let remote;
let hub;
let a;
let controlToken;
let room;
let random = mulberry32(seed);

function setting(name, fallback) {
  const value = Number(process.env[name] ?? fallback);
  assert.ok(Number.isInteger(value) && value > 0, `${name} must be a positive integer`);
  return value;
}
function mulberry32(state) {
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function between(low, high) {
  return low + Math.floor(random() * (high - low + 1));
}
async function waitFor(label, condition, timeout = 30000) {
  const deadline = Date.now() + timeout;
  let lastError;
  while (Date.now() < deadline) {
    try { const value = await condition(); if (value) return value; }
    catch (error) { lastError = error; }
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out: ${label}`, { cause: lastError });
}
async function start(command, args = []) {
  const child = spawn(binary, [command, "--data-dir", data, ...args], { stdio: ["ignore", "pipe", "pipe"] });
  processes.add(child);
  child.once("exit", () => processes.delete(child));
  let diagnostics = "";
  child.stderr.on("data", chunk => { diagnostics = (diagnostics + chunk).slice(-4000); });
  const ready = await new Promise((resolve, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error(`Startup timeout: ${command}`)), 60000);
    child.once("error", error => { clearTimeout(timer); reject(error); });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`${command} exited ${code}: ${diagnostics}`)); });
    child.stdout.on("data", chunk => {
      output += chunk;
      if (output.includes("\n")) {
        clearTimeout(timer);
        try { resolve(JSON.parse(output.slice(0, output.indexOf("\n")))); } catch (error) { reject(error); }
      }
    });
  });
  assert.equal(ready.state, "ready");
  return { child, ready };
}
async function control(input) {
  const response = await fetch(`http://${remote.ready.upstream}/control`, {
    method: "POST", headers: { Authorization: `Bearer ${controlToken}`, "Content-Type": "application/json" },
    body: JSON.stringify(input), signal: AbortSignal.timeout(5000),
  });
  assert.equal(response.status, 200, `Control ${input.op}: ${await response.clone().text()}`);
  return response.json();
}
async function state() {
  const response = await fetch(`http://${remote.ready.upstream}/state`, { headers: { Authorization: `Bearer ${controlToken}` } });
  assert.equal(response.status, 200);
  return response.json();
}
async function requests() {
  return (await state()).requests ?? {};
}
function active(counts) {
  return Object.entries(counts).filter(([op]) => !IDLE_REQUESTS.has(op)).reduce((sum, [, n]) => sum + n, 0);
}
async function startHub() {
  hub = await start("hub", ["--upstream", remote.ready.upstream]);
  a = new NativeWeChatClient({ baseUrl: hub.ready.endpoint, token: (await readFile(hub.ready.tokenFile, "utf8")).trim() });
}
async function waitConnected() {
  await waitFor(`${account} push connected`, async () => (await a.account(account)).incomingTransport === "simulator_push", 60000);
}
async function stopHub() {
  const status = await a.status();
  await a.request("v1/daemon/shutdown", { method: "POST", body: JSON.stringify({ pid: status.pid, serverId: status.serverId, ownershipGeneration: status.ownershipGeneration }) });
  await waitFor("hub shutdown", () => hub.child.exitCode !== null || hub.child.signalCode !== null);
}
async function crashHub() {
  hub.child.kill("SIGKILL");
  await waitFor("hub killed", () => hub.child.exitCode !== null || hub.child.signalCode !== null);
  totals.crashes += 1;
}
async function audit() {
  const { stdout } = await execute(binary, ["audit", "--data-dir", data], { timeout: 60000, maxBuffer: 16 << 20 });
  return JSON.parse(stdout);
}
async function finishOperation(label, operation) {
  return waitFor(label, async () => {
    const current = await a.operation(account, operation.id);
    if (["failed", "needs_review", "cancelled"].includes(current.state)) throw new Error(`${label}: ${JSON.stringify(current)}`);
    return current.state === "succeeded" && current;
  });
}
async function settleSend(intent) {
  return waitFor("send accepted", async () => {
    const current = await a.send(account, intent.id);
    if (["failed", "cancelled"].includes(current.state)) throw new Error(JSON.stringify(current));
    return current.state === "accepted" && current.localMessageId && current;
  });
}
async function login() {
  await a.createAccount(account);
  const operation = await a.login(account, { strategy: "qr", qrRefresh: { mode: "manual", maxRefreshes: 0 } }, `login-${account}`);
  await waitFor("QR ready", async () => (await a.operation(account, operation.id)).state === "awaiting_scan");
  await control({ op: "login", account, status: "approved" });
  await waitFor("signed in", async () => {
    const current = await a.operation(account, operation.id);
    if (["failed", "expired", "rejected"].includes(current.state)) throw new Error(JSON.stringify(current));
    return current.state === "succeeded";
  }, 120000);
  await waitConnected();
}
async function announcement(text) {
  await waitFor(`group announcement "${text}"`, async () => (await a.group(account, room)).announcement === text, 60000);
}
// Fails the refresh's roster lookup after it has fetched the group, then changes
// the group again before the retry: the #722 sequence that used to loop.
async function racedGroupChange(index) {
  await control({ op: "metadata_fault", account, kind: "room", count: 1 });
  await control({ op: "group_change", account, room, announcement: `Soak race ${index} first` });
  await waitFor("failed roster lookup", async () => (await state()).accounts.find(item => item.id === account)?.metadataFaults?.room === 0, 60000);
  const last = `Soak race ${index} second`;
  await control({ op: "group_change", account, room, announcement: last });
  totals.groupChanges += 2;
  totals.races += 1;
  return last;
}
async function round(index) {
  // The first two rounds run every kind of operation in a fixed order, so the
  // first checkpoint has seen them all. Later rounds vary counts and order.
  const fixed = index < 2;
  const steps = [];
  const direct = fixed ? 4 : between(1, 6);
  for (let i = 0; i < direct; i++) steps.push(() => control({ op: "inbound", account, peer: peers[i % 2], body: `Soak ${seed}.${index}.${i}` }));
  const group = fixed ? 2 : between(0, 4);
  for (let i = 0; i < group; i++) steps.push(() => control({ op: "inbound", account, peer: room, sender: peers[i % 2], body: `Soak group ${seed}.${index}.${i}` }));
  const sends = fixed ? 2 : between(0, 3);
  for (let i = 0; i < sends; i++) steps.push(async () => settleSend(await a.sendText(account, peers[(index + i) % 2], `Soak reply ${seed}.${index}.${i}`, `soak-send-${seed}-${index}-${i}`)));
  if (fixed || random() < 0.5) steps.push(() => control({ op: "receive_media", account, peer: peers[index % 2], kind: "file" }));
  if (fixed || random() < 0.5) steps.push(async () => finishOperation("pin", await a.setThreadPinned(account, peers[0], index % 2 === 0, `soak-pin-${seed}-${index}`)));
  if (!fixed) for (let i = steps.length - 1; i > 0; i--) { const j = Math.floor(random() * (i + 1)); [steps[i], steps[j]] = [steps[j], steps[i]]; }
  for (const step of steps) await step();
  totals.operations += steps.length;
  let last;
  if (index % 2 === 1) last = await racedGroupChange(index);
  else {
    last = `Soak round ${index}`;
    await control({ op: "group_change", account, room, announcement: last });
    totals.groupChanges += 1;
  }
  totals.operations += 1;
  await announcement(last);
  // A message arriving in between changes the read version; read it again.
  for (let attempt = 0; attempt < 5; attempt++) {
    const thread = await a.thread(account, peers[1]);
    if (!((thread.unreadCount ?? 0) > 0 && thread.readStateVersion)) break;
    try {
      await a.markRead(account, peers[1], thread.readStateVersion);
      totals.operations += 1;
      break;
    } catch (error) {
      if (error.code !== "read_state_conflict" || attempt === 4) throw error;
    }
  }
  // A crash every third round restarts through the copying admission path.
  if (index % 3 === 2) {
    await crashHub();
    await startHub();
    await waitConnected();
  }
}
// Waits for the hub to stop working, then requires it to stay quiet.
async function idle() {
  const busy = (before, after) => Object.keys(after).filter(op => !IDLE_REQUESTS.has(op) && after[op] !== (before[op] ?? 0)).map(op => `${op} +${after[op] - (before[op] ?? 0)}`);
  const start = await requests();
  let previous = active(start);
  let stableSince = Date.now();
  try {
    await waitFor("hub activity to settle", async () => {
      const current = active(await requests());
      if (current !== previous) { previous = current; stableSince = Date.now(); }
      return Date.now() - stableSince >= 6000;
    }, 120000);
  } catch (error) {
    throw new Error(`hub kept sending requests for 120s after activity stopped: ${busy(start, await requests()).join(", ")}`, { cause: error });
  }
  const before = await requests();
  await new Promise(resolve => setTimeout(resolve, idleSeconds * 1000));
  assert.deepEqual(busy(before, await requests()), [], `hub sent requests while idle for ${idleSeconds}s`);
}
function traffic(families) {
  return Object.values(families).filter(f => f.growth === "perTraffic" || f.growth === "archive").reduce((sum, f) => ({ rows: sum.rows + f.rows, bytes: sum.bytes + f.bytes }), { rows: 0, bytes: 0 });
}
async function checkpoint(afterRound) {
  await idle();
  await stopHub();
  totals.restarts += 1;
  const report = await audit();
  const store = report.accounts[account];
  assert.ok(store, "audit found the account");
  const point = { afterRound, operations: totals.operations, requests: await requests(), store, leftovers: report.leftovers };
  checkpoints.push(point);
  const index = checkpoints.length - 1;
  console.error(`checkpoint ${index} after round ${afterRound}: ${store.rows} notes, ${store.bytes} bytes, longest key ${store.maxKey}, file ${store.fileBytes} bytes (WAL ${store.walBytes}, journal ${store.journalBytes}), ${totals.operations} operations`);
  assert.equal(store.unknownRows, 0, `notes of no registered kind: ${JSON.stringify(store.unknownSamples)}`);
  assert.equal(store.oversizedRows, 0, "notes with oversized keys");
  assert.equal(store.malformedRows, 0, `notes whose key doesn't match its kind: ${JSON.stringify(store.malformedSamples)}`);
  const pending = Object.entries(store.families).filter(([, f]) => f.growth === "perJob" && f.rows > 0).map(([kind, f]) => `${kind}: ${f.rows}`);
  assert.deepEqual(pending, [], "work left queued after activity stopped");
  assert.deepEqual(report.leftovers.admission, [], "admission copies left behind");
  assert.ok(report.leftovers.recovery.length <= totals.crashes, `recovery copies ${report.leftovers.recovery.length} exceed crashes ${totals.crashes}`);
  // A singleton is one note, or a few for kinds with a variable tail; flags
  // come and go.
  const crowded = Object.entries(store.families).filter(([, f]) => f.growth === "singleton" && f.rows > 4).map(([kind, f]) => `${kind}: ${f.rows}`);
  assert.deepEqual(crowded, [], "singleton notes multiplied");
  if (index >= 1) {
    // Every kind of operation ran before the first checkpoint, so notes kept per
    // contact, group or thread have their final count from then on.
    const baseline = checkpoints[0].store.families;
    const grew = Object.entries(store.families)
      .filter(([, f]) => f.growth === "perEntity")
      .filter(([kind, f]) => f.rows > (baseline[kind]?.rows ?? 0))
      .map(([kind, f]) => `${kind}: ${baseline[kind]?.rows ?? 0} -> ${f.rows}`);
    assert.deepEqual(grew, [], "notes kept per entity grew after the first checkpoint");
  }
  if (index >= 2) {
    // Traffic notes grow at a steady rate per operation, not faster over time.
    const rate = (from, to) => {
      const ops = Math.max(1, checkpoints[to].operations - checkpoints[from].operations);
      const t0 = traffic(checkpoints[from].store.families);
      const t1 = traffic(checkpoints[to].store.families);
      return { rows: (t1.rows - t0.rows) / ops, bytes: (t1.bytes - t0.bytes) / ops };
    };
    const first = rate(0, 1);
    const latest = rate(index - 1, index);
    assert.ok(latest.rows <= first.rows * 2 + 2, `traffic notes per operation rose from ${first.rows.toFixed(2)} to ${latest.rows.toFixed(2)}`);
    assert.ok(latest.bytes <= first.bytes * 2 + 4096, `traffic bytes per operation rose from ${first.bytes.toFixed(0)} to ${latest.bytes.toFixed(0)}`);
  }
  await startHub();
  await waitConnected();
}

try {
  remote = await start("upstream");
  controlToken = (await readFile(remote.ready.controlTokenFile, "utf8")).trim();
  await startHub();
  await login();
  const created = await finishOperation("create soak group", await a.createGroup(account, { members: peers, name: "Soak group" }, `soak-group-${seed}`));
  room = created.result.groupId;
  console.error(`soak: ${rounds} rounds, checkpoint every ${checkpointEvery}, idle ${idleSeconds}s, seed ${seed}`);
  for (let index = 0; index < rounds; index++) {
    await round(index);
    if ((index + 1) % checkpointEvery === 0 || index === rounds - 1) await checkpoint(index);
  }
  const counts = await requests();
  // Each change fetches the roster once, plus once per injected failure, and a
  // restart can interrupt a refresh after its roster lookup.
  assert.ok((counts.room ?? 0) <= totals.groupChanges + totals.races + totals.restarts + totals.crashes + 2, `roster lookups ${counts.room} for ${totals.groupChanges} group changes`);
  console.log(JSON.stringify({ ok: true, rounds, seed, totals, checkpoints: checkpoints.length }));
} finally {
  if (process.env.SOAK_REPORT) await writeFile(process.env.SOAK_REPORT, JSON.stringify({ rounds, seed, totals, checkpoints }, null, 2));
  if (remote && controlToken) {
    await fetch(`http://${remote.ready.upstream}/shutdown`, { method: "POST", headers: { Authorization: `Bearer ${controlToken}` }, signal: AbortSignal.timeout(1000) }).catch(() => {});
  }
  for (const child of processes) child.kill();
  await Promise.all([...processes].map(child => new Promise(resolve => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve();
    child.once("exit", resolve);
    setTimeout(() => { child.kill("SIGKILL"); resolve(); }, 3000).unref();
  })));
  if (temporary) await rm(temporary, { recursive: true, force: true });
}
