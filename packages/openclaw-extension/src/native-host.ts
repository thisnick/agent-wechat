import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { access, lstat, readFile, realpath } from "node:fs/promises";
import { spawn, execFile, type ChildProcess } from "node:child_process";
import { promisify } from "node:util";
import { delimiter, dirname, isAbsolute, join } from "node:path";
import { createConnection } from "node:net";
import { NativeWeChatClient, validateNativeBaseUrl } from "@agent-wechat/shared";
import type { WeChatNativeConfig } from "./types.js";
const exec = promisify(execFile);
export type NativeHost = { client: NativeWeChatClient; stop: () => Promise<void> };
export function requireNativeIdentity(value: any): void {
  if (value?.product !== "agent-wechat" || value.backend !== "native" || value.apiVersion !== 1
    || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(value.version ?? "")
    || !Array.isArray(value.capabilities) || ["serve", "events", "profile-read", "text-send", "native-login"].some(capability => !value.capabilities.includes(capability))) {
    throw new Error("native_binary_incompatible: native v1 profile/events/login/text capabilities are required");
  }
}
export async function readNativeToken(path: string): Promise<string> {
  if (!isAbsolute(path)) throw new Error("native.tokenFile must be an absolute path");
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size > 65536 || (info.mode & 0o077)) throw new Error("Native token file must be a private regular file (0600)");
  const token = (await readFile(path, "utf8")).trim();
  if (token.length < 32 || /[\r\n]/.test(token)) throw new Error("Native API token must contain at least 32 bytes on one line");
  return token;
}
export async function resolveNativeExecutable(explicit?: string, path = process.env.PATH ?? ""): Promise<string> {
  let candidate = explicit;
  if (candidate && !isAbsolute(candidate)) throw new Error("native.executable must be absolute");
  if (!candidate) {
    for (const directory of path.split(delimiter).filter(isAbsolute)) {
      try { await access(join(directory, "wx"), constants.X_OK); candidate = join(directory, "wx"); break; } catch {}
    }
  }
  if (!candidate) throw new Error("native_binary_missing: install native wx separately or configure its absolute path");
  candidate = await realpath(candidate);
  // Tested adapter for our npm launcher: execute the installed ELF directly so
  // authenticated daemon pid can be bound to the child this service owns.
  if (candidate.endsWith("/scripts/native-launcher.mjs")) {
    const root = dirname(dirname(candidate));
    const pkg = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
    if (pkg.name !== "@agent-wechat/cli" || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(pkg.version)) throw new Error("Unsupported native CLI launcher");
    const manifest = JSON.parse(await readFile(join(root, "native-release.json"), "utf8"));
    const asset = manifest.assets?.find((value: any) => value.rustTarget === "x86_64-unknown-linux-gnu");
    if (manifest.version !== pkg.version || !asset || !/^[a-f0-9]{64}$/.test(asset.sha256)) throw new Error("Native CLI manifest is incompatible");
    candidate = await realpath(join(root, "native", pkg.version, "linux-x64-gnu", "wx"));
    const bytes = await readFile(candidate);
    if (bytes.length !== asset.bytes || createHash("sha256").update(bytes).digest("hex") !== asset.sha256) throw new Error("Installed native executable checksum mismatch");
  }
  const stat = await lstat(candidate);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("Native executable must be a regular file");
  await access(candidate, constants.X_OK);
  const magic = (await readFile(candidate)).subarray(0, 4);
  if (!magic.equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))) throw new Error("native_binary_required: configure the actual native ELF executable, not an unverified process wrapper");
  const { stdout } = await exec(candidate, ["--json", "version"], { timeout: 5000, maxBuffer: 65536 });
  const version = JSON.parse(stdout);
  requireNativeIdentity(version);
  if (version.target !== "x86_64-unknown-linux-gnu") throw new Error("native_binary_incompatible: only the validated Linux x64 GNU target is supported");
  return candidate;
}
async function portOccupied(url: URL): Promise<boolean> {
  return new Promise((resolve, reject) => {
    const socket = createConnection({ host: url.hostname.replace(/^\[|\]$/g, ""), port: Number(url.port || (url.protocol === "https:" ? 443 : 80)) });
    socket.setTimeout(1000);
    socket.once("connect", () => { socket.destroy(); resolve(true); });
    socket.once("error", error => { socket.destroy(); if ((error as NodeJS.ErrnoException).code === "ECONNREFUSED") resolve(false); else reject(new Error("Cannot establish managed listener ownership")); });
    socket.once("timeout", () => { socket.destroy(); reject(new Error("Managed listener ownership check timed out")); });
  });
}
async function stopOwnedChild(child: ChildProcess): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise<void>(resolve => {
    const kill = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL"); }, 5000);
    child.once("exit", () => { clearTimeout(kill); resolve(); });
    child.kill("SIGTERM");
  });
}
export async function startNativeHost(config: WeChatNativeConfig): Promise<NativeHost> {
  if (!config.accountId || !isAbsolute(config.consumerStateDir)) throw new Error("Native accountId and absolute consumerStateDir are required");
  const token = await readNativeToken(config.tokenFile);
  if (config.mode === "external") {
    if (!config.endpoint || config.executable || config.serverConfig || config.dataDir || config.listen) throw new Error("External native mode accepts endpoint/token references and never executable/process options");
    const client = new NativeWeChatClient({ baseUrl: config.endpoint, token });
    const status = await client.status();
    requireNativeIdentity(status.version);
    return { client, stop: async () => {} };
  }
  if (config.mode !== "managed" || config.endpoint || !config.dataDir || !isAbsolute(config.dataDir)
    || (config.serverConfig && !isAbsolute(config.serverConfig))) throw new Error("Managed native mode requires absolute dataDir/config paths and no remote endpoint");
  const url = validateNativeBaseUrl(`http://${config.listen ?? "127.0.0.1:6174"}`);
  if (await portOccupied(url)) throw new Error("native_owner_conflict: configured port is already owned; use external mode for an independently operated daemon");
  const executable = await resolveNativeExecutable(config.executable);
  const client = new NativeWeChatClient({ baseUrl: url.href, token, timeoutMs: 1000 });
  const args = ["serve", "--data-dir", config.dataDir, "--token-file", config.tokenFile, "--listen", url.host];
  if (config.serverConfig) args.push("--server-config", config.serverConfig);
  const child = spawn(executable, args, { stdio: ["ignore", "ignore", "pipe"], shell: false });
  // Drain stderr without copying account/device/transport details into Gateway logs.
  child.stderr?.on("data", () => {});
  let exited = false;
  child.once("exit", () => { exited = true; });
  child.once("error", () => { exited = true; });
  try {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline) {
      if (exited) throw new Error("Native owned child exited before readiness");
      try {
        const status = await client.status();
        if (exited || status.version.backend !== "native" || status.version.apiVersion !== 1 || status.pid !== child.pid
          || status.dataDir !== config.dataDir || typeof status.serverId !== "string" || typeof status.ownershipGeneration !== "string") throw new Error("native_owner_conflict: readiness identity does not match the owned process/store");
        requireNativeIdentity(status.version);
        return { client, stop: () => stopOwnedChild(child) };
      } catch (error) {
        if (String(error).includes("native_owner_conflict")) throw error;
        await new Promise(resolve => setTimeout(resolve, 100));
      }
    }
    throw new Error("Native owned child readiness timed out");
  } catch (error) { await stopOwnedChild(child); throw error; }
}
let active: { key: string; value: Promise<NativeHost> } | undefined;
export function ensureNativeHost(config: WeChatNativeConfig): Promise<NativeHost> {
  const key = JSON.stringify(config);
  if (active && active.key !== key) throw new Error("Native service configuration changed; stop the owned generation before restart");
  if (!active) {
    const value = startNativeHost(config).catch(error => { active = undefined; throw error; });
    active = { key, value };
  }
  return active.value;
}
export async function stopNativeHost(): Promise<void> {
  const prior = active; active = undefined;
  if (prior) await (await prior.value).stop();
}
