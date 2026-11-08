import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { apiAssetUrl, downloadVerified } from "../scripts/media-download.mjs";

const bytes = Buffer.from("ffprobe");
const asset = {
  name: "ffprobe-linux-x64",
  sha256: createHash("sha256").update(bytes).digest("hex"),
  url: "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffprobe-linux-x64",
  assetId: 316529396,
};
const api = "https://api.github.com/repos/eugeneware/ffmpeg-static/releases/assets/316529396";
const storage = "https://release-assets.githubusercontent.com/ffprobe?sig=1";

function fakeFetch(routes) {
  const calls = [];
  const fetchImpl = async (url, options = {}) => {
    calls.push({ url, options });
    const route = routes[url];
    const reply = typeof route === "function" ? route() : route;
    if (!reply) return new Response(null, { status: 404 });
    return reply;
  };
  return { calls, fetchImpl };
}
const noWait = async () => {};

test("every pinned media asset names its GitHub release asset", async () => {
  const assets = JSON.parse(await readFile(new URL("../media-assets.json", import.meta.url), "utf8"));
  for (const pinned of assets) assert.match(apiAssetUrl(pinned) ?? "", /^https:\/\/api\.github\.com\/repos\/eugeneware\/ffmpeg-static\/releases\/assets\/\d+$/, pinned.name);
});

test("uses the release link when it works", async () => {
  const { calls, fetchImpl } = fakeFetch({ [asset.url]: () => new Response(bytes) });
  assert.deepEqual(await downloadVerified(asset, { fetchImpl, wait: noWait }), bytes);
  assert.deepEqual(calls.map((call) => call.url), [asset.url]);
});

test("falls back to the API and keeps the token off the storage host", async () => {
  const { calls, fetchImpl } = fakeFetch({
    [asset.url]: () => new Response(null, { status: 504 }),
    [api]: () => new Response(null, { status: 302, headers: { location: storage } }),
    [storage]: () => new Response(bytes),
  });
  assert.deepEqual(await downloadVerified(asset, { fetchImpl, token: "secret", wait: noWait }), bytes);
  assert.deepEqual(calls.map((call) => call.url), [asset.url, api, storage]);
  assert.equal(calls[1].options.headers.Authorization, "Bearer secret");
  assert.equal(calls[1].options.redirect, "manual");
  assert.equal(calls[2].options.headers, undefined);
});

test("retries both sources before giving up", async () => {
  const { calls, fetchImpl } = fakeFetch({
    [asset.url]: () => new Response(null, { status: 504 }),
    [api]: () => new Response(null, { status: 502 }),
  });
  const waits = [];
  await assert.rejects(downloadVerified(asset, { fetchImpl, wait: async (n) => waits.push(n) }),
    /Cannot download ffprobe-linux-x64 after 3 attempts: release link: HTTP 504; API: HTTP 502/);
  assert.equal(calls.length, 6);
  assert.deepEqual(waits, [1, 2]);
});

test("stops at once when the bytes do not match the pin", async () => {
  const { calls, fetchImpl } = fakeFetch({ [asset.url]: () => new Response("tampered") });
  await assert.rejects(downloadVerified(asset, { fetchImpl, wait: noWait }), /Integrity check failed for ffprobe-linux-x64/);
  assert.equal(calls.length, 1);
});
