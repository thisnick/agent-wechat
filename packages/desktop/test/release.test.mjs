import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
const root = resolve(import.meta.dirname, "../../..");
const { version } = JSON.parse(
  await readFile(join(root, "packages/cli/package.json")),
);
test("final desktop manifests reject bytes changed after signing or packaging", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "desktop-manifests-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const platforms = [
    "macos-arm64",
    "windows-x64",
    "windows-arm64",
    "linux-x64",
  ];
  const fragments = [];
  for (const platform of platforms) {
    const fragment = join(directory, platform);
    await mkdir(fragment);
    fragments.push(fragment);
    await writeFile(
      join(fragment, `AgentWechat-v${version}-${platform}.zip`),
      `signed fixture ${platform}`,
    );
    execFileSync(
      process.execPath,
      ["packages/desktop/scripts/manifest.mjs", fragment, platform],
      { cwd: root },
    );
  }
  const output = join(directory, "output");
  execFileSync(
    process.execPath,
    ["packages/desktop/scripts/merge-manifests.mjs", output, ...fragments],
    { cwd: root },
  );
  const manifest = JSON.parse(
    await readFile(join(output, `AgentWechat-v${version}-checksums.json`)),
  );
  assert.equal(manifest.assets.length, 4);
  await writeFile(
    join(fragments[0], `AgentWechat-v${version}-macos-arm64.zip`),
    "changed bytes",
  );
  const changed = spawnSync(
    process.execPath,
    [
      "packages/desktop/scripts/merge-manifests.mjs",
      join(directory, "other"),
      ...fragments,
    ],
    { cwd: root, encoding: "utf8" },
  );
  assert.notEqual(changed.status, 0);
  assert.match(changed.stderr, /integrity mismatch/);
});
test("missing platforms prevent publishing a partial desktop release", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "desktop-partial-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const result = spawnSync(
    process.execPath,
    ["packages/desktop/scripts/merge-manifests.mjs", directory],
    { cwd: root, encoding: "utf8" },
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Missing desktop platforms/);
});
