import test from "node:test";
import assert from "node:assert/strict";
import { MediaPreviewCache } from "../src/media-preview-cache.ts";
const asset = (id = "photo", extra = {}) => ({
  id,
  accountId: "a",
  messageId: "m",
  kind: "image",
  variant: "standard",
  actualVariant: "standard",
  state: "available",
  sha256: "source-1",
  ...extra,
});
const content = (text = "image bytes") => ({
  bytes: new TextEncoder().encode(text),
  contentType: "image/png",
});
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

test("chat, viewer and thumbnail leases share one request and survive closing/reopening", async (t) => {
  let reads = 0;
  const loading = deferred();
  const cache = new MediaPreviewCache({
    mediaContent: async () => {
      reads++;
      return loading.promise;
    },
  });
  t.after(() => cache.close());
  const chat = cache.acquire(asset()),
    viewer = cache.acquire(asset()),
    thumbnail = cache.acquire(asset());
  chat.release();
  loading.resolve(content());
  const [one, two] = await Promise.all([viewer.ready, thumbnail.ready]);
  assert.equal(reads, 1);
  assert.equal(one.url, two.url);
  viewer.release();
  thumbnail.release();
  assert.equal(cache.peek(asset()).url, one.url);
  const reopened = cache.acquire(asset());
  assert.equal((await reopened.ready).url, one.url);
  assert.equal(reads, 1);
  assert.equal(await (await fetch(one.url)).text(), "image bytes");
  reopened.release();
});

test("accounts, qualities, source hashes and rendered previews have separate cache entries", async (t) => {
  const requests = [];
  const cache = new MediaPreviewCache({
    mediaContent: async (account, id, options) => {
      requests.push([account, id, options.representation]);
      return content(String(requests.length));
    },
  });
  t.after(() => cache.close());
  const versions = [
    asset(),
    asset("photo", { accountId: "b" }),
    asset("photo", { variant: "original", actualVariant: "original" }),
    asset("photo", { sha256: "changed" }),
    asset("photo", {
      preview: { sha256: "rendered-1", sourceSha256: "source-1" },
    }),
    asset("photo", {
      preview: { sha256: "rendered-2", sourceSha256: "source-1" },
    }),
  ];
  const urls = [];
  for (const item of versions) {
    const lease = cache.acquire(item);
    urls.push((await lease.ready).url);
    lease.release();
  }
  assert.equal(new Set(urls).size, versions.length);
  assert.equal(requests.length, versions.length);
  assert.deepEqual(
    requests.map((r) => r[2]),
    ["original", "original", "original", "original", "preview", "preview"],
  );
});

test("idle LRU eviction respects byte limits while visible URLs stay pinned", async (t) => {
  const cache = new MediaPreviewCache(
    { mediaContent: async () => content("1234") },
    8,
  );
  t.after(() => cache.close());
  const pinned = cache.acquire(asset("pinned"));
  const value = await pinned.ready;
  const idle = cache.acquire(asset("idle"));
  const evicted = await idle.ready;
  idle.release();
  const fresh = cache.acquire(asset("fresh"));
  await fresh.ready;
  fresh.release();
  assert.equal(cache.peek(asset("idle")), undefined);
  assert.equal(cache.peek(asset("pinned")).url, value.url);
  await assert.rejects(fetch(evicted.url));
  assert.equal(await (await fetch(value.url)).text(), "1234");
  pinned.release();
  pinned.release();
});

test("entry limits and oversized images release idle blob URLs", async (t) => {
  const cache = new MediaPreviewCache(
    { mediaContent: async () => content("large") },
    4,
    1,
  );
  t.after(() => cache.close());
  const lease = cache.acquire(asset());
  const value = await lease.ready;
  assert.equal(cache.peek(asset()).url, value.url);
  lease.release();
  assert.equal(cache.peek(asset()), undefined);
  await assert.rejects(fetch(value.url));
  const limited = new MediaPreviewCache(
    { mediaContent: async () => content("x") },
    100,
    1,
  );
  t.after(() => limited.close());
  const one = limited.acquire(asset("one"));
  await one.ready;
  one.release();
  const two = limited.acquire(asset("two"));
  await two.ready;
  two.release();
  assert.equal(limited.peek(asset("one")), undefined);
  assert.ok(limited.peek(asset("two")));
});

test("recall discards in-flight bytes and cannot repopulate from a late response", async (t) => {
  const loading = deferred();
  let signal;
  const cache = new MediaPreviewCache({
    mediaContent: async (_a, _id, options) => {
      signal = options.signal;
      return loading.promise;
    },
  });
  t.after(() => cache.close());
  const lease = cache.acquire(asset());
  await Promise.resolve();
  cache.invalidateMessage("a", "m");
  assert.equal(signal.aborted, true);
  loading.resolve(content("recalled"));
  await assert.rejects(lease.ready, { name: "AbortError" });
  assert.equal(cache.peek(asset()), undefined);
  lease.release();
});

test("sign-out clears only its account and disconnect revokes the remaining URLs", async () => {
  const cache = new MediaPreviewCache({ mediaContent: async () => content() });
  const a = cache.acquire(asset()),
    b = cache.acquire(asset("photo", { accountId: "b" }));
  const av = await a.ready,
    bv = await b.ready;
  cache.clear("a");
  await assert.rejects(fetch(av.url));
  assert.equal(cache.peek(asset()), undefined);
  assert.equal(cache.peek(asset("photo", { accountId: "b" })).url, bv.url);
  cache.close();
  await assert.rejects(fetch(bv.url));
  assert.throws(() => cache.acquire(asset()), /not available/);
  a.release();
  b.release();
});

test("failed reads are retryable and advertised media never enters the preview cache", async (t) => {
  let calls = 0;
  const cache = new MediaPreviewCache({
    mediaContent: async () => {
      if (++calls === 1) throw Error("offline");
      return content();
    },
  });
  t.after(() => cache.close());
  assert.throws(
    () =>
      cache.acquire(
        asset("pending", { state: "pending", actualVariant: null }),
      ),
    /not available/,
  );
  const first = cache.acquire(asset());
  await assert.rejects(first.ready, /offline/);
  first.release();
  const second = cache.acquire(asset());
  await second.ready;
  second.release();
  assert.equal(calls, 2);
});
