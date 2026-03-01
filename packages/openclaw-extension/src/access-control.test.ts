import test from "node:test";
import assert from "node:assert/strict";
import type { OpenClawConfig } from "openclaw/plugin-sdk";
import type { ResolvedWeChatAccount } from "./types.ts";
import {
  normalizeWeChatCommandBody,
  normalizeWeChatAllowFrom,
  normalizeWeChatId,
  resolveWeChatCommandAuthorization,
  resolveWeChatInboundAccessDecision,
  resolveWeChatMentionGate,
  resolveWeChatPolicyContext,
} from "./access-control.ts";

function baseAccount(overrides: Partial<ResolvedWeChatAccount> = {}): ResolvedWeChatAccount {
  return {
    accountId: "default",
    enabled: true,
    serverUrl: "http://localhost:6174",
    token: undefined,
    dmPolicy: "open",
    allowFrom: [],
    groupPolicy: "open",
    groupAllowFrom: [],
    groups: {},
    pollIntervalMs: 1000,
    authPollIntervalMs: 30000,
    ...overrides,
  };
}

test("normalizeWeChatId strips prefix and whitespace", () => {
  assert.equal(normalizeWeChatId("  wechat:wxid_123  "), "wxid_123");
  assert.equal(normalizeWeChatId("room@chatroom"), "room@chatroom");
  assert.equal(normalizeWeChatId(""), "");
});

test("normalizeWeChatAllowFrom dedupes and preserves wildcard", () => {
  const entries = normalizeWeChatAllowFrom([
    " wechat:wxid_abc ",
    "wxid_abc",
    "*",
    "wechat:wxid_xyz",
  ]);
  assert.deepEqual(entries, ["wxid_abc", "*", "wxid_xyz"]);
});

test("normalizeWeChatCommandBody strips leading mentions before slash commands in groups", () => {
  assert.equal(
    normalizeWeChatCommandBody("@agent\u2005/compact", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/compact",
  );
  assert.equal(
    normalizeWeChatCommandBody("@agent\u2005/compact focus", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/compact focus",
  );
  assert.equal(
    normalizeWeChatCommandBody("@agent hello /compact", {
      isGroup: true,
      wasMentioned: true,
    }),
    "@agent hello /compact",
  );
  assert.equal(
    normalizeWeChatCommandBody("@agent /compact", {
      isGroup: true,
      wasMentioned: false,
    }),
    "@agent /compact",
  );
  // Multi-word agent name with hair space separator
  assert.equal(
    normalizeWeChatCommandBody("@Agent Name\u2005/compact", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/compact",
  );
  // Multi-word name with command args
  assert.equal(
    normalizeWeChatCommandBody("@Agent Name\u2005/compact focus", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/compact focus",
  );
  // Multiple multi-word mentions
  assert.equal(
    normalizeWeChatCommandBody("@Agent Name\u2005@Other Person\u2005/status", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/status",
  );
  // No hair space — no stripping (not a real WeChat mention)
  assert.equal(
    normalizeWeChatCommandBody("@Agent Name /compact", {
      isGroup: true,
      wasMentioned: true,
    }),
    "@Agent Name /compact",
  );
  // Full-width @ with multi-word name
  assert.equal(
    normalizeWeChatCommandBody("\uFF20Agent Name\u2005/compact", {
      isGroup: true,
      wasMentioned: true,
    }),
    "/compact",
  );
});

test("resolveWeChatPolicyContext resolves overrides and effective allowlists", () => {
  const cfg: OpenClawConfig = {
    channels: {
      wechat: {},
      defaults: { groupPolicy: "allowlist" },
    },
  } as OpenClawConfig;
  const account = baseAccount({
    dmPolicy: "allowlist",
    allowFrom: ["wechat:wxid_dm"],
    groupPolicy: "open",
    groupAllowFrom: ["wxid_group"],
    groups: {
      "room@chatroom": {
        enabled: true,
        requireMention: false,
        groupPolicy: "allowlist",
        allowFrom: ["wechat:wxid_room"],
      },
    },
  });
  const policy = resolveWeChatPolicyContext({
    account,
    cfg,
    chatId: "wechat:room@chatroom",
    storeAllowFrom: ["wxid_store"],
  });
  assert.equal(policy.dmPolicy, "allowlist");
  assert.equal(policy.groupPolicy, "allowlist");
  assert.equal(policy.requireMention, false);
  // dmPolicy=allowlist ignores pairing-store allowFrom
  assert.deepEqual(policy.effectiveAllowFrom, ["wxid_dm"]);
  // group-level allowFrom override wins
  assert.deepEqual(policy.effectiveGroupAllowFrom, ["wxid_room"]);
});

test("resolveWeChatInboundAccessDecision enforces DM/group allowlists", () => {
  const blockedDm = resolveWeChatInboundAccessDecision({
    isGroup: false,
    senderId: "wxid_stranger",
    policy: {
      dmPolicy: "allowlist",
      groupPolicy: "open",
      requireMention: true,
      groupEnabled: true,
      effectiveAllowFrom: ["wxid_owner"],
      effectiveGroupAllowFrom: [],
    },
  });
  assert.equal(blockedDm.allowed, false);

  const allowedGroup = resolveWeChatInboundAccessDecision({
    isGroup: true,
    senderId: "wxid_member",
    policy: {
      dmPolicy: "open",
      groupPolicy: "allowlist",
      requireMention: true,
      groupEnabled: true,
      effectiveAllowFrom: [],
      effectiveGroupAllowFrom: ["*", "wxid_member"],
    },
  });
  assert.equal(allowedGroup.allowed, true);
});

test("resolveWeChatMentionGate bypasses mention only for authorized commands", () => {
  const bypass = resolveWeChatMentionGate({
    isGroup: true,
    requireMention: true,
    canDetectMention: true,
    wasMentioned: false,
    allowTextCommands: true,
    hasControlCommand: true,
    commandAuthorized: true,
  });
  assert.equal(bypass.shouldBypassMention, true);
  assert.equal(bypass.shouldSkip, false);

  const blocked = resolveWeChatMentionGate({
    isGroup: true,
    requireMention: true,
    canDetectMention: true,
    wasMentioned: false,
    allowTextCommands: true,
    hasControlCommand: true,
    commandAuthorized: false,
  });
  assert.equal(blocked.shouldBypassMention, false);
  assert.equal(blocked.shouldSkip, true);
});

test("resolveWeChatCommandAuthorization computes only for command-like bodies", async () => {
  const cfg: OpenClawConfig = { commands: { useAccessGroups: true } } as OpenClawConfig;
  const deps = {
    shouldComputeCommandAuthorized: (rawBody: string) => rawBody.trim().startsWith("/"),
    resolveCommandAuthorizedFromAuthorizers: (params: {
      useAccessGroups: boolean;
      authorizers: Array<{ configured: boolean; allowed: boolean }>;
    }) => {
      if (!params.useAccessGroups) {
        return true;
      }
      return params.authorizers.some((entry) => entry.configured && entry.allowed);
    },
    readAllowFromStore: async () => ["wxid_store"],
  };

  const authorized = await resolveWeChatCommandAuthorization({
    cfg,
    rawBody: "/status",
    isGroup: false,
    senderId: "wechat:wxid_store",
    dmPolicy: "open",
    allowFromForCommands: [],
    deps,
  });
  assert.equal(authorized, true);

  const skipped = await resolveWeChatCommandAuthorization({
    cfg,
    rawBody: "hello",
    isGroup: false,
    senderId: "wechat:wxid_store",
    dmPolicy: "open",
    allowFromForCommands: [],
    deps,
  });
  assert.equal(skipped, undefined);
});
