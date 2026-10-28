import test from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

test("pinned host tools list exposes message mentions and required queues retain validated options", { timeout: 45000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), "wechat-host-options-")); t.after(() => rm(root, { recursive: true, force: true }));
  for (const name of ["home", "state", "workspace"]) await mkdir(join(root, name), { mode: 0o700 });
  const extension = fileURLToPath(new URL("../", import.meta.url));
  await promisify(execFile)(process.execPath, ["--experimental-strip-types", join(extension, "test/pinned-outbound-options.mjs"), extension, root], {
    env: { PATH: process.env.PATH, HOME: join(root, "home"), OPENCLAW_STATE_DIR: join(root, "state"), OPENCLAW_CONFIG_PATH: join(root, "state/openclaw.json"), TZ: "UTC" },
    timeout: 40000, maxBuffer: 1024 * 1024,
  });
  const result = JSON.parse(await readFile(join(root, "result.json"), "utf8"));
  assert.deepEqual(result.checks, ["prepared-normal", "prepared-original", "forced-core-original", "document-alias", "queued-original", "queued-document-alias"]);
  assert.equal(result.runtime, "2026.10.7"); assert.equal(result.schemaIncludesQuality, true); assert.equal(result.queuedConflictRejected, true);
  assert.equal(result.messageToolListed, true); assert.equal(result.schemaIncludesMentions, true); assert.equal(result.queuedMentionsRejected, true);
  assert.deepEqual(result.mentionChecks, ["prepared-members", "prepared-all", "forced-core-members", "queued-members", "queued-all"]);
  assert.equal(result.networkCalls, 0); assert.equal(result.providerSends, 0);
});
