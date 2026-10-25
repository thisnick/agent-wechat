import {
  copyFile,
  mkdir,
  readFile,
  writeFile,
  chmod,
  stat,
} from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const args = process.argv.slice(2);
const argument = (name, fallback) =>
  args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
const target = argument(
  "--target",
  execFileSync("rustc", ["--print", "host-tuple"], { encoding: "utf8" }).trim(),
);
const platforms = {
  "aarch64-apple-darwin": "darwin-arm64",
  "x86_64-pc-windows-msvc": "win32-x64",
  "aarch64-pc-windows-msvc": "win32-x64",
  "x86_64-unknown-linux-gnu": "linux-x64",
};
const platform = platforms[target];
if (!platform) throw new Error(`Unsupported desktop target: ${target}`);
const extension = target.includes("windows") ? ".exe" : "";
const output = join(root, "packages/desktop/src-tauri/binaries");
const resources = join(root, "packages/desktop/src-tauri/resources");
await mkdir(output, { recursive: true });
await mkdir(resources, { recursive: true });
const binary = resolve(
  argument(
    "--wx",
    join(
      root,
      `packages/native-client-rust/target/${target}/release/wx${extension}`,
    ),
  ),
);
if (!(await stat(binary)).isFile())
  throw new Error(
    "Build the native wx executable before preparing desktop sidecars",
  );
await copyFile(binary, join(output, `wx-${target}${extension}`));
await chmod(join(output, `wx-${target}${extension}`), 0o755);
const assets = JSON.parse(
  await readFile(join(root, "packages/desktop/media-assets.json")),
);
for (const asset of assets.filter(
  (asset) =>
    asset.name.endsWith(platform) || asset.name.startsWith(`${platform}.`),
)) {
  const binaryName = asset.name.startsWith("ffmpeg-")
    ? "ffmpeg"
    : asset.name.startsWith("ffprobe-")
      ? "ffprobe"
      : null;
  const destination = binaryName
    ? join(output, `${binaryName}-${target}${extension}`)
    : join(resources, `ffmpeg-${asset.name}`);
  let bytes;
  try {
    const existing = await readFile(destination);
    if (createHash("sha256").update(existing).digest("hex") === asset.sha256)
      bytes = existing;
  } catch {}
  if (!bytes) {
    const response = await fetch(asset.url, {
      signal: AbortSignal.timeout(300000),
    });
    if (!response.ok)
      throw new Error(`Cannot download ${asset.name}: HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    if (createHash("sha256").update(bytes).digest("hex") !== asset.sha256)
      throw new Error(`Integrity check failed for ${asset.name}`);
    await writeFile(destination, bytes);
  }
  if (binaryName) await chmod(destination, 0o755);
}
await copyFile(
  join(root, "packages/cli/THIRD_PARTY_NOTICES.txt"),
  join(resources, "THIRD_PARTY_NOTICES.txt"),
);
await writeFile(
  join(resources, "media-sources.json"),
  JSON.stringify(
    {
      release:
        "https://github.com/eugeneware/ffmpeg-static/releases/tag/b6.1.1",
      assets: assets.filter((asset) => asset.name.includes(platform)),
      windowsArm64Helpers:
        target === "aarch64-pc-windows-msvc"
          ? "x64 helpers use Windows emulation"
          : undefined,
    },
    null,
    2,
  ),
);
console.log(`Prepared wx and verified media helpers for ${target}`);
