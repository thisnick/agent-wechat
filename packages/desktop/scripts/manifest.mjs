import { createHash } from "node:crypto";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { resolve, basename, join } from "node:path";
import { execFileSync } from "node:child_process";
const directory = resolve(process.argv[2]);
const platform = process.argv[3];
if (
  !["macos-arm64", "windows-x64", "windows-arm64", "linux-x64"].includes(
    platform,
  )
)
  throw new Error("Unknown desktop platform");
const { version } = JSON.parse(await readFile("packages/cli/package.json"));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], {
  encoding: "utf8",
}).trim();
const assets = [];
for (const file of (await readdir(directory)).sort()) {
  if (file.endsWith(".json")) continue;
  if (!file.startsWith(`AgentWechat-v${version}-`))
    throw new Error("Unexpected desktop release asset");
  const bytes = await readFile(join(directory, file));
  assets.push({
    file: basename(file),
    size: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  });
}
if (!assets.length) throw new Error("No final desktop assets");
await writeFile(
  join(directory, `desktop-${platform}.json`),
  JSON.stringify(
    { schemaVersion: 1, version, sourceCommit, platform, assets },
    null,
    2,
  ) + "\n",
);
