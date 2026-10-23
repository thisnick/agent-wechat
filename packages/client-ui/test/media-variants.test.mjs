import test from "node:test";
import assert from "node:assert/strict";
import {
  standardImage,
  cachedVariant,
  canRetrieve,
} from "../src/media-variants.ts";

test("images keep standard quality when a cached original is also present", () => {
  const standard = {
    id: "standard",
    variant: "standard",
    state: "pending",
    retryable: true,
  };
  const original = {
    id: "original",
    variant: "original",
    actualVariant: "original",
    state: "available",
  };
  assert.equal(standardImage([original, standard]), standard);
  assert.equal(canRetrieve(standard), true);
  assert.equal(standardImage([original]), undefined);
});
test("advertised originals and lower quality fallback bytes are not saved originals", () => {
  assert.equal(
    cachedVariant({
      variant: "original",
      actualVariant: "standard",
      state: "available",
    }),
    false,
  );
  assert.equal(
    cachedVariant({
      variant: "original",
      actualVariant: "original",
      state: "pending",
    }),
    false,
  );
  assert.equal(
    cachedVariant({
      variant: "original",
      actualVariant: "original",
      state: "available",
    }),
    true,
  );
  for (const state of ["available", "expired", "unsupported"])
    assert.equal(canRetrieve({ state, retryable: true }), false);
});
