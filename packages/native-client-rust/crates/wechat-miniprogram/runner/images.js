import * as initial from "./page.js";

const launchKey = location.hash.slice(1);
const images = new Map();
const blobs = new Set();
const lifetime = new AbortController();
const MAX_BYTES = 4 * 1024 * 1024;
let retainedBytes = 0;
let active = 0;
const waiting = [];

// Both template adapters resolve images through the same package/network policy.
export async function imageSource(value, route) {
  if (typeof value !== "string" || !value) throw new Error("Image source is empty");
  if (value.startsWith("https://")) return remoteImage(value);
  if (value.length <= 2 * 1024 * 1024
    && /^data:image\/(?:png|jpeg|gif|webp|svg\+xml)(?:;[^,]*)?,/i.test(value)) return value;
  if (/^[a-zA-Z0-9_.@/-]+$/.test(value) && !value.startsWith("//")) {
    const url = new URL(value, new URL("/" + route, location.origin));
    if (url.origin === location.origin) return url.href;
  }
  throw new Error("Unsupported image source");
}

// Glass-easel supplies component mechanics, not WeChat's native image component.
// Register that component through the public adapter API instead of mutating DOM
// produced by the template engine.
export function registerGlassImage(space, route) {
  const path = "__wx_builtin_image";
  space.addCompiledTemplate(path, { groupList: imageTemplates, content: imageTemplates[path], fallbackListenerOnNativeNode: true });
  space.addComponentStaticConfig(path, { component: true, styleIsolation: "shared" });
  const jobs = new WeakMap();
  const refresh = instance => {
    const job = { alive: true };
    jobs.set(instance, job);
    instance.setData({ resolvedSrc: "" });
    if (!instance.data.src) return;
    void imageSource(instance.data.src, route).then(source => {
      if (!job.alive || jobs.get(instance) !== job) return;
      const probe = new Image();
      probe.onload = () => {
        if (!job.alive || jobs.get(instance) !== job) return;
        job.size = { width: probe.naturalWidth, height: probe.naturalHeight };
        instance.setData({ resolvedSrc: source });
      };
      probe.onerror = () => {
        if (job.alive && jobs.get(instance) === job) instance.triggerEvent("error", { errMsg: "Image is unavailable" });
      };
      probe.src = source;
    }, () => {
      if (job.alive && jobs.get(instance) === job) instance.triggerEvent("error", { errMsg: "Image is unavailable" });
    });
  };
  space.componentEnv(path, ({ Component }) => Component({
    properties: {
      src: { type: String, value: "", observer() { refresh(this); } },
      mode: { type: String, value: "scaleToFill", observer(value) {
        this.setData({ fit: ["aspectFit", "aspectFill", "widthFix", "heightFix", "top", "bottom", "center", "left", "right", "top left", "top right", "bottom left", "bottom right"].includes(value)
          ? value.replaceAll(" ", "-") : "scaleToFill" });
      } },
    },
    data: { resolvedSrc: "", fit: "scaleToFill" },
    lifetimes: {
      attached() { refresh(this); },
      detached() { const job = jobs.get(this); if (job) job.alive = false; },
    },
    methods: {
      loaded() { const job = jobs.get(this); if (job?.alive && job.size) this.triggerEvent("load", job.size); },
      failed() { this.triggerEvent("error", { errMsg: "Image is unavailable" }); },
    },
  }));
  return path;
}

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

// Synthetic <img> template compiled with glass-easel-template-compiler 1.2.1.
const imageTemplates = (()=>{var G={};var R={};var X=function(a){return a==null?Object.create(null):a};var Y=function(a){return a==null?'':String(a)};var Z=function(a,b){if(a===true)return true;if(a)return a[b]};var P=function(a){return typeof a==='function'?a:()=>{}};var Q={a:function(a){for(var i=0;i<a.length;i++)if(a[i])return a},b:function(b){var a=Object.values(b);for(var i=0;i<a.length;i++)if(a[i])return a}};G["__wx_builtin_image"]=(()=>{var H={};var S;var I=(P)=>{if(!S)S=Object.assign({},H);return S[P]};H[""]=(R,C,D,U,A)=>{if(typeof R.setEventListenerWrapper==='function')R.setEventListenerWrapper(Q.C);var L=R.c,M=R.m,O=R.r,K=U===true,b,d=(C)=>{},c=(C,T,E)=>{if(b===1){E("img",{},(N,C)=>{if(C||K||U.fit)L(N,D.fit);if(C||K||U.resolvedSrc)O(N,"src",D.resolvedSrc);if(C)O(N,"bindload","loaded");if(C)O(N,"binderror","failed")},d)}},a=(C,T,E,B)=>{b=D.resolvedSrc?1:0;B(b,c);C=!1;U=Object.create(null);K=!1};A=A||{};return {C:a,B:A}};return Object.assign(function(R){return H[R]},{_:H})})();return G})();
