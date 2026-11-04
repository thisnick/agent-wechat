import type { OpenClawConfig } from "openclaw/plugin-sdk/core";

const PROFILE_FILES = ["IDENTITY.md", "SOUL.md"] as const;
const MAX_CONTEXT_CHARS = 6000;
const MAX_FILE_BYTES = 64 * 1024;

function clip(text: string, limit: number): string {
  if (text.length <= limit) return text;
  const marker = "\n[truncated]";
  return text.slice(0, limit - marker.length).replace(/[\uD800-\uDBFF]$/, "").trimEnd() + marker;
}

/** The configured identity is safe to project by default. Workspace persona
 * is separately opted in and reads only the two selected profile files. */
export async function resolveCallVoiceContext(cfg: OpenClawConfig, agentId: string, includeWorkspace: boolean): Promise<string | undefined> {
  if (!cfg || typeof cfg !== "object") return;
  const agentRuntimePath: string = "openclaw/plugin-sdk/agent-runtime";
  const sdk = await import(agentRuntimePath).catch(() => undefined) as typeof import("openclaw/plugin-sdk/agent-runtime") | undefined;
  if (!sdk) return;
  const identity = sdk.resolveAgentIdentity(cfg, agentId);
  const fields = [
    ["Name", identity?.name], ["Emoji", identity?.emoji], ["Theme", identity?.theme],
  ].flatMap(([label, value]) => {
    const text = typeof value === "string" ? value.replace(/\s+/g, " ").trim() : "";
    return text ? [`- ${label}: ${clip(text, 256)}`] : [];
  });
  const sections = fields.length ? [`Configured agent identity:\n${fields.join("\n")}`] : [];
  // Workspace resolution can synthesize a path for an unknown id. Require an
  // actual host agent, including the legacy implicit main agent.
  if (includeWorkspace && sdk.resolveAgentConfig(cfg, agentId)) {
    const securityPath: string = "openclaw/plugin-sdk/security-runtime";
    const security = await import(securityPath).catch(() => undefined) as typeof import("openclaw/plugin-sdk/security-runtime") | undefined;
    const workspace = await security?.root(sdk.resolveAgentWorkspaceDir(cfg, agentId), {
      symlinks: "reject", hardlinks: "reject", maxBytes: MAX_FILE_BYTES,
    }).catch(() => undefined);
    if (workspace) {
      for (const file of PROFILE_FILES) {
        const text = (await workspace.readText(file).catch(() => undefined))?.trim();
        if (text) sections.push(`Workspace voice profile (${file}):\n${clip(text, MAX_CONTEXT_CHARS / PROFILE_FILES.length)}`);
      }
    }
  }
  return sections.length ? clip(sections.join("\n\n"), MAX_CONTEXT_CHARS) : undefined;
}
