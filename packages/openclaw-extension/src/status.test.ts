import test from "node:test";
import assert from "node:assert/strict";
import { collectWeChatStatusIssues } from "./status.ts";

test("collectWeChatStatusIssues: no issues when connected", () => {
  const issues = collectWeChatStatusIssues([
    {
      accountId: "default",
      connected: true,
      linked: true,
    },
  ]);
  assert.deepStrictEqual(issues, []);
});

test("collectWeChatStatusIssues: WARN when connected is explicitly false", () => {
  const issues = collectWeChatStatusIssues([
    {
      accountId: "default",
      connected: false,
    },
  ]);
  assert.equal(issues.length, 1);
  assert.equal(issues[0].kind, "runtime");
  assert.ok(issues[0].message.includes("Cannot reach"));
});

test("collectWeChatStatusIssues: WARN includes lastError when present", () => {
  const issues = collectWeChatStatusIssues([
    {
      accountId: "default",
      connected: false,
      lastError: "ECONNREFUSED",
    },
  ]);
  assert.equal(issues.length, 1);
  assert.ok(issues[0].message.includes("ECONNREFUSED"));
});

test("collectWeChatStatusIssues: no issue when connected is undefined (static snapshot)", () => {
  // CLI-side snapshots don't have runtime state, so connected is undefined.
  // This must NOT trigger a WARN.
  const issues = collectWeChatStatusIssues([
    { accountId: "default" },
  ]);
  assert.deepStrictEqual(issues, []);
});

test("collectWeChatStatusIssues: reports unlinked session", () => {
  const issues = collectWeChatStatusIssues([
    {
      accountId: "default",
      connected: true,
      linked: false,
    },
  ]);
  assert.equal(issues.length, 1);
  assert.equal(issues[0].kind, "auth");
  assert.ok(issues[0].message.includes("not authenticated"));
});

test("collectWeChatStatusIssues: handles empty array", () => {
  assert.deepStrictEqual(collectWeChatStatusIssues([]), []);
});

test("native connectivity issues point to the selected daemon instead of Docker lifecycle", () => {
  const issues = collectWeChatStatusIssues([{ accountId: "native", backend: "native", connected: false }]);
  assert.match(issues[0].fix, /native daemon/); assert.doesNotMatch(issues[0].fix, /wx-legacy|container/);
});
test("native unconfirmed handoff and uncertain custody are visible without retrying side effects", () => {
  const issues = collectWeChatStatusIssues([{ accountId: "native", backend: "native", connected: true, linked: true, handoffUnconfirmed: true, needsReview: 1 }]);
  assert.equal(issues.length, 2); assert.match(issues[0].message, /read state remains unchanged/); assert.match(issues[1].fix, /do not automatically resend/);
});
