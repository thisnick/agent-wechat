import test from "node:test";
import assert from "node:assert/strict";
import type { NativeSuppliedAsset } from "./native-consumer-state.ts";
import { nativeMediaFacts } from "./native-media-facts.ts";

const asset = (value: Partial<NativeSuppliedAsset> & Pick<NativeSuppliedAsset, "kind" | "mime" | "path">): NativeSuppliedAsset => ({
  id: value.path, sha256: "0".repeat(64), byteLength: 1, role: "file_reference", runtimeMedia: false, contextRole: "trigger", ...value });

test("each supplied asset becomes one media fact with its name and message", () => {
  assert.deepEqual(nativeMediaFacts([asset({ kind: "file", mime: "application/msword", path: "/m/doc---1.doc", fileName: "招标文件.doc", messageId: "m1" })]),
    [{ path: "/m/doc---1.doc", url: "/m/doc---1.doc", contentType: "application/msword", kind: "document", fileName: "招标文件.doc", messageId: "m1" }]);
});
test("only the runtime image is hydrated; its prepared original and other images are not", () => {
  const facts = nativeMediaFacts([
    asset({ kind: "image", mime: "image/png", path: "/m/earlier.png", messageId: "m1" }),
    asset({ kind: "image", mime: "image/jpeg", path: "/m/b.model.jpg", role: "model_image", runtimeMedia: true, messageId: "m2",
      sourcePath: "/m/b.wxgf", sourceMime: "image/x-wxgf" }),
  ]);
  assert.deepEqual(facts.map(fact => [fact.path, fact.contentType, fact.kind, fact.hydrationSuppressed ?? false]),
    [["/m/earlier.png", "image/png", "image", true], ["/m/b.model.jpg", "image/jpeg", "image", false], ["/m/b.wxgf", "image/x-wxgf", "image", true]]);
});
test("an image sent as a file is a suppressed image, so file extraction does not hydrate it", () => {
  assert.deepEqual(nativeMediaFacts([asset({ kind: "file", mime: "image/png", path: "/m/scan.png" })]).map(fact => [fact.kind, fact.hydrationSuppressed]), [["image", true]]);
});
test("voice notes are marked transcribed only for the messages that were", () => {
  const facts = nativeMediaFacts([asset({ kind: "audio", mime: "audio/mpeg", path: "/m/a.mp3", messageId: "a" }), asset({ kind: "audio", mime: "audio/mpeg", path: "/m/b.mp3", messageId: "b" })], new Set(["a"]));
  assert.deepEqual(facts.map(fact => fact.transcribed ?? false), [true, false]);
});
