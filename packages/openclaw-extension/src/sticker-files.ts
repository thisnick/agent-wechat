import { posix } from "node:path";
import type { OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveSandboxContext } from "openclaw/plugin-sdk/agent-harness-runtime";
import { getAgentWorkspaceAccess } from "openclaw/plugin-sdk/agent-workspace-runtime";

type FileBridge = {
  readFile(params: { filePath: string; cwd?: string; maxBytes?: number; signal?: AbortSignal }): Promise<Buffer>;
  createFileExclusive?: (params: { filePath: string; cwd?: string; data: Buffer; mkdir?: boolean; signal?: AbortSignal }) => Promise<"created" | "exists">;
};
/** A temporary, host-owned view of the active execution workspace. */
export type StickerFiles = {
  read(path: string, maxBytes: number): Promise<Buffer>;
  create(path: string, bytes: Buffer): Promise<void>;
};
function relativePath(path: string): string {
  if (!path || path.length > 4096 || /[\\\u0000-\u001f\u007f]/.test(path) || posix.isAbsolute(path)
    || path.split("/").some(part => part === ".." || part === "")) throw Error("Sticker file must be a path inside the execution workspace");
  return posix.normalize(path);
}
/** Read/write through OpenClaw's active SSH sandbox or paired-node binding.
 * A missing or stopped binding is an error; gateway files are never a fallback. */
export async function withStickerFiles<T>(
  context: OpenClawPluginToolContext,
  guard: () => void,
  run: (files: StickerFiles) => Promise<T>,
  signal?: AbortSignal,
  dependencies = { resolveSandboxContext, getAgentWorkspaceAccess },
): Promise<T> {
  guard(); signal?.throwIfAborted();
  const config = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!config || !context.workspaceDir || !context.sessionKey) throw Error("Sticker execution workspace is unavailable");
  const sandbox = await dependencies.resolveSandboxContext({ config, sessionKey: context.sessionKey,
    agentId: context.agentId, workspaceDir: context.workspaceDir, requireCurrentConfig: true, assertCurrent: guard });
  try {
    guard();
    let bridge: FileBridge | undefined;
    let cwd: string;
    if (sandbox) {
      bridge = sandbox.fsBridge;
      cwd = sandbox.containerWorkdir;
    } else {
      const workspace = dependencies.getAgentWorkspaceAccess(context.workspaceDir);
      bridge = workspace?.bridge;
      cwd = context.workspaceDir;
    }
    if (!bridge) throw Error("OpenClaw has no file-transfer binding for the execution workspace");
    const bound = bridge;
    return await run({
      read: async (path, maxBytes) => {
        if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw Error("Invalid sticker byte limit");
        guard(); signal?.throwIfAborted();
        const bytes = await bound.readFile({ filePath: relativePath(path), cwd, maxBytes, signal });
        guard(); signal?.throwIfAborted();
        if (!bytes.length || bytes.length > maxBytes) throw Error("Sticker source exceeds the preparation limit");
        return bytes;
      },
      create: async (path, bytes) => {
        if (!bound.createFileExclusive) throw Error("OpenClaw workspace does not support exclusive file creation");
        guard(); signal?.throwIfAborted();
        const result = await bound.createFileExclusive({ filePath: relativePath(path), cwd, data: bytes, mkdir: true, signal });
        try { guard(); signal?.throwIfAborted(); }
        catch {
          if(result==="created") throw Error("Sticker output may already exist: invocation ended after staging. Inspect the execution workspace before retrying.");
          throw Error("Sticker staging authority ended; inspect the execution workspace before retrying.");
        }
        if (result !== "created") throw Error("Sticker output already exists; choose a new filename");
      },
    });
  } finally { sandbox?.[Symbol.dispose]?.(); }
}
