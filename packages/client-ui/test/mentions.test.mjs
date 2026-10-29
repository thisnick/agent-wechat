import test from "node:test";
import assert from "node:assert/strict";
import { allMentionProblem, canMentionAll, mentionOptions, mentionQuery, removeAllMentions, validMentionReceipt, validMentionTokens } from "../src/mentions.ts";

const token = (userId, label, start) => ({ userId, label, start, end: start + label.length + 2 });
test("inline recipients preserve UTF-16 positions, repeated people and surrounding text", () => {
  const text = "😀 Hi @Jin\u2005 — ask @Mei\u2005 and @Jin\u2005.";
  const tokens = [token("wxid_jin", "Jin", 6), token("wxid_mei", "Mei", 18), token("wxid_jin", "Jin", 28)];
  // Derive the two later offsets from the actual draft, avoiding code-point counts.
  tokens[1] = token("wxid_mei", "Mei", text.indexOf("@Mei"));
  tokens[2] = token("wxid_jin", "Jin", text.lastIndexOf("@Jin"));
  const options = mentionOptions(text, tokens);
  assert.deepEqual(options.userIds, ["wxid_jin", "wxid_mei"]);
  assert.deepEqual(options.positions, tokens.map((value, index) => ({ start: value.start, end: value.end, index: index === 1 ? 1 : 0 })));
  assert.equal(validMentionReceipt(text, options), true);
  assert.equal(mentionOptions("Just @Jin without a selected identity", []), undefined);
});

test("Everyone is separate from individual recipients", () => {
  assert.deepEqual(mentionOptions("Hi @all\u2005!", [token(null, "all", 3)]), { all: true, positions: [{ start: 3, end: 8, index: 0 }] });
  assert.equal(validMentionTokens("@all\u2005@Jin\u2005", [token(null, "all", 0), token("jin", "Jin", 5)]), false);
});

test("Everyone follows new server permission and preserves legacy group response behavior", () => {
  const group = { id: "room@chatroom", roomId: "room@chatroom", isInGroup: true, ownerId: "wxid_me" };
  for (const permission of [false, null, "true", 1])
    assert.equal(canMentionAll({ ...group, canMentionAll: permission }, group.id), false);
  assert.equal(canMentionAll(group, group.id), true, "a legacy server omits the field");
  assert.equal(canMentionAll({ ...group, ownerId: "someone_else" }, group.id), true, "legacy behavior does not infer roles");
  assert.equal(canMentionAll({ ...group, canMentionAll: true }, group.id), true);
  assert.equal(canMentionAll({ ...group, ownerId: "someone_else", canMentionAll: true }, group.id), true, "server may grant a non-owner role");
  assert.equal(canMentionAll({ ...group, canMentionAll: true, isInGroup: false }, group.id), false);
  assert.equal(canMentionAll({ ...group, canMentionAll: true }, "other@chatroom"), false);
  assert.equal(canMentionAll({ ...group, id: "peer", canMentionAll: true }, "peer"), false);
  assert.equal(canMentionAll(null, group.id), false);
  assert.match(allMentionProblem({ ...group, canMentionAll: false }, group.id), /can’t mention everyone/);
  assert.match(allMentionProblem({ ...group, canMentionAll: null }, group.id), /permissions are unavailable/);
  assert.equal(allMentionProblem({ ...group, canMentionAll: true }, group.id), "");
});

test("removing a blocked Everyone selection preserves the authored body and ordinary recipients", () => {
  const text = "😀 Before @all\u2005 after", tokens = [token(null, "all", text.indexOf("@all"))];
  assert.deepEqual(removeAllMentions(text, tokens), { text: "😀 Before  after", tokens: [] });
  assert.equal(text.slice(tokens[0].start, tokens[0].end), "@all\u2005", "removal does not mutate saved draft inputs");
  const members = "Ask @Jin\u2005 about it", selected = [token("wxid_jin", "Jin", 4)];
  assert.deepEqual(removeAllMentions(members, selected), { text: members, tokens: selected });
  assert.throws(() => removeAllMentions("edited", tokens), /Choose/);
});

test("edited, overlapping, corrupt and incomplete persisted identities fail closed", () => {
  const text = "@Jin\u2005", tokens = [token("jin", "Jin", 0)];
  for (const value of [null, {}, [null], [{ ...tokens[0], label: "Mei" }], [{ ...tokens[0], userId: "notify@all" }], [{ ...tokens[0], start: -1 }], [...tokens, ...tokens]])
    assert.equal(validMentionTokens(text, value), false);
  const valid = mentionOptions(text, tokens);
  for (const value of [null, { ...valid, positions: [null] }, { ...valid, userIds: ["jin", "mei"] }, { ...valid, userIds: ["jin", "jin"] }, { ...valid, all: true }, { ...valid, positions: [] }, { ...valid, positions: [{ start: 0, end: 5, index: 2 }] }, { ...valid, extra: true }])
    assert.equal(validMentionReceipt(text, value), false);
  assert.equal(validMentionTokens("@😀\u2005", [token("jin", "😀", 0)]), true);
  assert.equal(validMentionTokens("@\ud800\u2005", [token("jin", "\ud800", 0)]), false);
  assert.equal(validMentionTokens("@\u0085\u2005", [token("jin", "\u0085", 0)]), false);
  assert.throws(() => mentionOptions(text + "x".repeat(65536), tokens), /too long/);
});

test("picker activates at the cursor, avoids emails and already selected mentions", () => {
  assert.deepEqual(mentionQuery("Before @Me after", 10, []), { start: 7, end: 10, query: "Me" });
  assert.equal(mentionQuery("name@example.com", 16, []), null);
  assert.equal(mentionQuery("@Jin\u2005", 5, [token("jin", "Jin", 0)]), null);
  assert.equal(mentionQuery("@Jin\u2005", 4, [token("jin", "Jin", 0)]), null);
});
