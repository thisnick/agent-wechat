import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { updaterFiles, verifyUpdate } from "./updater.mjs";
const [directory, platform] = process.argv.slice(2);
if (!updaterFiles[platform]) throw new Error("Unknown updater platform");
if (
  !process.env.TAURI_SIGNING_PRIVATE_KEY ||
  !process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD
)
  throw new Error("Updater release signing credentials are required");
const { version, plugins } = JSON.parse(
  await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url))
);
const file = join(
  resolve(directory),
  `AgentWechat-v${version}-${updaterFiles[platform][1]}`
);
const cli = createRequire(new URL("../package.json", import.meta.url)).resolve(
  "@tauri-apps/cli/tauri.js"
);
try {
  execFileSync(
    process.execPath,
    [cli, "signer", "sign", "--app-version", version, file],
    { stdio: "pipe" }
  );
} catch {
  throw new Error("Updater signing failed");
}
verifyUpdate(
  await readFile(file),
  (await readFile(file + ".sig", "utf8")).trim(),
  plugins.updater.pubkey,
  version
);
console.log(`Verified signed updater artifact: ${platform}`);
