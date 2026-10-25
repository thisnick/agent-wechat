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

test("downloads prefer the advertised original and never substitute standard bytes", async () => {
  const { downloadAsset, conversationMedia } = await import(
    "../src/media-variants.ts"
  );
  const standard = {
    id: "s",
    variant: "standard",
    state: "available",
    actualVariant: "standard",
  };
  const original = {
    id: "o",
    variant: "original",
    state: "pending",
    retryable: true,
  };
  assert.equal(downloadAsset([standard, original], standard), original);
  assert.equal(downloadAsset([standard], standard), standard);
  assert.equal(
    downloadAsset(
      [
        { ...original, state: "available", actualVariant: "original" },
        standard,
      ],
      standard,
    ).id,
    "o",
  );
  assert.deepEqual(
    conversationMedia(
      [
        { id: "photo", accountId: "a", threadId: "t", type: 3 },
        { id: "video", accountId: "a", threadId: "t", type: 43 },
        { id: "pdf", accountId: "a", threadId: "t", type: 49 },
        { id: "sticker", accountId: "a", threadId: "t", type: 47 },
        { id: "recalled", accountId: "a", threadId: "t", type: 3, recall: {} },
        { id: "other-account", accountId: "b", threadId: "t", type: 3 },
        { id: "other-chat", accountId: "a", threadId: "u", type: 3 },
      ],
      "a",
      "t",
    ).map((m) => m.id),
    ["photo", "video"],
  );
});

test("video browsing uses thumbnails while the viewer prefers cached originals or normal playback", async () => {
  const { videoAsset } = await import("../src/media-variants.ts");
  const thumbnail = { id: "thumb", variant: "thumbnail", state: "pending" };
  const normal = { id: "normal", variant: "normal", state: "pending" };
  const original = { id: "original", variant: "original", state: "pending" };
  assert.equal(videoAsset([original, normal, thumbnail], false), thumbnail);
  assert.equal(videoAsset([original, normal, thumbnail], true), normal);
  assert.equal(videoAsset([original, normal], false), undefined);
  assert.equal(videoAsset([original], true), undefined);
  const cached = { ...original, state: "available", actualVariant: "original" };
  assert.equal(videoAsset([normal, cached, thumbnail], true), cached);
  assert.equal(videoAsset([normal, cached, thumbnail], false), thumbnail);
});

test("carousel upgrades accept only verified cached originals", async () => {
  const { cachedOriginal } = await import("../src/media-variants.ts");
  const original = {
    id: "o",
    variant: "original",
    actualVariant: "original",
    state: "available",
  };
  assert.equal(cachedOriginal([original]), original);
  for (const change of [
    { state: "pending" },
    { state: "expired" },
    { actualVariant: "standard" },
    { variant: "standard" },
  ])
    assert.equal(cachedOriginal([{ ...original, ...change }]), undefined);
});

test("image previews fall back to thumbnails without bypassing the download threshold", async () => {
  const { imagePreview, canAutoRetrieve } = await import(
    "../src/media-variants.ts"
  );
  const standard = {
    id: "large",
    variant: "standard",
    state: "pending",
    autoDownloadAllowed: false,
  };
  const thumbnail = {
    id: "thumb",
    variant: "thumbnail",
    state: "available",
    actualVariant: "thumbnail",
  };
  assert.equal(imagePreview([standard, thumbnail]), thumbnail);
  assert.equal(
    imagePreview([{ ...standard, autoDownloadAllowed: true }, thumbnail]),
    thumbnail,
  );
  assert.equal(
    imagePreview([
      { ...standard, state: "available", actualVariant: "standard" },
      thumbnail,
    ]).id,
    "large",
  );
  assert.equal(canAutoRetrieve(standard), false);
  assert.equal(canAutoRetrieve({ variant: "standard" }), false);
  assert.equal(canAutoRetrieve({ variant: "thumbnail" }), true);
  assert.equal(canAutoRetrieve({ kind: "voice", variant: "normal" }), true);
});
