/**
 * Calls configuration (DESIGN.md §6.1). Ported from the OpenClaw adapter's
 * `calls-config.ts` semantics, minus its SecretRef binding: API keys resolve
 * through the harness credential resolver instead. Validation fails fast —
 * an invalid calls configuration parks the whole channel (fail closed).
 */

export type CallMode = 'bidi' | 'agent';
export type CallToolPolicy = 'none' | 'safe-read-only' | 'owner';
export type CallSessionScope = 'per-contact' | 'per-call';
export type ConsultPolicy = 'auto' | 'substantive' | 'always';
export type FastContextSource = 'memory' | 'sessions';

export type WeChatConferenceGroupConfig = {
  autoJoin?: boolean;
  autoJoinAllowFrom?: string[];
  toolPolicy?: CallToolPolicy;
  ownerInviterToolPolicy?: CallToolPolicy;
  sessionKey?: string;
};

export type WeChatVoiceCallsConfig = {
  enabled?: boolean;
  mode?: CallMode;
  transcription?: { provider?: string; model?: string; language?: string };
  inboundPolicy?: 'disabled' | 'allowlist';
  inboundAllowFrom?: string[];
  outboundPolicy?: 'disabled' | 'allowlist';
  outboundAllowTo?: string[];
  outboundToolAllowFrom?: string[];
  conferences?: { groups?: Record<string, WeChatConferenceGroupConfig> };
  agentId?: string;
  realtime?: {
    provider?: string;
    model?: string;
    voice?: string;
    instructions?: string;
    /** Harness credential ref (POSIX identifier) holding the provider API key. */
    apiKeyCredentialRef?: string;
    agentContext?: { enabled?: boolean };
    recentContext?: { enabled?: boolean; maxMessages?: number };
    consultPolicy?: ConsultPolicy;
    /** v1 note: thinking levels/fast mode are accepted but unused by the runner. */
    consultThinkingLevel?: string;
    consultFastMode?: boolean;
    fastContext?: {
      enabled?: boolean;
      timeoutMs?: number;
      maxResults?: number;
      sources?: FastContextSource[];
      fallbackToConsult?: boolean;
    };
  };
  toolPolicy?: CallToolPolicy;
  allowConsultSessionPersistence?: boolean;
  consultation?: {
    timeoutMs?: number;
    inboundSessionKey?: string;
    summary?: boolean;
    recentMessages?: number;
  };
  sessionScope?: CallSessionScope;
  maxDurationSeconds?: number;
};

export type WeChatCallsConfig = {
  voice?: WeChatVoiceCallsConfig;
  video?: { enabled?: boolean };
};

export type ResolvedCallFastContextConfig = {
  enabled: boolean;
  timeoutMs: number;
  maxResults: number;
  sources: FastContextSource[];
  fallbackToConsult: boolean;
};

export type ResolvedWeChatConferenceGroupConfig = {
  autoJoin: boolean;
  autoJoinAllowFrom?: string[];
  toolPolicy: CallToolPolicy;
  ownerInviterToolPolicy?: CallToolPolicy;
  sessionKey?: string;
};

export type ResolvedWeChatVoiceCallsConfig = {
  enabled: true;
  mode: CallMode;
  transcription: { provider: string; model?: string; language?: string };
  inboundPolicy: 'disabled' | 'allowlist';
  inboundAllowFrom: string[];
  outboundPolicy: 'disabled' | 'allowlist';
  outboundAllowTo: string[];
  outboundToolAllowFrom: string[];
  agentId: string;
  realtime: {
    provider: string;
    model?: string;
    voice?: string;
    instructions?: string;
    readonly apiKeyCredentialRef?: string;
    agentContext: { enabled: boolean };
    recentContext: { enabled: boolean; maxMessages: number };
    consultPolicy: ConsultPolicy;
    consultThinkingLevel?: string;
    consultFastMode?: boolean;
    fastContext: ResolvedCallFastContextConfig;
  };
  toolPolicy: CallToolPolicy;
  allowConsultSessionPersistence: boolean;
  consultation: { timeoutMs: number; inboundSessionKey?: string; summary: boolean; recentMessages: number };
  sessionScope: CallSessionScope;
  maxDurationSeconds: number;
  conferences: { groups: Record<string, ResolvedWeChatConferenceGroupConfig> };
};

export type ResolvedWeChatCallsConfig = {
  voice?: ResolvedWeChatVoiceCallsConfig;
  video: { enabled: boolean };
};

const consultPolicies = ['auto', 'substantive', 'always'] as const;
const consultThinkingLevels = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'adaptive', 'max', 'ultra'] as const;
const fastContextSources = ['memory', 'sessions'] as const;
const callSessionScopes = ['per-contact', 'per-call'] as const;

export const CALL_SESSION_KEY_PATTERN = '^agent:[a-z0-9][a-z0-9_-]{0,63}:[^:\\s][^\\s]*$';
export const MAX_CALL_SESSION_KEY_LENGTH = 1024;
const keyPattern = new RegExp(CALL_SESSION_KEY_PATTERN);

export function isCallSessionKey(value: unknown): value is string {
  return typeof value === 'string' && value.length <= MAX_CALL_SESSION_KEY_LENGTH && keyPattern.test(value)
    && !/[\u0000-\u001f\u007f]/.test(value);
}

/** Exact group IDs override the optional "*" rule; no rule means no automation. */
export function resolveConferenceGroupConfig(
  voice: ResolvedWeChatVoiceCallsConfig,
  groupId: string,
): ResolvedWeChatConferenceGroupConfig | undefined {
  if (groupId === '*' || !groupId.endsWith('@chatroom')) return undefined;
  return Object.hasOwn(voice.conferences.groups, groupId)
    ? voice.conferences.groups[groupId]
    : voice.conferences.groups['*'];
}

const identifier = (value: unknown): value is string =>
  typeof value === 'string' && value.length > 0 && value.length <= 200 && !/[\u0000-\u001f\u007f]/.test(value);
const object = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === 'object' && !Array.isArray(value);

function names(value: unknown, allowWildcard = false): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 128 || value.some(name => !identifier(name) || (!allowWildcard && name === '*'))) {
    throw new Error('Invalid calling allowlist');
  }
  return [...new Set(value)];
}

function conferenceGroups(
  value: WeChatVoiceCallsConfig['conferences'],
  toolPolicy: CallToolPolicy,
): Record<string, ResolvedWeChatConferenceGroupConfig> {
  if (value === undefined) return {};
  if (!object(value) || Object.keys(value).some(key => key !== 'groups') || (value.groups !== undefined && !object(value.groups))) {
    throw new Error('Invalid conference configuration');
  }
  const entries = Object.entries(value.groups ?? {});
  if (entries.length > 128) throw new Error('Too many conference groups');
  const groups: Record<string, ResolvedWeChatConferenceGroupConfig> = Object.create(null);
  for (const [id, group] of entries) {
    if (
      !identifier(id) || (id !== '*' && !id.endsWith('@chatroom')) || !object(group)
      || Object.keys(group).some(key => !['autoJoin', 'autoJoinAllowFrom', 'toolPolicy', 'ownerInviterToolPolicy', 'sessionKey'].includes(key))
      || (group.autoJoin !== undefined && typeof group.autoJoin !== 'boolean')
      || (id === '*' && group.autoJoin === true && group.autoJoinAllowFrom === undefined)
      || ![undefined, 'none', 'safe-read-only', 'owner'].includes(group.toolPolicy as CallToolPolicy | undefined)
      || ![undefined, 'none', 'safe-read-only', 'owner'].includes(group.ownerInviterToolPolicy as CallToolPolicy | undefined)
      || ((group.toolPolicy === 'owner' || group.ownerInviterToolPolicy === 'owner') && toolPolicy !== 'owner')
      || (group.sessionKey !== undefined && !isCallSessionKey(group.sessionKey))
    ) {
      throw new Error('Invalid conference group policy');
    }
    groups[id] = {
      autoJoin: group.autoJoin === true,
      ...(group.autoJoinAllowFrom !== undefined ? { autoJoinAllowFrom: names(group.autoJoinAllowFrom, true) } : {}),
      toolPolicy: toolPolicy === 'none' ? 'none' : (group.toolPolicy ?? 'safe-read-only'),
      ...(group.ownerInviterToolPolicy !== undefined ? { ownerInviterToolPolicy: group.ownerInviterToolPolicy as CallToolPolicy } : {}),
      ...(group.sessionKey !== undefined ? { sessionKey: group.sessionKey as string } : {}),
    };
  }
  return groups;
}

function fastContextConfig(value: unknown): ResolvedCallFastContextConfig {
  const fast = (value ?? {}) as Record<string, unknown>;
  if (
    !object(fast) || Object.keys(fast).some(key => !['enabled', 'timeoutMs', 'maxResults', 'sources', 'fallbackToConsult'].includes(key))
    || (fast.enabled !== undefined && typeof fast.enabled !== 'boolean')
    || (fast.fallbackToConsult !== undefined && typeof fast.fallbackToConsult !== 'boolean')
    || (fast.timeoutMs !== undefined && (!Number.isInteger(fast.timeoutMs) || (fast.timeoutMs as number) < 100 || (fast.timeoutMs as number) > 10000))
    || (fast.maxResults !== undefined && (!Number.isInteger(fast.maxResults) || (fast.maxResults as number) < 1 || (fast.maxResults as number) > 10))
    || (fast.sources !== undefined && (!Array.isArray(fast.sources) || fast.sources.length === 0
      || fast.sources.some(source => !fastContextSources.includes(source as FastContextSource)) || new Set(fast.sources).size !== fast.sources.length))
  ) {
    throw new Error('Invalid calling configuration');
  }
  return {
    enabled: fast.enabled === true,
    timeoutMs: (fast.timeoutMs as number | undefined) ?? 800,
    maxResults: (fast.maxResults as number | undefined) ?? 3,
    sources: (fast.sources as FastContextSource[] | undefined) ?? [...fastContextSources],
    fallbackToConsult: fast.fallbackToConsult === true,
  };
}

function voiceConfig(value: WeChatVoiceCallsConfig | undefined): ResolvedWeChatVoiceCallsConfig | undefined {
  if (!value || value.enabled !== true) return undefined;
  const mode = value.mode ?? 'bidi';
  const realtime = value.realtime ?? (mode === 'agent' ? { provider: 'openai' } : undefined);
  const transcription = value.transcription ?? {};
  const agentContext = realtime?.agentContext;
  const recentContext = realtime?.recentContext;
  const consultation = value.consultation ?? {};
  if (
    !['bidi', 'agent'].includes(mode) || !object(transcription)
    || Object.keys(transcription).some(key => !['provider', 'model', 'language'].includes(key))
    || [transcription.provider, transcription.model, transcription.language].some(item => item !== undefined && !identifier(item))
    || (mode === 'agent' && value.allowConsultSessionPersistence !== true)
    || !object(realtime) || !identifier(realtime.provider ?? (mode === 'agent' ? 'openai' : undefined)) || !identifier(value.agentId)
    || (realtime.apiKeyCredentialRef !== undefined && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(realtime.apiKeyCredentialRef))
    || ![undefined, 'disabled', 'allowlist'].includes(value.inboundPolicy)
    || ![undefined, 'disabled', 'allowlist'].includes(value.outboundPolicy)
    || ![undefined, 'none', 'safe-read-only', 'owner'].includes(value.toolPolicy)
    || [realtime.model, realtime.voice].some(item => item !== undefined && !identifier(item))
    || (consultation.inboundSessionKey !== undefined && !isCallSessionKey(consultation.inboundSessionKey))
    || (consultation.summary !== undefined && typeof consultation.summary !== 'boolean')
    || (consultation.recentMessages !== undefined && (!Number.isInteger(consultation.recentMessages) || consultation.recentMessages < 0 || consultation.recentMessages > 50))
    || (value.sessionScope !== undefined && !callSessionScopes.includes(value.sessionScope))
    || (recentContext !== undefined && (!object(recentContext) || Object.keys(recentContext).some(key => !['enabled', 'maxMessages'].includes(key))
      || (recentContext.enabled !== undefined && typeof recentContext.enabled !== 'boolean')
      || (recentContext.maxMessages !== undefined && (!Number.isInteger(recentContext.maxMessages) || recentContext.maxMessages < 1 || recentContext.maxMessages > 50))))
    || (realtime.consultPolicy !== undefined && !consultPolicies.includes(realtime.consultPolicy))
    || (realtime.consultThinkingLevel !== undefined && !consultThinkingLevels.includes(realtime.consultThinkingLevel as (typeof consultThinkingLevels)[number]))
    || (realtime.consultFastMode !== undefined && typeof realtime.consultFastMode !== 'boolean')
    || (realtime.instructions !== undefined && (typeof realtime.instructions !== 'string' || realtime.instructions.length > 4000 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(realtime.instructions)))
    || (agentContext !== undefined && (!object(agentContext) || Object.keys(agentContext).some(key => key !== 'enabled')
      || (agentContext.enabled !== undefined && typeof agentContext.enabled !== 'boolean')))
  ) {
    throw new Error('Invalid calling configuration');
  }
  const maxDurationSeconds = value.maxDurationSeconds ?? 3600;
  const timeoutMs = consultation.timeoutMs ?? 30000;
  if (!Number.isInteger(maxDurationSeconds) || maxDurationSeconds < 30 || maxDurationSeconds > 3600) throw new Error('Invalid calling duration');
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 120000) throw new Error('Invalid calling consultation timeout');
  if ((value.toolPolicy === 'safe-read-only' || value.toolPolicy === 'owner') && value.allowConsultSessionPersistence !== true) {
    throw new Error('Calling consultation requires opting in to host session persistence');
  }
  const toolPolicy = value.toolPolicy ?? (value.allowConsultSessionPersistence === true ? 'safe-read-only' : 'none');
  return {
    enabled: true,
    mode,
    transcription: {
      provider: transcription.provider ?? 'openai',
      ...(transcription.model !== undefined ? { model: transcription.model } : {}),
      ...(transcription.language !== undefined ? { language: transcription.language } : {}),
    },
    inboundPolicy: value.inboundPolicy ?? 'disabled',
    inboundAllowFrom: names(value.inboundAllowFrom, true),
    outboundPolicy: value.outboundPolicy ?? 'disabled',
    outboundAllowTo: names(value.outboundAllowTo),
    outboundToolAllowFrom: names(value.outboundToolAllowFrom),
    agentId: value.agentId,
    realtime: {
      provider: realtime.provider ?? 'openai',
      ...(realtime.model !== undefined ? { model: realtime.model } : {}),
      ...(realtime.voice !== undefined ? { voice: realtime.voice } : {}),
      ...(realtime.instructions !== undefined ? { instructions: realtime.instructions } : {}),
      ...(realtime.apiKeyCredentialRef !== undefined ? { apiKeyCredentialRef: realtime.apiKeyCredentialRef } : {}),
      agentContext: { enabled: agentContext?.enabled === true },
      recentContext: { enabled: recentContext?.enabled === true, maxMessages: recentContext?.maxMessages ?? 10 },
      consultPolicy: realtime.consultPolicy ?? 'auto',
      ...(realtime.consultThinkingLevel !== undefined ? { consultThinkingLevel: realtime.consultThinkingLevel } : {}),
      ...(realtime.consultFastMode !== undefined ? { consultFastMode: realtime.consultFastMode } : {}),
      fastContext: fastContextConfig(realtime.fastContext),
    },
    toolPolicy,
    conferences: { groups: conferenceGroups(value.conferences, toolPolicy) },
    allowConsultSessionPersistence: value.allowConsultSessionPersistence === true,
    consultation: {
      timeoutMs,
      ...(consultation.inboundSessionKey !== undefined ? { inboundSessionKey: consultation.inboundSessionKey } : {}),
      summary: consultation.summary ?? true,
      recentMessages: consultation.recentMessages ?? 10,
    },
    sessionScope: value.sessionScope ?? 'per-contact',
    maxDurationSeconds,
  };
}

/** Resolve and validate the calls config. Returns undefined when calls stay
 * off; throws (fail fast) on any malformed value. */
export function resolveWeChatCallsConfig(value: unknown): ResolvedWeChatCallsConfig | undefined {
  if (value === undefined) return undefined;
  const input = value as WeChatCallsConfig;
  if (
    !object(input) || Object.keys(input).some(key => !['voice', 'video'].includes(key))
    || [input.voice, input.video].some(item => item !== undefined && (!object(item) || (item.enabled !== undefined && typeof item.enabled !== 'boolean')))
  ) {
    throw new Error('Invalid calling configuration; use calls.voice.enabled and calls.video.enabled');
  }
  const voice = voiceConfig(input.voice);
  const video = { enabled: input.video?.enabled === true };
  return voice || video.enabled ? { ...(voice !== undefined ? { voice } : {}), video } : undefined;
}
