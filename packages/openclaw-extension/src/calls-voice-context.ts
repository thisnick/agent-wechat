import type { OpenClawConfig } from "openclaw/plugin-sdk/core";

/** Use the host's profile/bootstrap handling, including hooks and workspace access.
 * Regular agent turns already receive their bootstrap context from OpenClaw. */
export async function resolveCallVoiceContext(cfg: OpenClawConfig, agentId: string, includeWorkspace: boolean): Promise<string | undefined> {
  if (!cfg || typeof cfg !== "object") return;
  const runtimePath: string = "openclaw/plugin-sdk/agent-runtime";
  const contextPath: string = "openclaw/plugin-sdk/realtime-bootstrap-context";
  const [runtime, sdk] = await Promise.all([import(runtimePath), import(contextPath)]);
  if (typeof sdk.resolveRealtimeVoiceAgentContextInstructions !== "function") throw Error("Calling voice context SDK unavailable");
  const includeFiles = includeWorkspace && !!runtime.resolveAgentConfig(cfg, agentId);
  return sdk.resolveRealtimeVoiceAgentContextInstructions({ config: cfg, agentId, includeIdentity: true,
    files: includeFiles ? ["IDENTITY.md", "USER.md", "SOUL.md"] : [], maxChars: 6000 });
}
