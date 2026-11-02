/** Calling permissions are independent of direct-message permission. */
export type WeChatCallsConfig = {
  enabled?: boolean;
  inboundPolicy?: "disabled" | "allowlist";
  inboundAllowFrom?: string[];
  outboundPolicy?: "disabled" | "allowlist";
  outboundAllowTo?: string[];
  outboundToolAllowFrom?: string[];
  provider?: string;
  model?: string;
  voice?: string;
  agentId?: string;
  toolPolicy?: "none" | "safe-read-only";
  /** Host agent consultation creates its own session; opt in to that retention. */
  allowConsultSessionPersistence?: boolean;
  maxDurationSeconds?: number;
};
export type ResolvedWeChatCallsConfig = Required<Omit<WeChatCallsConfig, "model" | "voice">> & Pick<WeChatCallsConfig, "model" | "voice">;
const identifier = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 200 && !/[\u0000-\u001f\u007f]/.test(value);
function names(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 128 || value.some(name => !identifier(name) || name === "*")) throw Error("Invalid calling allowlist");
  return [...new Set(value)];
}
export function resolveWeChatCallsConfig(value: WeChatCallsConfig | undefined): ResolvedWeChatCallsConfig | undefined {
  if (!value || value.enabled !== true) return undefined;
  if (!identifier(value.provider) || !identifier(value.agentId)
    || ![undefined, "disabled", "allowlist"].includes(value.inboundPolicy)
    || ![undefined, "disabled", "allowlist"].includes(value.outboundPolicy)
    || ![undefined, "none", "safe-read-only"].includes(value.toolPolicy)
    || [value.model, value.voice].some(item => item !== undefined && !identifier(item))) throw Error("Invalid calling configuration");
  const maxDurationSeconds = value.maxDurationSeconds ?? 300;
  if (!Number.isInteger(maxDurationSeconds) || maxDurationSeconds < 30 || maxDurationSeconds > 3600) throw Error("Invalid calling duration");
  if (value.toolPolicy === "safe-read-only" && value.allowConsultSessionPersistence !== true) throw Error("Calling consultation requires opting in to host session persistence");
  return { enabled: true, inboundPolicy: value.inboundPolicy ?? "disabled", inboundAllowFrom: names(value.inboundAllowFrom),
    outboundPolicy: value.outboundPolicy ?? "disabled", outboundAllowTo: names(value.outboundAllowTo), outboundToolAllowFrom: names(value.outboundToolAllowFrom),
    provider: value.provider, agentId: value.agentId, model: value.model, voice: value.voice, toolPolicy: value.toolPolicy ?? "none",
    allowConsultSessionPersistence: value.allowConsultSessionPersistence === true, maxDurationSeconds };
}
export const callsConfigSchema = {
  type: "object", additionalProperties: false,
  description: "Direct voice calling using OpenAI's PCM response bridge. Disabled by default. Video, conferences and continuous GPT-Live output are unsupported.",
  properties: {
    enabled: { type: "boolean", default: false },
    inboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, inboundAllowFrom: { type: "array", maxItems: 128, items: { type: "string", minLength: 1, maxLength: 200 } },
    outboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, outboundAllowTo: { type: "array", maxItems: 128, items: { type: "string", minLength: 1, maxLength: 200 } },
    outboundToolAllowFrom: { type: "array", maxItems: 128, items: { type: "string", minLength: 1, maxLength: 200 } },
    provider: { type: "string", minLength: 1, maxLength: 200, description: "Use openai with its existing OpenClaw authentication. Other providers fail preflight; there is no automatic fallback." }, model: { type: "string", minLength: 1, maxLength: 200 }, voice: { type: "string", minLength: 1, maxLength: 200 },
    agentId: { type: "string", minLength: 1, maxLength: 200 }, toolPolicy: { type: "string", enum: ["none", "safe-read-only"] },
    allowConsultSessionPersistence: { type: "boolean", default: false, description: "Required for safe-read-only consultation: the host creates a separate per-call agent session, which can persist. No consultation or call transcript is stored by this adapter by default." }, maxDurationSeconds: { type: "integer", minimum: 30, maximum: 3600 },
  },
};
