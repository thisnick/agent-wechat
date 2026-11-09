// A disposable synthetic hub for one scene: the wx-dev simulator upstream plus a
// real hub on a fresh data directory, as scripts/dev.mjs runs them, but without
// the clock ticking. The simulator clock only moves when a scene advances it.
//
// Owns exactly the two child processes it starts and stops only those.
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const repositoryDir = resolve(packageDir, "../..");

/** The wx-dev binary: WX_DEV_BINARY, else the `--profile test` build of the simulator. */
export function simulatorBinary() {
  if (process.env.WX_DEV_BINARY) return resolve(process.env.WX_DEV_BINARY);
  const target = process.env.CARGO_TARGET_DIR
    ? resolve(process.env.CARGO_TARGET_DIR)
    : join(repositoryDir, "packages/native-client-rust/target");
  return join(target, "debug", process.platform === "win32" ? "wx-dev.exe" : "wx-dev");
}

function launch(binary, command, dataDir, args = []) {
  const child = spawn(binary, [command, "--data-dir", dataDir, ...args], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stderr.resume();
  const ready = new Promise((done, reject) => {
    const lines = createInterface({ input: child.stdout });
    const timer = setTimeout(() => reject(new Error(`wx-dev ${command} did not start in 30 s`)), 30_000);
    child.once("error", reject);
    child.once("exit", () => {
      clearTimeout(timer);
      reject(new Error(`wx-dev ${command} stopped before it was ready`));
    });
    lines.once("line", (line) => {
      clearTimeout(timer);
      try {
        const info = JSON.parse(line);
        if (info.state !== "ready") throw new Error("not ready");
        done(info);
      } catch {
        reject(new Error(`wx-dev ${command} printed an unexpected startup line`));
      }
    });
  });
  return { child, ready };
}

async function post(url, token, body, timeout = 5000) {
  return fetch(url, {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(timeout),
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function exited(child, timeout) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((done) => {
    const timer = setTimeout(() => {
      child.kill();
      done();
    }, timeout);
    child.once("exit", () => {
      clearTimeout(timer);
      done();
    });
  });
}

/**
 * Starts a fresh simulator and hub. `control(op)` drives the simulator (ops in
 * crates/wechat-simulator/src/contract.rs `Control`); `stop()` shuts both down and
 * removes the data directory.
 */
export async function startStack(binary = simulatorBinary()) {
  const dataDir = await mkdtemp(join(tmpdir(), "agentwechat-scenes-"));
  const children = [];
  let upstream, hub, hubToken, controlToken;
  const stop = async () => {
    try {
      if (hub && hubToken) {
        try {
          const status = await (
            await fetch(`${hub.endpoint}/v1/status`, {
              headers: { Authorization: `Bearer ${hubToken}` },
              signal: AbortSignal.timeout(3000),
            })
          ).json();
          await post(`${hub.endpoint}/v1/daemon/shutdown`, hubToken, status, 3000);
        } catch {
          /* The owned child is stopped below. */
        }
      }
      if (upstream && controlToken)
        await post(`http://${upstream.upstream}/shutdown`, controlToken, {}, 3000).catch(() => {});
      for (const child of children) await exited(child, 5000);
    } finally {
      await rm(dataDir, { recursive: true, force: true });
    }
  };
  try {
    const first = launch(binary, "upstream", dataDir);
    children.push(first.child);
    upstream = await first.ready;
    controlToken = (await readFile(upstream.controlTokenFile, "utf8")).trim();
    const second = launch(binary, "hub", dataDir, ["--upstream", upstream.upstream]);
    children.push(second.child);
    hub = await second.ready;
    const upstreamUrl = `http://${upstream.upstream}`;
    hubToken = (await readFile(hub.tokenFile, "utf8")).trim();
    const status = await (
      await fetch(`${hub.endpoint}/v1/status`, { headers: { Authorization: `Bearer ${hubToken}` } })
    ).json();
    if (status.version?.backend !== "simulator") throw new Error("Scenes need a simulator hub");
    return {
      dataDir,
      hub: { endpoint: hub.endpoint, token: hubToken, serverId: status.serverId },
      upstream: upstreamUrl,
      async control(body) {
        const response = await post(`${upstreamUrl}/control`, controlToken, body);
        if (!response.ok) throw new Error(`Simulator control ${JSON.stringify(body.op)} failed (${response.status})`);
      },
      async clock() {
        const response = await fetch(`${upstreamUrl}/state`, {
          headers: { Authorization: `Bearer ${controlToken}` },
          signal: AbortSignal.timeout(5000),
        });
        if (!response.ok) throw new Error("Simulator state unavailable");
        return (await response.json()).clock;
      },
      stop,
    };
  } catch (error) {
    await stop();
    throw error;
  }
}
