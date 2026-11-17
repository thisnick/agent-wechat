import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { defaultOptions, exceedsStickerEdge, gifTiming, requestKey, sizeStops, sourceKind, stickerRequest, stopPosition, trimRange, withLength, withStart } from "../src/sticker-options.ts";
import { StickerJobError } from "../src/sticker-jobs.ts";

const request = source => stickerRequest(source, defaultOptions(source));

test("images keep their size by default and offer only smaller stops", () => {
  const source = { kind: "image", width: 800, height: 600 };
  const stops = sizeStops(source);
  assert.deepEqual(stops.map(stop => stop.px), [120, 160, 200, 240, 300, 360, 480, 640, 800]);
  assert.deepEqual(stops.at(-1), { edge: null, px: 800, original: true });
  assert.deepEqual(request(source), { fit: false, maxEdge: null, fps: null, trim: null });
  assert.equal(stickerRequest(source, { ...defaultOptions(source), size: 5 }).maxEdge, 360);
});

test("images over WeChat's limit default to 1024 and have no original stop", () => {
  const source = { kind: "gif", width: 1600, height: 1200, durationMs: 1200, frames: 12 };
  assert.equal(sizeStops(source).some(stop => stop.original), false);
  assert.equal(exceedsStickerEdge(source), true);
  assert.deepEqual(request(source), { fit: false, maxEdge: 1024, fps: null, trim: null });
});

test("a tiny image has only its original size, and an unreadable one never enlarges", () => {
  assert.deepEqual(sizeStops({ kind: "gif", width: 96, height: 96 }), [{ edge: null, px: 96, original: true }]);
  const unknown = { kind: "image" };
  assert.equal(sizeStops(unknown).length, 10);
  assert.equal(request(unknown).maxEdge, 1024);
});

test("video defaults to 240 pixels and 10 fps and always names its edge", () => {
  const clip = { kind: "video", width: 1280, height: 858, durationMs: 4000 };
  assert.deepEqual(request(clip), { fit: false, maxEdge: 240, fps: 10, trim: null });
  const small = { kind: "video", width: 200, height: 112, durationMs: 4000 };
  assert.deepEqual(sizeStops(small).map(stop => stop.edge), [120, 160, 200]);
  assert.equal(request(small).maxEdge, 200);
  assert.equal(request({ kind: "video" }).maxEdge, 240);
  assert.equal(trimRange({ kind: "video", width: 64, height: 64 }), null);
});

test("a segment is sent only when it differs from the whole clip or the clip is too long", () => {
  const clip = { kind: "video", width: 640, height: 360, durationMs: 4380 };
  const options = defaultOptions(clip);
  assert.equal(options.lengthMs, 4300);
  assert.equal(stickerRequest(clip, options).trim, null);
  assert.deepEqual(stickerRequest(clip, withStart(clip, options, 1000)).trim, { startMs: 1000, durationMs: 3300 });
  assert.deepEqual(stickerRequest(clip, withLength(clip, options, 2000)).trim, { startMs: 0, durationMs: 2000 });

  const long = { kind: "video", width: 640, height: 360, durationMs: 15_500 };
  assert.deepEqual(request(long).trim, { startMs: 0, durationMs: 10_000 });
  const later = withStart(long, defaultOptions(long), 6000);
  assert.deepEqual(stickerRequest(long, later).trim, { startMs: 6000, durationMs: 9500 });
  assert.equal(withLength(long, later, 12_000).lengthMs, 9500);
  assert.equal(withStart(long, later, 99_000).startMs, 15_400);

  assert.deepEqual(request({ kind: "video", durationMs: 10_050 }).trim, { startMs: 0, durationMs: 10_000 });
});

test("GIFs get a segment only when they are over the length or frame limit", () => {
  assert.equal(trimRange({ kind: "gif", durationMs: 4000, frames: 40 }), null);
  assert.deepEqual(trimRange({ kind: "gif", durationMs: 7000, frames: 210 }), { totalMs: 7000 });
  assert.deepEqual(request({ kind: "gif", width: 300, height: 300, durationMs: 12_340, frames: 120 }).trim, { startMs: 0, durationMs: 10_000 });
});

test("equal requests share a key and the phone mark sits between its stops", () => {
  const clip = { kind: "video", durationMs: 3000 };
  assert.equal(requestKey(request(clip)), requestKey({ trim: null, fps: 10, maxEdge: 240, fit: false }));
  assert.notEqual(requestKey(request(clip)), requestKey({ ...request(clip), fps: 12 }));
  const stops = sizeStops({ kind: "video" });
  assert.equal(stopPosition(stops, 384), (5 + 24 / 120) / 9);
  assert.equal(stopPosition(sizeStops({ kind: "image", width: 300, height: 300 }), 384), undefined);
});

test("file kinds come from the type or the name", () => {
  assert.equal(sourceKind({ type: "video/quicktime", name: "a.MOV" }), "video");
  assert.equal(sourceKind({ type: "", name: "clip.mp4" }), "video");
  assert.equal(sourceKind({ type: "image/gif", name: "x" }), "gif");
  assert.equal(sourceKind({ type: "image/png", name: "x.png" }), "image");
});

test("GIF timing counts each frame's preceding delay, as the hub does", async () => {
  const gce = delay => [0x21, 0xf9, 4, 0, delay & 0xff, delay >> 8, 0, 0];
  const frame = [0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x4c, 0x01, 0];
  const bytes = Uint8Array.from([...Buffer.from("GIF89a"), 1, 0, 1, 0, 0, 0, 0, ...gce(7), ...frame, ...frame, ...gce(5), ...frame, 0x3b]);
  assert.deepEqual(gifTiming(bytes), { durationMs: 120, frames: 3 });
  assert.equal(gifTiming(Uint8Array.from(Buffer.from("not a gif at all"))), null);
  const fixture = await readFile(new URL("../../native-client-rust/crates/wechat-simulator/fixtures/sticker.gif", import.meta.url));
  assert.ok(gifTiming(new Uint8Array(fixture)).frames >= 1);
});

test("job errors expose their safe result code for friendlier messages", () => {
  assert.equal(new StickerJobError({ id: "job-1", state: "failed", reason: "sticker_output_size_limit" }).reason, "sticker_output_size_limit");
  assert.equal(new StickerJobError({ id: "job-1", state: "failed", reason: "https://private" }).reason, "");
});
