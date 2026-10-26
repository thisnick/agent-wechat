import {
  generateKeyPairSync,
  randomBytes,
  sign,
  createHash,
} from "node:crypto";
import { mergeManifests } from "../scripts/merge-manifests.mjs";
import { updaterFiles, verifyUpdate } from "../scripts/updater.mjs";
import { bundleType } from "../scripts/bundle-type.mjs";
import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
const root = resolve(import.meta.dirname, "../../..");
const { version } = JSON.parse(
  await readFile(join(root, "packages/cli/package.json"))
);
test("final desktop manifests reject bytes changed after signing or packaging", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "desktop-manifests-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const platforms = [
    "macos-arm64",
    "windows-x64",
    "windows-arm64",
    "linux-x64",
    "linux-arm64",
  ];
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const id = randomBytes(8);
  const key = Buffer.from(
    "untrusted comment: fixture\n" +
      Buffer.concat([
        Buffer.from("Ed"),
        id,
        publicKey.export({ format: "der", type: "spki" }).subarray(-32),
      ]).toString("base64") +
      "\n"
  ).toString("base64");
  const signed = (bytes) => {
    const signature = sign(
      null,
      createHash("blake2b512").update(bytes).digest(),
      privateKey
    );
    const comment = `version:${version}`;
    return Buffer.from(
      "untrusted comment: fixture\n" +
        Buffer.concat([Buffer.from("ED"), id, signature]).toString("base64") +
        "\ntrusted comment: " +
        comment +
        "\n" +
        sign(
          null,
          Buffer.concat([signature, Buffer.from(comment)]),
          privateKey
        ).toString("base64") +
        "\n"
    ).toString("base64");
  };
  const fragments = [];
  for (const platform of platforms) {
    const fragment = join(directory, platform);
    await mkdir(fragment);
    fragments.push(fragment);
    const file = join(
      fragment,
      `AgentWechat-v${version}-${updaterFiles[platform][1]}`
    );
    const bytes = Buffer.from(`signed fixture ${platform}`);
    const signature = signed(bytes);
    await writeFile(file, bytes);
    await writeFile(file + ".sig", signature);
    verifyUpdate(bytes, signature, key, version);
    assert.throws(
      () => verifyUpdate(Buffer.from("tampered"), signature, key, version),
      /verification failed/
    );
    assert.throws(
      () => verifyUpdate(bytes, signature, key, "9.0.0"),
      /signed version/
    );
    execFileSync(
      process.execPath,
      ["packages/desktop/scripts/manifest.mjs", fragment, platform],
      { cwd: root }
    );
  }
  const output = join(directory, "output");
  await mergeManifests(output, fragments, key);
  const manifest = JSON.parse(
    await readFile(join(output, `AgentWechat-v${version}-checksums.json`))
  );
  assert.equal(manifest.assets.length, 11);
  const feed = JSON.parse(await readFile(join(output, "desktop-updater.json")));
  assert.equal(Object.keys(feed.platforms).length, 5);
  assert.equal(feed.version, version);
  await writeFile(
    join(fragments[0], `AgentWechat-v${version}-macos-arm64.app.tar.gz`),
    "changed bytes"
  );
  const changed = spawnSync(
    process.execPath,
    [
      "packages/desktop/scripts/merge-manifests.mjs",
      join(directory, "other"),
      ...fragments,
    ],
    { cwd: root, encoding: "utf8" }
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
    { cwd: root, encoding: "utf8" }
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Missing desktop platforms/);
});

test("installer identity is set once before signing and preserves all other bytes", () => {
  const bytes = Buffer.from("before__TAURI_BUNDLE_TYPE_VAR_UNKafter");
  const patched = bundleType(bytes, "deb");
  assert.equal(patched.toString(), "before__TAURI_BUNDLE_TYPE_VAR_DEBafter");
  assert.throws(
    () => bundleType(patched, "deb"),
    /unsigned Tauri bundle marker/
  );
  assert.throws(
    () => bundleType(Buffer.concat([bytes, bytes]), "nsis"),
    /unsigned Tauri bundle marker/
  );
});

test("release verification accepts Tauri CLI signatures and rejects changed bytes or versions", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "desktop-updater-signature-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const cli = new URL(
    "../node_modules/@tauri-apps/cli/tauri.js",
    import.meta.url
  );
  const key = join(directory, "fixture.key"),
    file = join(directory, "fixture.bin");
  execFileSync(
    process.execPath,
    [
      cli.pathname,
      "signer",
      "generate",
      "--ci",
      "-p",
      "fixture-password",
      "-w",
      key,
    ],
    { stdio: "pipe" }
  );
  await writeFile(file, "updater signature fixture");
  const privateKey = (await readFile(key, "utf8")).trim();
  execFileSync(
    process.execPath,
    [cli.pathname, "signer", "sign", "--app-version", version, file],
    {
      stdio: "pipe",
      env: {
        ...process.env,
        TAURI_SIGNING_PRIVATE_KEY: privateKey,
        TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "fixture-password",
      },
    }
  );
  const signature = (await readFile(file + ".sig", "utf8")).trim();
  const publicKey = (await readFile(key + ".pub", "utf8")).trim();
  verifyUpdate(await readFile(file), signature, publicKey, version);
  assert.throws(
    () => verifyUpdate(Buffer.from("changed"), signature, publicKey, version),
    /verification failed/
  );
  assert.throws(() =>
    verifyUpdate(
      Buffer.from("updater signature fixture"),
      signature,
      publicKey,
      "99.0.0"
    )
  );
});
