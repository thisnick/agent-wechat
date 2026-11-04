import { bindCallVoiceSecret } from "./calls-secrets.ts";
import { CALL_SESSION_KEY_PATTERN, isCallSessionKey, MAX_CALL_SESSION_KEY_LENGTH } from "./calls-session.ts";

const consultPolicies = ["auto", "substantive", "always"] as const;
const consultThinkingLevels = ["off", "minimal", "low", "medium", "high", "xhigh", "adaptive", "max", "ultra"] as const;
type ConsultPolicy = (typeof consultPolicies)[number];
type ConsultThinkingLevel = (typeof consultThinkingLevels)[number];
export const callSessionScopes = ["per-contact", "per-call"] as const;
export type CallSessionScope = (typeof callSessionScopes)[number];

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
    agentContext?: { enabled?: boolean };
    recentContext?: { enabled?: boolean; maxMessages?: number };
    consultPolicy?: ConsultPolicy;
    consultThinkingLevel?: ConsultThinkingLevel;
    consultFastMode?: boolean;
    providers?: { openai?: { apiKey?: unknown } };
  };
  toolPolicy?: "none" | "safe-read-only" | "owner";
  /** Host agent consultation creates its own session; opt in to that retention. */
  allowConsultSessionPersistence?: boolean;
  consultation?: { timeoutMs?: number; inboundSessionKey?: string; summary?: boolean };
  /** Which session consultations run in, as in OpenClaw's voice-call plugin. */
  sessionScope?: CallSessionScope;
  maxDurationSeconds?: number;
};
export type WeChatCallsConfig = { voice?: WeChatVoiceCallsConfig; video?: { enabled?: boolean } };
export type ResolvedWeChatVoiceCallsConfig = Required<Omit<WeChatVoiceCallsConfig, "realtime" | "consultation">> & {
  realtime: { provider: string; model?: string; voice?: string; instructions?: string; agentContext: { enabled: boolean };
    recentContext: { enabled: boolean; maxMessages: number };
    consultPolicy: ConsultPolicy; consultThinkingLevel?: ConsultThinkingLevel; consultFastMode?: boolean };
  consultation: { timeoutMs: number; inboundSessionKey?: string; summary: boolean };
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
  const agentContext = realtime?.agentContext;
  const recentContext = realtime?.recentContext;
  const consultation = value.consultation ?? {};
  if (!object(realtime) || !identifier(realtime.provider) || !identifier(value.agentId)
    || ![undefined, "disabled", "allowlist"].includes(value.inboundPolicy)
    || ![undefined, "disabled", "allowlist"].includes(value.outboundPolicy)
    || ![undefined, "none", "safe-read-only", "owner"].includes(value.toolPolicy)
    || [realtime.model, realtime.voice].some(item => item !== undefined && !identifier(item))
    || (consultation.inboundSessionKey !== undefined && !isCallSessionKey(consultation.inboundSessionKey))
    || (consultation.summary !== undefined && typeof consultation.summary !== "boolean")
    || (value.sessionScope !== undefined && !callSessionScopes.includes(value.sessionScope))
    || (recentContext !== undefined && (!object(recentContext) || Object.keys(recentContext).some(key => !["enabled", "maxMessages"].includes(key))
      || (recentContext.enabled !== undefined && typeof recentContext.enabled !== "boolean")
      || (recentContext.maxMessages !== undefined && (!Number.isInteger(recentContext.maxMessages) || recentContext.maxMessages < 1 || recentContext.maxMessages > 50))))
    || (realtime.consultPolicy !== undefined && !consultPolicies.includes(realtime.consultPolicy))
    || (realtime.consultThinkingLevel !== undefined && !consultThinkingLevels.includes(realtime.consultThinkingLevel))
    || (realtime.consultFastMode !== undefined && typeof realtime.consultFastMode !== "boolean")
    || (realtime.instructions !== undefined && (typeof realtime.instructions !== "string" || realtime.instructions.length > 4000 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(realtime.instructions)))
    || (agentContext !== undefined && (!object(agentContext) || Object.keys(agentContext).some(key => key !== "enabled")
      || (agentContext.enabled !== undefined && typeof agentContext.enabled !== "boolean")))) throw Error("Invalid calling configuration");
  const maxDurationSeconds = value.maxDurationSeconds ?? 3600;
  const timeoutMs = consultation.timeoutMs ?? 30000;
  if (!Number.isInteger(maxDurationSeconds) || maxDurationSeconds < 30 || maxDurationSeconds > 3600) throw Error("Invalid calling duration");
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 120000) throw Error("Invalid calling consultation timeout");
  if ((value.toolPolicy === "safe-read-only" || value.toolPolicy === "owner") && value.allowConsultSessionPersistence !== true) throw Error("Calling consultation requires opting in to host session persistence");
  return bindCallVoiceSecret({ enabled: true, inboundPolicy: value.inboundPolicy ?? "disabled", inboundAllowFrom: names(value.inboundAllowFrom),
    outboundPolicy: value.outboundPolicy ?? "disabled", outboundAllowTo: names(value.outboundAllowTo), outboundToolAllowFrom: names(value.outboundToolAllowFrom),
    agentId: value.agentId, realtime: { provider: realtime.provider, model: realtime.model, voice: realtime.voice, instructions: realtime.instructions,
      agentContext: { enabled: agentContext?.enabled === true },
      recentContext: { enabled: recentContext?.enabled === true, maxMessages: recentContext?.maxMessages ?? 10 }, consultPolicy: realtime.consultPolicy ?? "auto",
      consultThinkingLevel: realtime.consultThinkingLevel, consultFastMode: realtime.consultFastMode },
    toolPolicy: value.toolPolicy ?? (value.allowConsultSessionPersistence === true ? "safe-read-only" : "none"),
    allowConsultSessionPersistence: value.allowConsultSessionPersistence === true,
    consultation: { timeoutMs, inboundSessionKey: consultation.inboundSessionKey, summary: consultation.summary ?? true },
    sessionScope: value.sessionScope ?? "per-contact", maxDurationSeconds }, realtime.providers?.openai?.apiKey);
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
        provider: { ...nameSchema, description: "Use openai. Other providers fail preflight without fallback." },
        model: { ...nameSchema, description: "Speech model, such as gpt-realtime-2.1 or the continuous gpt-live-1-codex; omitted uses the selected provider's default." },
        voice: { ...nameSchema, description: "Spoken voice; omitted uses the selected provider's default." },
        instructions: { type: "string", maxLength: 4000, description: "Additional conversation guidance; does not grant tool or account-owner authority." },
        agentContext: { type: "object", additionalProperties: false, properties: {
          enabled: { type: "boolean", default: false, description: "Send bounded excerpts of the selected agent's IDENTITY.md and SOUL.md to the Realtime provider. Configured agent identity is used independently." },
        } },
        recentContext: { type: "object", additionalProperties: false, properties: {
          enabled: { type: "boolean", default: false, description: "When a call starts, send the Realtime provider the summaries of the last few calls with this contact and the latest messages of your WeChat chat with them, so the voice model knows what you last talked about." },
          maxMessages: { type: "integer", minimum: 1, maximum: 50, default: 10, description: "How many of the latest chat messages to include." },
        } },
        consultPolicy: { type: "string", enum: consultPolicies, default: "auto", description: "Guide when the voice model consults. Always can use a final caller utterance as the consultation question when the voice model does not consult. GPT-Live models do not support always." },
        consultThinkingLevel: { type: "string", enum: consultThinkingLevels, description: "Thinking level for the regular OpenClaw consultation run." },
        consultFastMode: { type: "boolean", description: "Set fast mode for the regular OpenClaw consultation run when its model supports it." },
        providers: { type: "object", additionalProperties: false, properties: {
          openai: { type: "object", additionalProperties: false, properties: {
            apiKey: { description: "Optional OpenClaw SecretRef or API-key string. Omitted uses the selected agent's OpenAI auth profile or Gateway environment." },
          } },
        } },
      } },
      toolPolicy: { type: "string", enum: ["none", "safe-read-only", "owner"], description: "Owner uses the normal agent tool policy only for an exact agent-wechat entry in commands.ownerAllowFrom matching the connected voice peer. Other admitted peers get read-only consultation. Defaults to safe-read-only with session persistence, otherwise none." },
      allowConsultSessionPersistence: { type: "boolean", default: false, description: "Required for consultation. OpenClaw creates separate agent sessions for calls which can persist. This adapter stores no recordings; with consultation.summary it keeps the last few call summaries for each contact." },
      consultation: { type: "object", additionalProperties: false, properties: {
        timeoutMs: { type: "integer", minimum: 1000, maximum: 120000, default: 30000 },
        inboundSessionKey: { type: "string", pattern: CALL_SESSION_KEY_PATTERN, maxLength: MAX_CALL_SESSION_KEY_LENGTH,
          description: "Optional existing session of the selected agent to fork into a call's session when that session is first created by an admitted incoming call." },
        summary: { type: "boolean", default: true, description: "After an answered call, give the call's transcript to the agent in the call's session and keep its short summary. Calls placed with the wechat_call tool also report the summary to the session that placed them." },
      } },
      sessionScope: { type: "string", enum: callSessionScopes, default: "per-contact", description: "per-contact keeps one session for each contact, so the agent remembers earlier calls with them; per-call starts every call in a new session." },
      maxDurationSeconds: { type: "integer", minimum: 30, maximum: 3600, default: 3600 },
    } },
    video: { type: "object", additionalProperties: false, properties: {
      enabled: { type: "boolean", default: false, description: "Allow video independently of voice. Current video capability is unavailable; permission alone never answers a video call or routes it as voice." },
    } },
  },
};
