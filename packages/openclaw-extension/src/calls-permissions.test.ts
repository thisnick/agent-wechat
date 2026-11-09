import test from "node:test";
import assert from "node:assert/strict";
import { resolveWeChatCallsConfig } from "./calls-config.ts";
import { conferenceCallPolicy } from "./calls-permissions.ts";

const groupId = "test@chatroom";
const voice = () => resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "main", toolPolicy: "owner", allowConsultSessionPersistence: true,
  realtime: { provider: "openai" }, conferences: { groups: { [groupId]: { toolPolicy: "safe-read-only", ownerInviterToolPolicy: "owner" } } } } })!.voice!;
test("owner invitations select the override only with exact human and group host grants", () => {
  const config = voice(), group = config.conferences.groups[groupId];
  const entries = ["agent-wechat:trusted", `agent-wechat:${groupId}`];
  const resolve = (inviterId?: string, owners: string[] = entries) => conferenceCallPolicy(config, group, { groupId, inviterId }, owners);
  assert.deepEqual(resolve("trusted"), { toolPolicy: "owner", reason: "owner_inviter" });
  for (const id of [undefined, "other", "trusted ", "*", groupId]) assert.deepEqual(resolve(id), { toolPolicy: "safe-read-only", reason: "group_default" });
  assert.equal(resolve("trusted", ["trusted", `agent-wechat:${groupId}`]).toolPolicy, "safe-read-only");
  assert.deepEqual(resolve("trusted", ["agent-wechat:trusted"]), { toolPolicy: "safe-read-only", reason: "owner_grant_missing" });
  assert.equal(conferenceCallPolicy(config, group, { groupId, inviterId: "trusted", policyAtAdmission: "safe-read-only" }, entries).toolPolicy, "safe-read-only");
  assert.equal(conferenceCallPolicy({ ...config, toolPolicy: "none" }, group, { groupId, inviterId: "trusted" }, entries).toolPolicy, "none");
  assert.equal(conferenceCallPolicy({ ...config, allowConsultSessionPersistence: false }, group, { groupId, inviterId: "trusted" }, entries).toolPolicy, "none");
});
test("omitted overrides retain group policy, and owner overrides can also reduce permissions", () => {
  const config = voice(), group = config.conferences.groups[groupId]; const entries = ["agent-wechat:trusted", `agent-wechat:${groupId}`];
  assert.equal(conferenceCallPolicy(config, { ...group, ownerInviterToolPolicy: undefined }, { groupId, inviterId: "trusted" }, entries).toolPolicy, "safe-read-only");
  assert.equal(conferenceCallPolicy(config, { ...group, ownerInviterToolPolicy: "none" }, { groupId, inviterId: "trusted" }, entries).toolPolicy, "none");
  assert.equal(conferenceCallPolicy(config, { ...group, toolPolicy: "owner", ownerInviterToolPolicy: undefined }, { groupId }, entries).toolPolicy, "owner");
  const base = { enabled: true, agentId: "main", allowConsultSessionPersistence: true, realtime: { provider: "openai" } };
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...base, conferences: { groups: { [groupId]: { ownerInviterToolPolicy: "owner" } } } } }), /conference/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...base, conferences: { groups: { [groupId]: { ownerInviterToolPolicy: "invalid" as "owner" } } } } }), /conference/);
});
