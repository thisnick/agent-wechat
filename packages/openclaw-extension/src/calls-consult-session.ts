import { randomUUID } from "node:crypto";
import type { OpenClawConfig, PluginRuntime } from "openclaw/plugin-sdk/core";
import { beginCallConsultAuthority } from "./calls-consult-authority.ts";
import { buildCallConsultContext } from "./calls-consult-context.ts";

type CallConsultation = {
  cfg: OpenClawConfig;
  reply: PluginRuntime["channel"]["reply"];
  sessionKey: string;
  parentKey?: string;
  senderId: string;
  accountId: string;
  /** Trusted configuration grant for the actual group principal, never a human sender alias. */
  conference?: { groupId: string; owner: boolean };
  runtime?: PluginRuntime;
  question: string;
  instructions: string;
  /** Heads the question in the agent's prompt (default "Caller question"). */
  inputLabel?: string;
  /** Restricts the run to these tools; omitted keeps the caller's normal tool policy. */
  toolsAllow?: string[];
  thinking?: string;
  fastMode?: boolean;
  signal: AbortSignal;
  assertCurrent(): void;
  onRunStarted(): void;
};

/** Run a call consultation through the agent's normal session, without delivering a chat reply.
 * This uses the agent's configured model routing, including CLI runtimes and fallbacks. */
export async function consultCallSession(params: CallConsultation): Promise<{ text: string }> {
  const { reply, cfg, senderId, sessionKey, parentKey, accountId, signal, question } = params;
  if (params.conference && (senderId !== params.conference.groupId
    || (params.conference.owner && !cfg.commands?.ownerAllowFrom?.includes(`agent-wechat:${params.conference.groupId}`)))) throw Error("Conference consultation principal is not authorized");
  params.assertCurrent();
  signal.throwIfAborted();
  const runId = `${params.conference ? "wechat-conference" : "wechat-call"}-${randomUUID()}`;
  const body = `${params.instructions}\n\n${params.inputLabel ?? (params.conference ? "Conference question" : "Caller question")}:\n${question}`;
  const authority = new AbortController();
  const runSignal = AbortSignal.any([signal, authority.signal]);
  const ctx = params.runtime?.channel?.inbound?.buildContext ? await buildCallConsultContext(params.runtime, cfg, {
    senderId, accountId, sessionKey, parentKey, runId, question, body, conference: params.conference,
  }) : reply.finalizeInboundContext({
    Body: question, BodyForAgent: body, RawBody: question,
    SessionKey: sessionKey, ...(parentKey ? { ParentSessionKey: parentKey } : {}),
    AccountId: accountId, From: params.conference ? `wechat:group:${params.conference.groupId}` : `wechat:${senderId}`, SenderId: senderId,
    ChatType: params.conference ? "group" : "direct", Provider: "agent-wechat", Surface: "agent-wechat",
    ...(params.conference ? { To: params.conference.groupId, GroupSubject: params.conference.groupId } : {}),
    MessageSid: runId, CommandAuthorized: false, CommandInterpretationSuppressed: true,
    SuppressMessageReceivedHooks: true,
  });
  let text = "";
  let failed = false;
  let completed = false;
  // The host resolves its configured owner grant for the actual group principal.
  // This adapter never alters that grant, model routing, tool policy or approvals.
  const release = params.conference ? beginCallConsultAuthority(params.runtime!, runId, sessionKey, params.assertCurrent, reason => authority.abort(reason)) : () => {};
  try {
    await reply.dispatchReplyWithBufferedBlockDispatcher({
      ctx, cfg,
      dispatcherOptions: {
        deliver: async (payload, info) => {
          params.assertCurrent();
          runSignal.throwIfAborted();
          if (payload.isError) failed = true;
          else if (info.kind === "final" && payload.text?.trim()) text = [text, payload.text.trim()].filter(Boolean).join("\n").slice(0, 8000);
        },
        onError: () => { failed = true; },
      },
      replyOptions: {
        runId, abortSignal: runSignal, suppressTyping: true, disableBlockStreaming: true,
        sourceReplyDeliveryMode: "automatic",
        suppressDefaultToolProgressMessages: true, suppressToolProgressMessages: true,
        thinkingLevelOverride: params.thinking, fastModeOverride: params.fastMode,
        ...(params.toolsAllow ? { toolsAllow: [...params.toolsAllow] } : {}),
        onAgentRunStart: () => {
          try { params.assertCurrent(); }
          catch (error) { authority.abort(error); throw error; }
          params.onRunStarted();
        },
        onAgentRunTerminalOutcome: outcome => {
          if (outcome === "completed") completed = true;
          else failed = true;
        },
      },
    }).catch(error => {
      authority.abort(error);
      throw error;
    });
    runSignal.throwIfAborted();
    if (!completed || failed || !text) {
      const error = Error("Calling consultation returned no answer");
      authority.abort(error);
      throw error;
    }
    params.assertCurrent();
    return { text };
  } finally { release(); if (params.conference) authority.abort(); }
}
