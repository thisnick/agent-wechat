import { createHash } from "node:crypto";
import {
  readFile,
  readdir,
  mkdir,
  copyFile,
  writeFile,
} from "node:fs/promises";
import { resolve, join } from "node:path";
import { execFileSync } from "node:child_process";
const output = resolve(process.argv[2]);
const roots = process.argv.slice(3);
const { version } = JSON.parse(await readFile("packages/cli/package.json"));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], {
  encoding: "utf8",
}).trim();
const platforms = new Set([
  "macos-arm64",
  "windows-x64",
  "windows-arm64",
  "linux-x64",
  "linux-arm64",
]);
const assets = [],
  names = new Set();
await mkdir(output, { recursive: true });
for (const directory of roots) {
  const files = await readdir(directory);
  const metadata = files.filter((name) => /^desktop-.*\.json$/.test(name));
  if (metadata.length !== 1)
    throw new Error("Expected one desktop manifest per artifact");
  const fragment = JSON.parse(await readFile(join(directory, metadata[0])));
  if (
    fragment.schemaVersion !== 1 ||
    fragment.version !== version ||
    fragment.sourceCommit !== sourceCommit ||
    !platforms.delete(fragment.platform)
  )
    throw new Error("Desktop build identity mismatch");
  if (files.length !== fragment.assets.length + 1 || !fragment.assets.length)
    throw new Error("Unexpected desktop artifact contents");
  for (const asset of fragment.assets) {
    if (
      asset.file !== asset.file.split(/[\\/]/).pop() ||
      names.has(asset.file) ||
      !asset.file.startsWith(`AgentWechat-v${version}-`)
    )
      throw new Error("Invalid desktop asset");
    const bytes = await readFile(join(directory, asset.file));
    if (
      bytes.length !== asset.size ||
      createHash("sha256").update(bytes).digest("hex") !== asset.sha256
    )
      throw new Error("Desktop asset integrity mismatch");
    names.add(asset.file);
    assets.push({ ...asset, platform: fragment.platform });
    await copyFile(join(directory, asset.file), join(output, asset.file));
  }
}
if (platforms.size) throw new Error("Missing desktop platforms");
await writeFile(
  join(output, `AgentWechat-v${version}-checksums.json`),
  JSON.stringify({ schemaVersion: 1, version, sourceCommit, assets }, null, 2) +
    "\n",
);
