import { bindCallVoiceSecret } from "./calls-secrets.ts";
import { CALL_SESSION_KEY_PATTERN, isCallSessionKey, MAX_CALL_SESSION_KEY_LENGTH } from "./calls-session.ts";

const consultPolicies = ["auto", "substantive", "always"] as const;
const consultThinkingLevels = ["off", "minimal", "low", "medium", "high", "xhigh", "adaptive", "max", "ultra"] as const;
type ConsultPolicy = (typeof consultPolicies)[number];
type ConsultThinkingLevel = (typeof consultThinkingLevels)[number];
export const callSessionScopes = ["per-contact", "per-call"] as const;
export type CallSessionScope = (typeof callSessionScopes)[number];
export type WeChatConferenceGroupConfig = { autoJoin?: boolean; autoJoinAllowFrom?: string[]; toolPolicy?: "none" | "safe-read-only" | "owner"; ownerInviterToolPolicy?: "none" | "safe-read-only" | "owner"; sessionKey?: string };
export type ResolvedWeChatConferenceGroupConfig = { autoJoin: boolean; autoJoinAllowFrom?: string[]; toolPolicy: "none" | "safe-read-only" | "owner"; ownerInviterToolPolicy?: "none" | "safe-read-only" | "owner"; sessionKey?: string };

/** Calling permissions and providers are configured independently for each type. */
export type WeChatVoiceCallsConfig = {
  enabled?: boolean;
  mode?: "bidi" | "agent";
  transcription?: { provider?: string; model?: string; language?: string; apiKey?: unknown };
  inboundPolicy?: "disabled" | "allowlist";
  inboundAllowFrom?: string[];
  outboundPolicy?: "disabled" | "allowlist";
  outboundAllowTo?: string[];
  outboundToolAllowFrom?: string[];
  /** Exact group IDs. An empty map enables neither conference tools nor auto-join. */
  conferences?: { groups?: Record<string, WeChatConferenceGroupConfig> };
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
  consultation?: { timeoutMs?: number; inboundSessionKey?: string; summary?: boolean; recentMessages?: number };
  /** Which session consultations run in, as in OpenClaw's voice-call plugin. */
  sessionScope?: CallSessionScope;
  maxDurationSeconds?: number;
};
export type WeChatCallsConfig = { voice?: WeChatVoiceCallsConfig; video?: { enabled?: boolean } };
export type ResolvedWeChatVoiceCallsConfig = Required<Omit<WeChatVoiceCallsConfig, "realtime" | "consultation" | "conferences" | "transcription">> & {
  transcription: { provider: string; model?: string; language?: string };
  conferences: { groups: Record<string, ResolvedWeChatConferenceGroupConfig> };
  realtime: { provider: string; model?: string; voice?: string; instructions?: string; agentContext: { enabled: boolean };
    recentContext: { enabled: boolean; maxMessages: number };
    consultPolicy: ConsultPolicy; consultThinkingLevel?: ConsultThinkingLevel; consultFastMode?: boolean };
  consultation: { timeoutMs: number; inboundSessionKey?: string; summary: boolean; recentMessages: number };
};
export type ResolvedWeChatCallsConfig = { voice?: ResolvedWeChatVoiceCallsConfig; video: { enabled: boolean } };
const identifier = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 200 && !/[\u0000-\u001f\u007f]/.test(value);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
function names(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 128 || value.some(name => !identifier(name) || name === "*")) throw Error("Invalid calling allowlist");
  return [...new Set(value)];
}
function conferenceGroups(value: WeChatVoiceCallsConfig["conferences"], toolPolicy: ResolvedWeChatVoiceCallsConfig["toolPolicy"]): Record<string, ResolvedWeChatConferenceGroupConfig> {
  if (value === undefined) return {};
  if (!object(value) || Object.keys(value).some(key => key !== "groups") || (value.groups !== undefined && !object(value.groups))) throw Error("Invalid conference configuration");
  const entries = Object.entries(value.groups ?? {});
  if (entries.length > 128) throw Error("Too many conference groups");
  const groups = Object.create(null) as Record<string, ResolvedWeChatConferenceGroupConfig>;
  for (const [id, group] of entries) {
    if (!identifier(id) || !id.endsWith("@chatroom") || !object(group) || Object.keys(group).some(key => !["autoJoin", "autoJoinAllowFrom", "toolPolicy", "ownerInviterToolPolicy", "sessionKey"].includes(key))
      || (group.autoJoin !== undefined && typeof group.autoJoin !== "boolean")
      || ![undefined, "none", "safe-read-only", "owner"].includes(group.toolPolicy)
      || ![undefined, "none", "safe-read-only", "owner"].includes(group.ownerInviterToolPolicy)
      || ((group.toolPolicy === "owner" || group.ownerInviterToolPolicy === "owner") && toolPolicy !== "owner")
      || (group.sessionKey !== undefined && !isCallSessionKey(group.sessionKey))) throw Error("Invalid conference group policy");
    groups[id] = { autoJoin: group.autoJoin === true, ...(group.autoJoinAllowFrom !== undefined ? { autoJoinAllowFrom: names(group.autoJoinAllowFrom) } : {}), toolPolicy: toolPolicy === "none" ? "none" : group.toolPolicy ?? "safe-read-only",
      ...(group.ownerInviterToolPolicy !== undefined ? { ownerInviterToolPolicy: group.ownerInviterToolPolicy } : {}), sessionKey: group.sessionKey };
  }
  return groups;
}
function voiceConfig(value: WeChatVoiceCallsConfig | undefined): ResolvedWeChatVoiceCallsConfig | undefined {
  if (!value || value.enabled !== true) return undefined;
  const mode = value.mode ?? "bidi";
  const realtime = value.realtime ?? (mode === "agent" ? { provider: "openai" } : undefined);
  const transcription = value.transcription ?? {};
  const agentContext = realtime?.agentContext;
  const recentContext = realtime?.recentContext;
  const consultation = value.consultation ?? {};
  if (!["bidi", "agent"].includes(mode) || !object(transcription)
    || Object.keys(transcription).some(key => !["provider", "model", "language", "apiKey"].includes(key))
    || [transcription.provider, transcription.model, transcription.language].some(item => item !== undefined && !identifier(item))
    || (mode === "agent" && value.allowConsultSessionPersistence !== true)
    || !object(realtime) || !identifier(realtime.provider ?? (mode === "agent" ? "openai" : undefined)) || !identifier(value.agentId)
    || ![undefined, "disabled", "allowlist"].includes(value.inboundPolicy)
    || ![undefined, "disabled", "allowlist"].includes(value.outboundPolicy)
    || ![undefined, "none", "safe-read-only", "owner"].includes(value.toolPolicy)
    || [realtime.model, realtime.voice].some(item => item !== undefined && !identifier(item))
    || (consultation.inboundSessionKey !== undefined && !isCallSessionKey(consultation.inboundSessionKey))
    || (consultation.summary !== undefined && typeof consultation.summary !== "boolean")
    || (consultation.recentMessages !== undefined && (!Number.isInteger(consultation.recentMessages) || consultation.recentMessages < 0 || consultation.recentMessages > 50))
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
  const toolPolicy = value.toolPolicy ?? (value.allowConsultSessionPersistence === true ? "safe-read-only" : "none");
  return bindCallVoiceSecret({ enabled: true, mode, transcription: { provider: transcription.provider ?? "openai", model: transcription.model, language: transcription.language }, inboundPolicy: value.inboundPolicy ?? "disabled", inboundAllowFrom: names(value.inboundAllowFrom),
    outboundPolicy: value.outboundPolicy ?? "disabled", outboundAllowTo: names(value.outboundAllowTo), outboundToolAllowFrom: names(value.outboundToolAllowFrom),
    agentId: value.agentId, realtime: { provider: realtime.provider ?? "openai", model: realtime.model, voice: realtime.voice, instructions: realtime.instructions,
      agentContext: { enabled: agentContext?.enabled === true },
      recentContext: { enabled: recentContext?.enabled === true, maxMessages: recentContext?.maxMessages ?? 10 }, consultPolicy: realtime.consultPolicy ?? "auto",
      consultThinkingLevel: realtime.consultThinkingLevel, consultFastMode: realtime.consultFastMode },
    toolPolicy, conferences: { groups: conferenceGroups(value.conferences, toolPolicy) },
    allowConsultSessionPersistence: value.allowConsultSessionPersistence === true,
    consultation: { timeoutMs, inboundSessionKey: consultation.inboundSessionKey, summary: consultation.summary ?? true,
      recentMessages: consultation.recentMessages ?? 10 },
    sessionScope: value.sessionScope ?? "per-contact", maxDurationSeconds }, realtime.providers?.openai?.apiKey, transcription.apiKey);
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
  description: "Independent voice and video permissions. Voice includes direct calls and separately configured conferences; both default off.",
  properties: {
    voice: { type: "object", additionalProperties: false, properties: {
      enabled: { type: "boolean", default: false, description: "Enable voice calling. Conferences also require an exact configured group. Disabled voice calls are ignored without answering or declining." },
      mode: { type: "string", enum: ["bidi", "agent"], default: "bidi", description: "bidi uses a realtime voice model (including Live); agent transcribes speech, runs the configured OpenClaw agent and speaks through OpenClaw TTS. Applies to direct calls and conferences." },
      transcription: { type: "object", additionalProperties: false, properties: {
        provider: { ...nameSchema, default: "openai", description: "Registered realtime transcription provider for agent mode." },
        model: nameSchema, language: nameSchema,
        apiKey: { description: "Optional OpenClaw SecretRef or API-key string for the transcription provider. Omitted uses provider configuration." },
      } },
      inboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, inboundAllowFrom: allowlistSchema,
      outboundPolicy: { type: "string", enum: ["disabled", "allowlist"] }, outboundAllowTo: allowlistSchema, outboundToolAllowFrom: allowlistSchema,
      conferences: { type: "object", additionalProperties: false, properties: {
        groups: { type: "object", maxProperties: 128, propertyNames: { type: "string", pattern: "^.+@chatroom$", maxLength: 200 }, additionalProperties: {
          type: "object", additionalProperties: false, properties: {
            autoJoin: { type: "boolean", default: false, description: "Join current authenticated invitations for this exact group after voice preflight. No configured groups means no conference automation." },
            autoJoinAllowFrom: { ...allowlistSchema, description: "Optional exact conference inviter IDs. When present, only invitations from these contacts can auto-join; an empty list disables auto-join. The configured group is still required." },
            toolPolicy: { type: "string", enum: ["none", "safe-read-only", "owner"], default: "safe-read-only", description: "Conference consultation authority. Owner requires voice.toolPolicy=owner and an exact agent-wechat:GROUP_ID grant in commands.ownerAllowFrom. It trusts the whole group without attributing speech to a person; otherwise consultation stays read-only." },
            ownerInviterToolPolicy: { type: "string", enum: ["none", "safe-read-only", "owner"], description: "Optional policy for calls whose authenticated inviter exactly matches an agent-wechat:CONTACT_ID owner grant. Otherwise toolPolicy applies. Owner also requires voice.toolPolicy=owner and an exact group owner grant. Applies to the whole call, including later participants; permissions can decrease but cannot increase during a call." },
            sessionKey: { type: "string", pattern: CALL_SESSION_KEY_PATTERN, maxLength: MAX_CALL_SESSION_KEY_LENGTH, description: "Optional consultation parent session of this voice agent for this group. Default sessions are isolated by account, group and authority." },
          },
        } },
      } },
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
        summary: { type: "boolean", default: true, description: "After an answered call, give the call's transcript to the agent in the call's session and keep its short summary. The summary is also left as a note in your WeChat chat with the caller, and calls placed with the wechat_call tool report it to the session that placed them." },
        recentMessages: { type: "integer", minimum: 0, maximum: 50, default: 10, description: "How many of the latest messages of your WeChat chat with the caller the call's first consultation gets as context; 0 turns this off." },
      } },
      sessionScope: { type: "string", enum: callSessionScopes, default: "per-contact", description: "per-contact keeps one session for each contact, so the agent remembers earlier calls with them; per-call starts every call in a new session." },
      maxDurationSeconds: { type: "integer", minimum: 30, maximum: 3600, default: 3600 },
    } },
    video: { type: "object", additionalProperties: false, properties: {
      enabled: { type: "boolean", default: false, description: "Allow video independently of voice. Current video capability is unavailable; permission alone never answers a video call or routes it as voice." },
    } },
  },
};
