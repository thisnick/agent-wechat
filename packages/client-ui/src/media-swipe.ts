export type SwipeGesture = {
  pointerType: string;
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  elapsed: number;
  startScale: number;
  endScale: number;
  cancelled: boolean;
};

/** Browse at fit-to-screen size; zoomed gestures belong to the image pan. */
export function mediaSwipeDirection(gesture: SwipeGesture): -1 | 1 | null {
  if (
    gesture.pointerType !== "touch" ||
    gesture.cancelled ||
    gesture.startScale > 1.001 ||
    gesture.endScale > 1.001 ||
    gesture.elapsed > 800
  )
    return null;
  const horizontal = gesture.endX - gesture.startX;
  const vertical = gesture.endY - gesture.startY;
  if (
    Math.abs(horizontal) < 48 ||
    Math.abs(horizontal) < Math.abs(vertical) * 1.5
  )
    return null;
  return horizontal < 0 ? 1 : -1;
}
