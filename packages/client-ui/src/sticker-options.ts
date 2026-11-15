import type { StickerPreparationRequest } from "@agent-wechat/shared";

/** Longest-edge choices offered for a sticker, in pixels. */
export const SIZE_STOPS = [120, 160, 200, 240, 300, 360, 480, 640, 800, 1024];
/** Frame-rate choices for a sticker made from video. */
export const FPS_STOPS = [5, 8, 10, 12, 15, 20, 24, 30];
/** WeChat refuses stickers with a longer edge. */
export const MAX_STICKER_EDGE = 1024;
/** WeChat on a phone draws stickers in a box at most this many pixels wide. */
export const PHONE_STICKER_EDGE = 384;
/** Longest segment a sticker may keep. */
export const MAX_SEGMENT_MS = 10_000;
const MAX_FRAMES = 200;
const VIDEO_EDGE = 240;
const VIDEO_FPS = 10;
const STEP_MS = 100;

/** What the browser could read about a chosen file before it is uploaded. */
export type StickerSource = {
  kind: "image" | "gif" | "video";
  width?: number;
  height?: number;
  /** Video duration, or the sum of a GIF's frame delays, in milliseconds. */
  durationMs?: number;
  /** GIF frame count. */
  frames?: number;
};
/** `edge` is sent as `maxEdge`; `px` is the longest edge that results. */
export type SizeStop = { edge: number | null; px: number; original: boolean };
/** Slider positions: `size` and `fps` are indexes into their stops. */
export type StickerOptions = { size: number; fps: number; startMs: number; lengthMs: number };
export type StickerRequestOptions = Omit<StickerPreparationRequest, "sourceId">;

export function sourceKind(file: { type: string; name: string }): StickerSource["kind"] {
  if (file.type.startsWith("video/") || /\.(mov|mp4|m4v)$/i.test(file.name)) return "video";
  return file.type === "image/gif" || /\.gif$/i.test(file.name) ? "gif" : "image";
}

const longestEdge = (source: StickerSource) =>
  source.width && source.height ? Math.max(source.width, source.height) : undefined;

/**
 * Sizes smaller than the source, then the source's own size when WeChat allows it.
 * Images keep their bytes at that last stop (`maxEdge: null`); video always names an edge.
 */
export function sizeStops(source: StickerSource): SizeStop[] {
  const longest = longestEdge(source);
  const stops: SizeStop[] = SIZE_STOPS.filter(edge => longest === undefined || edge < longest).map(edge => ({ edge, px: edge, original: false }));
  if (longest !== undefined && longest <= MAX_STICKER_EDGE)
    stops.push({ edge: source.kind === "video" ? Math.max(16, longest) : null, px: longest, original: true });
  return stops;
}

/** True when an image is larger than WeChat allows, so even its largest stop shrinks it. */
export const exceedsStickerEdge = (source: StickerSource) => (longestEdge(source) ?? 0) > MAX_STICKER_EDGE;

/**
 * The part of a clip the Start and Length sliders can choose from, in whole tenths of a
 * second. Video always gets them; a GIF only when it is over the length or frame limit.
 */
export function trimRange(source: StickerSource): { totalMs: number } | null {
  const duration = source.durationMs ?? 0;
  const needed = source.kind === "video" || source.kind === "gif" && (duration > MAX_SEGMENT_MS || (source.frames ?? 0) > MAX_FRAMES);
  const totalMs = Math.floor(duration / STEP_MS) * STEP_MS;
  return needed && totalMs >= 2 * STEP_MS ? { totalMs } : null;
}

export function defaultOptions(source: StickerSource): StickerOptions {
  const stops = sizeStops(source);
  const size = source.kind === "video" ? stops.reduce((best, stop, index) => (stop.px <= VIDEO_EDGE ? index : best), 0) : stops.length - 1;
  const range = trimRange(source);
  return { size, fps: FPS_STOPS.indexOf(VIDEO_FPS), startMs: 0, lengthMs: range ? Math.min(MAX_SEGMENT_MS, range.totalMs) : 0 };
}

/** Moves the start and keeps the length inside the clip. */
export function withStart(source: StickerSource, options: StickerOptions, startMs: number): StickerOptions {
  const range = trimRange(source);
  if (!range) return options;
  const start = Math.min(Math.max(0, startMs), range.totalMs - STEP_MS);
  return { ...options, startMs: start, lengthMs: Math.min(options.lengthMs, maxLength(range, start)) };
}

export function withLength(source: StickerSource, options: StickerOptions, lengthMs: number): StickerOptions {
  const range = trimRange(source);
  if (!range) return options;
  return { ...options, lengthMs: Math.min(Math.max(STEP_MS, lengthMs), maxLength(range, options.startMs)) };
}

export const maxLength = (range: { totalMs: number }, startMs: number) => Math.min(MAX_SEGMENT_MS, range.totalMs - startMs);

/** A segment only when it differs from the whole clip, or the clip is too long to keep whole. */
function segment(source: StickerSource, options: StickerOptions): StickerRequestOptions["trim"] {
  const range = trimRange(source);
  if (!range) return null;
  const whole = options.startMs === 0 && options.lengthMs >= range.totalMs;
  return whole && (source.durationMs ?? 0) <= MAX_SEGMENT_MS ? null : { startMs: options.startMs, durationMs: options.lengthMs };
}

export function stickerRequest(source: StickerSource, options: StickerOptions): StickerRequestOptions {
  const stops = sizeStops(source);
  const stop = stops[Math.min(options.size, stops.length - 1)];
  const video = source.kind === "video";
  return { fit: false, maxEdge: stop?.edge ?? null, fps: video ? FPS_STOPS[options.fps] ?? VIDEO_FPS : null, trim: segment(source, options) };
}

/** One key per distinct request, so equal requests share a prepared result. */
export const requestKey = (request: StickerRequestOptions) =>
  JSON.stringify({ fit: request.fit, maxEdge: request.maxEdge, fps: request.fps, trim: request.trim && { startMs: request.trim.startMs, durationMs: request.trim.durationMs } });

/** Where `px` falls along the stops, from 0 to 1, or undefined outside them. */
export function stopPosition(stops: SizeStop[], px: number): number | undefined {
  if (stops.length < 2 || px < stops[0].px || px > stops[stops.length - 1].px) return undefined;
  for (let index = 0; index < stops.length - 1; index++) {
    const from = stops[index].px, to = stops[index + 1].px;
    if (px <= to) return (index + (px - from) / (to - from)) / (stops.length - 1);
  }
  return undefined;
}

/**
 * Frame count and the sum of frame delays, read the way the hub reads them: a delay
 * belongs to the frame that follows it, in hundredths of a second. Null if not a GIF.
 */
export function gifTiming(bytes: Uint8Array): { durationMs: number; frames: number } | null {
  if (bytes.length < 13 || !/^GIF8[79]a$/.test(String.fromCharCode(...bytes.subarray(0, 6)))) return null;
  const table = (flags: number) => (flags & 0x80 ? 3 << ((flags & 7) + 1) : 0);
  let at = 13 + table(bytes[10]);
  const skipBlocks = () => {
    while (at < bytes.length) {
      const size = bytes[at++];
      if (!size) return;
      at += size;
    }
  };
  let delay = 0, durationMs = 0, frames = 0;
  while (at < bytes.length) {
    const block = bytes[at++];
    if (block === 0x3b) break;
    if (block === 0x21) {
      const label = bytes[at++];
      if (label === 0xf9 && bytes[at] >= 4) delay = bytes[at + 2] | (bytes[at + 3] << 8);
      skipBlocks();
    } else if (block === 0x2c) {
      at += 9;
      at += table(bytes[at - 1]) + 1;
      skipBlocks();
      frames++;
      durationMs += delay * 10;
      delay = 0;
    } else return null;
  }
  return frames ? { durationMs, frames } : null;
}

function imageShape(file: File): Promise<Partial<StickerSource>> {
  return new Promise(resolve => {
    const url = URL.createObjectURL(file), image = new Image();
    const finish = (shape: Partial<StickerSource>) => { URL.revokeObjectURL(url); resolve(shape); };
    image.onload = () => finish(image.naturalWidth && image.naturalHeight ? { width: image.naturalWidth, height: image.naturalHeight } : {});
    image.onerror = () => finish({});
    image.src = url;
  });
}

function videoShape(file: File): Promise<Partial<StickerSource>> {
  return new Promise(resolve => {
    const url = URL.createObjectURL(file), video = document.createElement("video");
    const finish = (shape: Partial<StickerSource>) => {
      clearTimeout(timer);
      video.onloadedmetadata = video.onerror = null;
      video.removeAttribute("src");
      video.load();
      URL.revokeObjectURL(url);
      resolve(shape);
    };
    const timer = setTimeout(() => finish({}), 10_000);
    video.preload = "metadata";
    video.muted = true;
    video.onloadedmetadata = () => finish({
      ...(video.videoWidth && video.videoHeight ? { width: video.videoWidth, height: video.videoHeight } : {}),
      ...(Number.isFinite(video.duration) && video.duration > 0 ? { durationMs: video.duration * 1000 } : {}),
    });
    video.onerror = () => finish({});
    video.src = url;
  });
}

/** Reads dimensions (and timing) locally. Anything unreadable is left out, never guessed. */
export async function readStickerSource(file: File): Promise<StickerSource> {
  const kind = sourceKind(file);
  try {
    if (kind === "video") return { kind, ...(await videoShape(file)) };
    const shape = await imageShape(file);
    const timing = kind === "gif" && file.size <= 64 * 1024 * 1024 ? gifTiming(new Uint8Array(await file.arrayBuffer())) : null;
    return { kind, ...shape, ...timing };
  } catch {
    return { kind };
  }
}
