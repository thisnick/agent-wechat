import test from "node:test";
import assert from "node:assert/strict";
import { EditBlocked, editReceipts, unsettled } from "../src/contact-editor.ts";

/** The browser cache's receipt store, in memory. */
function memoryCache() {
  const rows = new Map();
  return {
    rows,
    readState: (account, id) => rows.get(`${account}/${id}`),
    writeState: (account, id, value) => {
      if (value === null) rows.delete(`${account}/${id}`);
      else rows.set(`${account}/${id}`, value);
    },
  };
}
const privacyChange = (key, operation) => ({
  key,
  action: "privacy",
  contactId: "peer",
  input: { privacy: { chatsOnly: false, hideMyPosts: true, hideTheirPosts: false }, expectedVersion: "v1" },
  ...(operation ? { operation } : {}),
});
const remarkChange = (key) => ({ key, action: "remark", contactId: "peer", input: { remark: "Name", expectedVersion: "v1" } });

test("an edit queued behind a submission of unknown outcome can't overwrite or clear its receipt", () => {
  const cache = memoryCache();
  const receipts = editReceipts(cache, "one", "peer");
  const lost = privacyChange("privacy-key");
  receipts.write(lost, lost); // submitted; the response was lost
  const queued = remarkChange("remark-key");
  assert.throws(() => receipts.write(queued, queued), EditBlocked);
  assert.throws(() => receipts.write(queued, null), EditBlocked);
  assert.deepEqual(receipts.restore(), lost, "reopening the card still finds the original key");
  // Check or Retry continues the original submission with its own key.
  receipts.write(lost, { ...lost, operation: { id: "op", state: "running" } });
  receipts.write(lost, null);
  assert.equal(receipts.restore(), null);
  receipts.write(queued, queued);
  assert.equal(cache.rows.get("one/contact-change:privacy:peer").key, "remark-key");
});

test("settled receipts don't block, and only unsettled ones are restored", () => {
  for (const state of ["succeeded", "failed", "cancelled"]) {
    const cache = memoryCache();
    const receipts = editReceipts(cache, "one", "peer");
    cache.writeState("one", "contact-change:privacy:peer", privacyChange("old", { id: "op", state }));
    assert.equal(receipts.restore(), null);
    const next = remarkChange("next");
    receipts.write(next, next);
    assert.equal(cache.rows.get("one/contact-change:privacy:peer").key, "next");
  }
  for (const state of [undefined, "queued", "running", "needs_review"])
    assert.equal(unsettled(privacyChange("k", state ? { id: "op", state } : undefined)), true);
  // Request receipts share the naming scheme elsewhere, but are never edit receipts.
  assert.equal(unsettled({ key: "k", action: "accept", requestId: "r", version: "v" }), false);
});

test("the slot is per account and contact, and keeps the name earlier versions used", () => {
  const cache = memoryCache();
  const lost = privacyChange("lost");
  editReceipts(cache, "one", "peer").write(lost, lost);
  const other = remarkChange("other");
  editReceipts(cache, "one", "someone-else").write(other, other);
  editReceipts(cache, "two", "peer").write(other, other);
  assert.deepEqual([...cache.rows.keys()].sort(), [
    "one/contact-change:privacy:peer",
    "one/contact-change:privacy:someone-else",
    "two/contact-change:privacy:peer",
  ]);
});
