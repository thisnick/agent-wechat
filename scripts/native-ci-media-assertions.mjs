import assert from "node:assert/strict";

export function assertSameUnavailableMedia(actual, expected) {
  const identity = ({ id, accountId, messageId, kind, variant, advertisedByteLength }) =>
    ({ id, accountId, messageId, kind, variant, advertisedByteLength });
  // Background transfers can change state, reason and retryability between
  // requests. The batch must still return the same advertised assets.
  assert.deepEqual(actual.map(identity), expected.map(identity));
  for (const assets of [actual, expected]) {
    assert.ok(assets.length > 0);
    assert.ok(assets.every(asset => asset.state !== "available" && asset.sha256 === null),
      "unsupported simulator media must remain unverified and unavailable");
  }
}
