import test from "node:test";
import assert from "node:assert/strict";
import { withStickerFiles } from "./sticker-files.ts";
const context = { config: {}, workspaceDir: "/gateway/work", sessionKey: "test", agentId: "main" };

test("SSH workspace bytes and exclusive output paths use the host bridge", async () => {
  const calls: unknown[] = []; let disposed = 0;
  const sandbox = { containerWorkdir: "/remote/work", fsBridge: {
    readFile: async (request: unknown) => { calls.push(request); return Buffer.from("remote bytes"); },
    createFileExclusive: async (request: unknown) => { calls.push(request); return "created"; },
  }, [Symbol.dispose]: () => { disposed++; } };
  const result = await withStickerFiles(context as any, () => {}, async files => {
    const bytes = await files.read("outputs/source.gif", 20);
    await files.create("stickers/output.gif", bytes); return bytes.toString();
  }, undefined, { resolveSandboxContext: async () => sandbox, getAgentWorkspaceAccess: () => { assert.fail("SSH cannot fall back to gateway mapping"); } } as any);
  assert.equal(result, "remote bytes"); assert.equal(disposed, 1);
  assert.deepEqual(calls[0], { filePath: "outputs/source.gif", cwd: "/remote/work", maxBytes: 20, signal: undefined });
  assert.equal((calls[1] as any).cwd, "/remote/work");
});
test("paired-node transport failures never read an unrelated gateway path", async () => {
  const error = new Error("node disconnected");
  await assert.rejects(withStickerFiles(context as any, () => {}, files => files.read("same.gif", 20), undefined,
    { resolveSandboxContext: async () => null, getAgentWorkspaceAccess: () => ({ bridge: { readFile: async () => { throw error; } } }) } as any), error);
});
test("escaping paths and replacement writes are refused", async () => {
  let reads = 0, writes = 0;
  const dependencies = { resolveSandboxContext: async () => null, getAgentWorkspaceAccess: () => ({ bridge: {
    readFile: async () => { reads++; return Buffer.from("x"); }, createFileExclusive: async () => { writes++; return "exists"; },
  } }) } as any;
  await withStickerFiles(context as any, () => {}, async files => {
    for (const path of ["../secret", "/absolute", "dir/../../secret", "dir\\secret"]) await assert.rejects(files.read(path, 10));
    await assert.rejects(files.create("existing.gif", Buffer.from("x")), /already exists/);
  }, undefined, dependencies);
  assert.equal(reads, 0); assert.equal(writes, 1);
});
