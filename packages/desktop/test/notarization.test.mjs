import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  access,
  rm,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(import.meta.dirname, "../../..");
const mock = `#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
folder=pathlib.Path(os.environ['RUNNER_TEMP'])
if name=='xcrun':
    if args[:2]==['notarytool','submit']:
        with open(folder/'submissions','a') as out: out.write('submit\\n')
        print(json.dumps({'id':'fixture','status':os.environ.get('NOTARY_STATUS','Accepted')}))
        sys.exit(int(os.environ.get('NOTARY_EXIT','0')))
    if args[:2]==['notarytool','log']: (folder/'rejection-log').write_text('fixture log')
    sys.exit(0)
if name=='codesign': sys.exit(int(os.environ.get('SIGNATURE_EXIT','0')))
if name=='spctl': sys.exit(0)
if name=='ditto':
    if '-k' in args: pathlib.Path(args[-1]).write_bytes(b'fixture zip')
    else: shutil.copytree(args[-2],args[-1],dirs_exist_ok=True)
if name=='hdiutil': pathlib.Path(args[-1]).write_bytes(b'fixture dmg')
if name=='security':
    if args[0]=='list-keychains':
        (folder/'restored-keychains').write_text(json.dumps(args))
        sys.exit(1)
    if args[0]=='delete-keychain':
        pathlib.Path(args[-1]).unlink(missing_ok=True)
        (folder/'deleted-keychain').write_text('yes')
`;
async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), "desktop signing "));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = join(directory, "bin");
  await mkdir(bin);
  for (const tool of [
    "xcrun",
    "codesign",
    "spctl",
    "ditto",
    "hdiutil",
    "security",
  ])
    await writeFile(join(bin, tool), mock, { mode: 0o755 });
  const app = join(directory, "AgentWechat.app");
  await mkdir(app);
  const env = {
    ...process.env,
    PATH: bin + ":" + process.env.PATH,
    RUNNER_TEMP: directory,
    GITHUB_STEP_SUMMARY: join(directory, "summary"),
    APPLE_ID: "fixture",
    APPLE_PASSWORD: "fixture",
    APPLE_TEAM_ID: "fixture",
    APPLE_SIGNING_IDENTITY: "fixture",
  };
  return { directory, app, env };
}
const options = { skip: process.platform === "win32" };
test(
  "notarization orchestration requires acceptance of both app and final DMG",
  options,
  async (t) => {
    const value = await fixture(t),
      output = join(value.directory, "output");
    const result = spawnSync(
      "bash",
      [
        "packages/desktop/scripts/macos-notarize.sh",
        value.app,
        output,
        "1.0.0-beta.14",
      ],
      { cwd: root, env: value.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      (await readFile(join(value.directory, "submissions"), "utf8"))
        .trim()
        .split("\n").length,
      2,
    );
    await access(join(output, "AgentWechat-v1.0.0-beta.14-macos-arm64.dmg"));
    await access(join(output, "AgentWechat-v1.0.0-beta.14-macos-arm64.zip"));
  },
);
for (const exit of ["0", "1"])
  test(
    `rejected notarization retrieves its log and stops packaging (exit ${exit})`,
    options,
    async (t) => {
      const value = await fixture(t),
        output = join(value.directory, "output");
      const result = spawnSync(
        "bash",
        [
          "packages/desktop/scripts/macos-notarize.sh",
          value.app,
          output,
          "1.0.0-beta.14",
        ],
        {
          cwd: root,
          env: { ...value.env, NOTARY_STATUS: "Invalid", NOTARY_EXIT: exit },
          encoding: "utf8",
        },
      );
      assert.notEqual(result.status, 0);
      await access(join(value.directory, "rejection-log"));
      await assert.rejects(
        access(join(output, "AgentWechat-v1.0.0-beta.14-macos-arm64.dmg")),
      );
    },
  );
test(
  "an invalid app signature prevents submitting it for notarization",
  options,
  async (t) => {
    const value = await fixture(t);
    const result = spawnSync(
      "bash",
      [
        "packages/desktop/scripts/macos-notarize.sh",
        value.app,
        join(value.directory, "output"),
        "1.0.0-beta.14",
      ],
      {
        cwd: root,
        env: { ...value.env, SIGNATURE_EXIT: "1" },
        encoding: "utf8",
      },
    );
    assert.notEqual(result.status, 0);
    await assert.rejects(access(join(value.directory, "submissions")));
  },
);
test(
  "credential cleanup preserves keychain paths with spaces and runs after restore failure",
  options,
  async (t) => {
    const value = await fixture(t);
    const previous = join(value.directory, "login with spaces.keychain-db");
    await writeFile(
      join(value.directory, "agent-wechat-original-keychains"),
      JSON.stringify(previous),
    );
    await writeFile(
      join(value.directory, "agent-wechat-signing.keychain-db"),
      "fixture",
    );
    await writeFile(
      join(value.directory, "agent-wechat-signing.p12"),
      "fixture",
    );
    const result = spawnSync(
      "bash",
      ["packages/desktop/scripts/macos-cleanup.sh"],
      { cwd: root, env: value.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      JSON.parse(
        await readFile(join(value.directory, "restored-keychains"), "utf8"),
      ).at(-1),
      previous,
    );
    await access(join(value.directory, "deleted-keychain"));
    await assert.rejects(
      access(join(value.directory, "agent-wechat-signing.p12")),
    );
  },
);
