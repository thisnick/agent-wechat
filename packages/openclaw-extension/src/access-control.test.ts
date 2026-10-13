import test from "node:test";
import assert from "node:assert/strict";
import type { OpenClawConfig } from "openclaw/plugin-sdk/core";
import type { ResolvedWeChatAccount } from "./types.ts";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import {
  normalizeWeChatCommandBody,
  normalizeWeChatAllowFrom,
  normalizeWeChatId,
  resolveWeChatIngress,
  resolveWeChatPolicyContext,
  type WeChatPolicyContext,
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
    mediaMaxMb: 50,
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

test("resolveWeChatPolicyContext resolves per-group overrides", () => {
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
  const policy = resolveWeChatPolicyContext({ account, cfg, chatId: "wechat:room@chatroom" });
  assert.equal(policy.dmPolicy, "allowlist");
  assert.equal(policy.groupPolicy, "allowlist");
  assert.equal(policy.requireMention, false);
  assert.equal(policy.groupEnabled, true);
  assert.deepEqual(policy.allowFrom, ["wxid_dm"]);
  assert.deepEqual(policy.groupAllowFrom, ["wxid_group"]);
  assert.deepEqual(policy.groupSenderAllowFrom, ["wxid_room"]);

  const other = resolveWeChatPolicyContext({ account, cfg, chatId: "other@chatroom" });
  assert.equal(other.groupPolicy, "open");
  assert.equal(other.requireMention, true);
  assert.equal(other.groupSenderAllowFrom, undefined);
});

// The tests below run OpenClaw's real ingress policy (the SDK helper the runtime method wraps).
function policyOf(overrides: Partial<WeChatPolicyContext> = {}): WeChatPolicyContext {
  return {
    dmPolicy: "open",
    groupPolicy: "open",
    requireMention: true,
    groupEnabled: true,
    allowFrom: ["*"],
    groupAllowFrom: ["*"],
    groupAllowFromFallbackToAllowFrom: true,
    ...overrides,
  };
}

const CFG = {} as OpenClawConfig;
const ROOM = "room@chatroom";

function ingress(params: {
  policy?: Partial<WeChatPolicyContext>;
  isGroup?: boolean;
  senderId: string;
  cfg?: OpenClawConfig;
  store?: string[];
  command?: { allowTextCommands: boolean; hasControlCommand: boolean };
  wasMentioned?: boolean;
}) {
  const isGroup = params.isGroup === true;
  return resolveWeChatIngress({
    resolve: resolveStableChannelMessageIngress,
    cfg: params.cfg ?? CFG,
    accountId: "default",
    policy: policyOf(params.policy),
    isGroup,
    chatId: isGroup ? ROOM : params.senderId,
    senderId: params.senderId,
    readStoreAllowFrom: params.store ? async () => params.store ?? [] : undefined,
    command: params.command,
    mention:
      params.wasMentioned === undefined ? undefined : { wasMentioned: params.wasMentioned, allowTextCommands: true },
  });
}

const admission = async (params: Parameters<typeof ingress>[0]) => {
  const result = await ingress(params);
  return `${result.ingress.admission}:${result.ingress.reasonCode}`;
};

test("ingress: direct messages follow dmPolicy and allowFrom", async () => {
  assert.match(await admission({ senderId: "wxid_any" }), /^dispatch:/);
  const allowlist = { dmPolicy: "allowlist" as const, allowFrom: ["wxid_owner"] };
  assert.match(await admission({ policy: allowlist, senderId: "wechat:wxid_owner" }), /^dispatch:/);
  assert.equal(await admission({ policy: allowlist, senderId: "wxid_stranger" }), "drop:dm_policy_not_allowlisted");
  assert.equal(await admission({ policy: { dmPolicy: "disabled" }, senderId: "wxid_any" }), "drop:dm_policy_disabled");
  // Core requires "*" for open DMs; an empty list admits nobody.
  assert.equal(await admission({ policy: { allowFrom: [] }, senderId: "wxid_any" }), "drop:dm_policy_not_allowlisted");
});

test("resolveWeChatPolicyContext keeps dmPolicy open meaning anyone", async () => {
  const cfg = {} as OpenClawConfig;
  const implied = resolveWeChatPolicyContext({
    account: baseAccount({ dmPolicy: "open", allowFrom: ["wxid_owner"] }),
    cfg,
    chatId: "wxid_any",
  });
  assert.equal(implied.impliedOpenDmWildcard, true);
  assert.deepEqual(implied.allowFrom, ["*", "wxid_owner"]);
  assert.match(await admission({ policy: implied, senderId: "wxid_any" }), /^dispatch:/);

  const explicit = resolveWeChatPolicyContext({
    account: baseAccount({ dmPolicy: "open", allowFrom: ["*"] }),
    cfg,
    chatId: "wxid_any",
  });
  assert.equal(explicit.impliedOpenDmWildcard, undefined);
  assert.deepEqual(explicit.allowFrom, ["*"]);

  const allowlist = resolveWeChatPolicyContext({
    account: baseAccount({ dmPolicy: "allowlist", allowFrom: ["wxid_owner"] }),
    cfg,
    chatId: "wxid_any",
  });
  assert.deepEqual(allowlist.allowFrom, ["wxid_owner"]);
});

test("ingress: groups follow groupPolicy, the allowFrom fallback, and per-group overrides", async () => {
  assert.match(await admission({ isGroup: true, senderId: "wxid_member" }), /^dispatch:/);
  const allowlist = { groupPolicy: "allowlist" as const, groupAllowFrom: [], allowFrom: ["wxid_owner"] };
  assert.match(await admission({ policy: allowlist, isGroup: true, senderId: "wxid_owner" }), /^dispatch:/);
  assert.equal(
    await admission({ policy: allowlist, isGroup: true, senderId: "wxid_member" }),
    "drop:group_policy_not_allowlisted",
  );
  const override = { ...allowlist, groupSenderAllowFrom: ["wxid_member"] };
  assert.match(await admission({ policy: override, isGroup: true, senderId: "wxid_member" }), /^dispatch:/);
  assert.equal(
    await admission({ policy: override, isGroup: true, senderId: "wxid_owner" }),
    "drop:group_policy_not_allowlisted",
  );
  assert.equal(
    await admission({ policy: { groupEnabled: false }, isGroup: true, senderId: "wxid_member" }),
    "drop:route_blocked",
  );
  assert.equal(
    await admission({ policy: { groupPolicy: "disabled" }, isGroup: true, senderId: "wxid_member" }),
    "drop:group_policy_disabled",
  );
});

test("ingress: the pairing store never admits group senders", async () => {
  assert.equal(
    await admission({
      policy: { groupPolicy: "allowlist", groupAllowFrom: ["wxid_owner"], allowFrom: [] },
      isGroup: true,
      senderId: "wxid_paired",
      store: ["wxid_paired"],
    }),
    "drop:group_policy_not_allowlisted",
  );
});

test("ingress: control commands need an owner or group allowlist entry", async () => {
  const cmd = { allowTextCommands: true, hasControlCommand: true };
  const owners = { allowFrom: ["wxid_owner"], groupAllowFrom: ["wxid_owner"] };
  const allowed = await ingress({ policy: owners, isGroup: true, senderId: "wxid_owner", command: cmd });
  assert.equal(allowed.commandAccess.authorized, true);
  assert.equal(allowed.ingress.admission, "dispatch");

  const blocked = await ingress({
    policy: { ...owners, groupPolicy: "open", groupAllowFrom: ["wxid_owner"] },
    isGroup: true,
    senderId: "wxid_member",
    command: cmd,
  });
  assert.equal(blocked.commandAccess.authorized, false);
  assert.equal(`${blocked.ingress.admission}:${blocked.ingress.reasonCode}`, "drop:control_command_unauthorized");

  // With the wildcard config every sender may run commands, as before.
  const open = await ingress({ isGroup: true, senderId: "wxid_member", command: cmd });
  assert.equal(open.commandAccess.authorized, true);
});

test("ingress: mention gate skips unmentioned group messages unless an authorized command bypasses it", async () => {
  assert.equal(
    await admission({ isGroup: true, senderId: "wxid_member", wasMentioned: false }),
    "skip:activation_skipped",
  );
  const mentioned = await ingress({ isGroup: true, senderId: "wxid_member", wasMentioned: true });
  assert.equal(mentioned.ingress.admission, "dispatch");
  assert.equal(mentioned.activationAccess.effectiveWasMentioned, true);

  assert.match(
    await admission({ policy: { requireMention: false }, isGroup: true, senderId: "wxid_member", wasMentioned: false }),
    /^dispatch:/,
  );

  const cmd = { allowTextCommands: true, hasControlCommand: true };
  const bypass = await ingress({ isGroup: true, senderId: "wxid_member", wasMentioned: false, command: cmd });
  assert.equal(bypass.ingress.admission, "dispatch");
  assert.equal(bypass.activationAccess.effectiveWasMentioned, true);

  // DMs are never mention-gated.
  const dm = await ingress({ senderId: "wxid_any", wasMentioned: false });
  assert.equal(dm.activationAccess.shouldSkip, false);
  assert.equal(dm.ingress.admission, "dispatch");
});

test("ingress: accessGroup entries resolve through core", async () => {
  const cfg = {
    accessGroups: { family: { type: "message.senders", members: { "agent-wechat": ["wxid_mom"] } } },
  } as unknown as OpenClawConfig;
  const policy = { dmPolicy: "allowlist" as const, allowFrom: ["accessGroup:family"] };
  assert.match(await admission({ cfg, policy, senderId: "wxid_mom" }), /^dispatch:/);
  assert.equal(await admission({ cfg, policy, senderId: "wxid_other" }), "drop:dm_policy_not_allowlisted");
});

test("ingress: group commands use the group's own allowlist, not DM owners", async () => {
  const cmd = { allowTextCommands: true, hasControlCommand: true };
  // A per-group allowFrom authorizes its senders' commands too.
  const override = await ingress({
    policy: { groupPolicy: "allowlist", groupAllowFrom: ["wxid_member"], groupSenderAllowFrom: ["wxid_room"] },
    isGroup: true,
    senderId: "wxid_room",
    command: cmd,
  });
  assert.equal(override.ingress.admission, "dispatch");
  assert.equal(override.commandAccess.authorized, true);

  // DM owners are not command owners in a group with its own allowlist.
  const owner = await ingress({
    policy: { dmPolicy: "allowlist", allowFrom: ["wxid_owner"], groupAllowFrom: ["wxid_member"] },
    isGroup: true,
    senderId: "wxid_owner",
    command: cmd,
  });
  assert.equal(owner.commandAccess.authorized, false);
});

test("ingress: implied open-DM wildcard does not widen group access or command owners", async () => {
  const policy = resolveWeChatPolicyContext({
    account: baseAccount({ dmPolicy: "open", allowFrom: ["wxid_owner"], groupPolicy: "allowlist", groupAllowFrom: [] }),
    cfg: {} as OpenClawConfig,
    chatId: ROOM,
  });
  const cmd = { allowTextCommands: true, hasControlCommand: true };
  assert.equal(
    await admission({ policy, isGroup: true, senderId: "wxid_member" }),
    "drop:group_policy_not_allowlisted",
  );
  assert.match(await admission({ policy, isGroup: true, senderId: "wxid_owner" }), /^dispatch:/);
  const dmStranger = await ingress({ policy, senderId: "wxid_stranger", command: cmd });
  assert.equal(dmStranger.commandAccess.authorized, false);
  const dmOwner = await ingress({ policy, senderId: "wxid_owner", command: cmd });
  assert.equal(dmOwner.commandAccess.authorized, true);
});
