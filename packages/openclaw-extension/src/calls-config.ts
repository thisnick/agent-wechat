import { bindCallVoiceSecret } from "./calls-secrets.ts";

/** Calling permissions and providers are configured independently for each type. */
export type WeChatVoiceCallsConfig = {
  enabled?: boolean;
  inboundPolicy?: "disabled" | "allowlist";
  inboundAllowFrom?: string[];
  outboundPolicy?: "disabled" | "allowlist";
  outboundAllowTo?: string[];
  outboundToolAllowFrom?: string[];
  agentId?: string;
  realtime?: {
    provider?: string;
    model?: string;
    voice?: string;
    instructions?: string;
    providers?: { openai?: { apiKey?: unknown } };
  };
  toolPolicy?: "none" | "safe-read-only";
  /** Host agent consultation creates its own session; opt in to that retention. */
  allowConsultSessionPersistence?: boolean;
  consultation?: { model?: string; timeoutMs?: number };
  maxDurationSeconds?: number;
};
export type WeChatCallsConfig = { voice?: WeChatVoiceCallsConfig; video?: { enabled?: boolean } };
export type ResolvedWeChatVoiceCallsConfig = Required<Omit<WeChatVoiceCallsConfig, "realtime" | "consultation">> & {
  realtime: { provider: string; model?: string; voice?: string; instructions?: string };
  consultation: { model?: string; timeoutMs: number };
};
export type ResolvedWeChatCallsConfig = { voice?: ResolvedWeChatVoiceCallsConfig; video: { enabled: boolean } };
const identifier = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 200 && !/[\u0000-\u001f\u007f]/.test(value);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
function names(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 128 || value.some(name => !identifier(name) || name === "*")) throw Error("Invalid calling allowlist");
  return [...new Set(value)];
}
function voiceConfig(value: WeChatVoiceCallsConfig | undefined): ResolvedWeChatVoiceCallsConfig | undefined {
  if (!value || value.enabled !== true) return undefined;
  const realtime = value.realtime;
  const consultation = value.consultation ?? {};
  if (!object(realtime) || !identifier(realtime.provider) || !identifier(value.agentId)
    || ![undefined, "disabled", "allowlist"].includes(value.inboundPolicy)
    || ![undefined, "disabled", "allowlist"].includes(value.outboundPolicy)
    || ![undefined, "none", "safe-read-only"].includes(value.toolPolicy)
    || [realtime.model, realtime.voice, consultation.model].some(item => item !== undefined && !identifier(item))
    || (consultation.model !== undefined && !/^[^/]+\/.+$/.test(consultation.model))
    || (realtime.instructions !== undefined && (typeof realtime.instructions !== "string" || realtime.instructions.length > 4000 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(realtime.instructions)))) throw Error("Invalid calling configuration");
  const maxDurationSeconds = value.maxDurationSeconds ?? 300;
  const timeoutMs = consultation.timeoutMs ?? 30000;
  if (!Number.isInteger(maxDurationSeconds) || maxDurationSeconds < 30 || maxDurationSeconds > 3600) throw Error("Invalid calling duration");
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 120000) throw Error("Invalid calling consultation timeout");
  if (value.toolPolicy === "safe-read-only" && value.allowConsultSessionPersistence !== true) throw Error("Calling consultation requires opting in to host session persistence");
  return bindCallVoiceSecret({ enabled: true, inboundPolicy: value.inboundPolicy ?? "disabled", inboundAllowFrom: names(value.inboundAllowFrom),
    outboundPolicy: value.outboundPolicy ?? "disabled", outboundAllowTo: names(value.outboundAllowTo), outboundToolAllowFrom: names(value.outboundToolAllowFrom),
    agentId: value.agentId, realtime: { provider: realtime.provider, model: realtime.model, voice: realtime.voice, instructions: realtime.instructions },
    toolPolicy: value.toolPolicy ?? "none", allowConsultSessionPersistence: value.allowConsultSessionPersistence === true,
    consultation: { model: consultation.model, timeoutMs }, maxDurationSeconds }, realtime.providers?.openai?.apiKey);
}
export function resolveWeChatCallsConfig(value: WeChatCallsConfig | undefined): ResolvedWeChatCallsConfig | undefined {
  if (!value) return undefined;
  if (!object(value) || Object.keys(value).some(key => !["voice", "video"].includes(key))
    || [value.voice, value.video].some(item => item !== undefined && (!object(item) || (item.enabled !== undefined && typeof item.enabled !== "boolean")))) throw Error("Invalid calling configuration; use calls.voice.enabled and calls.video.enabled");
  const voice = voiceConfig(value.voice);
  const video = { enabled: value.video?.enabled === true };
  return voice || video.enabled ? { voice, video } : undefined;
}
const nameSchema = { type: "string", minLength: 1, maxLength: 200 };
const allowlistSchema = { type: "array", maxItems: 128, items: nameSchema };
export const callsConfigSchema = {
  type: "object", additionalProperties: false,
  description: "Independent voice and video permissions. Both default off; the current backend supports direct voice only.",
  properties: {
    voice: { type: "object", additionalProperties: false, properties: {
      enabled: { type: "boolean", default: false, description: "Enable direct voice calling. Disabled voice calls are ignored without answering or declining." },
      inboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, inboundAllowFrom: allowlistSchema,
      outboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, outboundAllowTo: allowlistSchema, outboundToolAllowFrom: allowlistSchema,
      agentId: nameSchema,
      realtime: { type: "object", additionalProperties: false, properties: {
        provider: { ...nameSchema, description: "Use openai. Other providers and continuous-output models fail preflight without fallback." },
        model: { ...nameSchema, description: "Realtime speech model; omitted uses the selected provider's default." },
        voice: { ...nameSchema, description: "Spoken voice; omitted uses the selected provider's default." },
        instructions: { type: "string", maxLength: 4000, description: "Additional conversation guidance; does not grant tool or account-owner authority." },
        providers: { type: "object", additionalProperties: false, properties: {
          openai: { type: "object", additionalProperties: false, properties: {
            apiKey: { description: "Optional OpenClaw SecretRef or API-key string. Omitted uses the selected agent's OpenAI auth profile or Gateway environment." },
          } },
        } },
      } },
      toolPolicy: { type: "string", enum: ["none", "safe-read-only"], default: "none" },
      allowConsultSessionPersistence: { type: "boolean", default: false, description: "Required for safe-read-only consultation: OpenClaw creates a separate per-call agent session which can persist. This adapter does not store transcripts or recordings." },
      consultation: { type: "object", additionalProperties: false, properties: {
        model: { ...nameSchema, pattern: "^[^/]+/.+$", description: "Agent-consult provider/model override; omitted uses the selected agent's model." },
        timeoutMs: { type: "integer", minimum: 1000, maximum: 120000, default: 30000 },
      } },
      maxDurationSeconds: { type: "integer", minimum: 30, maximum: 3600, default: 300 },
    } },
    video: { type: "object", additionalProperties: false, properties: {
      enabled: { type: "boolean", default: false, description: "Allow video independently of voice. Current video capability is unavailable; permission alone never answers a video call or routes it as voice." },
    } },
  },
};
