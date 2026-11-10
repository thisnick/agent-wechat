import * as initial from "./page.js";

const launchKey = location.hash.slice(1);
const images = new Map();
const blobs = new Set();
const lifetime = new AbortController();
const MAX_BYTES = 4 * 1024 * 1024;
let retainedBytes = 0;
let active = 0;
const waiting = [];

async function acquire() {
  lifetime.signal.throwIfAborted();
  if (active < 3) { active++; return; }
  await new Promise((resolve, reject) => waiting.push({ resolve, reject }));
}

function release() {
  const next = waiting.shift();
  if (next) next.resolve();
  else active--;
}

function imageType(bytes, header) {
  if (bytes.slice(0, 8).every((value, i) => value === [137, 80, 78, 71, 13, 10, 26, 10][i]) && bytes.length >= 8) return "image/png";
  if (bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255) return "image/jpeg";
  const signature = new TextDecoder().decode(bytes.slice(0, 12));
  if (/^GIF8[79]a/.test(signature)) return "image/gif";
  if (signature.startsWith("RIFF") && signature.slice(8) === "WEBP") return "image/webp";
  if (header?.split(";")[0].trim().toLowerCase() === "image/svg+xml") return "image/svg+xml";
  throw new Error("Unsupported image response");
}

// Display images use a separate public HTTPS image loader. The native host
// checks addresses and content and supplies no app headers, cookies or hub token.
export async function remoteImage(source) {
  const url = new URL(source);
  if (url.protocol !== "https:" || url.username || url.password || url.hash || source.length > 8192) {
    return Promise.reject(new Error("Unsupported image URL"));
  }
  if (!initial.allowImages || !launchKey) return Promise.reject(new Error("Image network access is disabled"));
  if (images.has(url.href)) return images.get(url.href);
  if (images.size >= 256) return Promise.reject(new Error("Image limit reached"));
  const pending = (async () => {
    await acquire();
    try {
      const response = await fetch("/_wx/image", {
        method: "POST", credentials: "omit", cache: "no-store", signal: lifetime.signal,
        headers: { "Content-Type": "application/json", "x-wx-launch": launchKey },
        body: JSON.stringify({ url: url.href }),
      });
      if (!response.ok) throw new Error("Image request was denied or failed");
      const reply = await response.json();
      if (reply.statusCode < 200 || reply.statusCode >= 300 || typeof reply.body !== "string"
        || reply.body.length > 4 * Math.ceil(MAX_BYTES / 3)) throw new Error("Invalid image response");
      const bytes = Uint8Array.from(atob(reply.body), character => character.charCodeAt(0));
      if (!bytes.length || bytes.length > MAX_BYTES || retainedBytes + bytes.length > 32 * 1024 * 1024) {
        throw new Error("Image size limit reached");
      }
      const type = imageType(bytes, reply.header?.["content-type"]);
      lifetime.signal.throwIfAborted();
      const objectUrl = URL.createObjectURL(new Blob([bytes], { type }));
      blobs.add(objectUrl);
      retainedBytes += bytes.length;
      return objectUrl;
    } finally {
      release();
    }
  })();
  images.set(url.href, pending);
  return pending;
}

window.addEventListener("pagehide", () => {
  lifetime.abort();
  for (const pending of waiting.splice(0)) pending.reject(new Error("Image host closed"));
  for (const url of blobs) URL.revokeObjectURL(url);
  blobs.clear();
  images.clear();
  retainedBytes = 0;
}, { once: true });
