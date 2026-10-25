import test from "node:test";
import assert from "node:assert/strict";
import { mediaSwipeDirection } from "../src/media-swipe.ts";

const gesture = {
  pointerType: "touch", startX: 220, startY: 300,
  endX: 80, endY: 310, elapsed: 250,
  startScale: 1, endScale: 1, cancelled: false,
};

test("a horizontal touch swipe browses forward or backward", () => {
  assert.equal(mediaSwipeDirection(gesture), 1);
  assert.equal(mediaSwipeDirection({ ...gesture, endX: 360 }), -1);
});

test("taps, vertical scrolling and slow drags do not change media", () => {
  for (const extra of [
    { endX: 217, endY: 302 },
    { endX: 180, endY: 100 },
    { endX: 120, endY: 170 },
    { elapsed: 1000 },
    { pointerType: "mouse" },
  ]) assert.equal(mediaSwipeDirection({ ...gesture, ...extra }), null);
});

test("pinch, zoomed panning and cancelled gestures cannot navigate", () => {
  for (const extra of [
    { startScale: 2, endScale: 2 },
    { startScale: 1, endScale: 2 },
    { startScale: 2, endScale: 1 },
    { cancelled: true },
  ]) assert.equal(mediaSwipeDirection({ ...gesture, ...extra }), null);
});
