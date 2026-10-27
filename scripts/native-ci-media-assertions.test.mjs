import assert from "node:assert/strict";
import test from "node:test";
import { assertSameUnavailableMedia } from "./native-ci-media-assertions.mjs";

const pending = {
  id: "asset-image", accountId: "personal", messageId: "image",
  kind: "image", variant: "standard", advertisedByteLength: 100,
  state: "pending", reason: null, retryable: true, sha256: null,
};
const failed = {
  ...pending, state: "failed", reason: "media_processing_failed", retryable: false,
};

test("a transfer failing between single and batch reads preserves asset equivalence", () => {
  assertSameUnavailableMedia([failed], [pending]);
  assertSameUnavailableMedia([pending], [failed]);
});

for (const [field, value] of Object.entries({
  id: "another-asset", accountId: "work", messageId: "another-message",
  kind: "file", variant: "thumbnail", advertisedByteLength: 200,
})) {
  test(`batch media must preserve ${field} across state changes`, () => {
    assert.throws(() => assertSameUnavailableMedia([{ ...failed, [field]: value }], [pending]));
  });
}

test("missing or extra batch assets are rejected", () => {
  assert.throws(() => assertSameUnavailableMedia([], [pending]));
  assert.throws(() => assertSameUnavailableMedia([failed, failed], [pending]));
  assert.throws(() => assertSameUnavailableMedia([], []));
});

test("neither snapshot may claim available or verified media", () => {
  for (const invalid of [{ ...pending, state: "available" }, { ...failed, sha256: "verified-hash" }]) {
    assert.throws(() => assertSameUnavailableMedia([invalid], [pending]));
    assert.throws(() => assertSameUnavailableMedia([failed], [invalid]));
  }
});
