import test from "node:test";
import assert from "node:assert/strict";
import { MediaOutbox } from "../src/media-outbox.ts";

const storage = () => {
  let value = [];
  return {
    read: () => value,
    write: (next) => {
      value = next;
    },
  };
};
const job = (state = "accepted", accountId = "a") => ({
  id: "job-1",
  accountId,
  kind: "send",
  state,
  phase: "completed",
  retryable: false,
  result: {},
});
const file = () => new File(["fixture"], "note.txt", { type: "text/plain" });
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
async function eventually(condition) {
  for (let index = 0; index < 100; index++) {
    if (condition()) return;
    await new Promise((done) => setTimeout(done, 10));
  }
  assert.ok(condition());
}

test("an uncertain media admission checks the same key without resubmitting", async (t) => {
  let submissions = 0,
    lookup;
  const outbox = new MediaOutbox(
    {
      upload: async () => ({ id: "source-1" }),
      sendMedia: async () => {
        submissions++;
        throw new Error("lost response");
      },
      sendByKey: async (account, key) => {
        lookup = { account, key };
        return job();
      },
    },
    "a",
    storage(),
  );
  t.after(() => outbox.close());
  await outbox.send("mei", file(), "file");
  const pending = outbox.snapshot()[0];
  assert.equal(pending.uncertain, true);
  await outbox.check(pending.key);
  assert.deepEqual(lookup, { account: "a", key: pending.key });
  assert.equal(submissions, 1);
  assert.equal(outbox.snapshot()[0].job.state, "accepted");
});
test("reload recovers only receipt metadata and does not upload or send again", async (t) => {
  const saved = storage();
  let submissions = 0,
    lookups = 0;
  const client = {
    upload: async () => ({ id: "source-1" }),
    sendMedia: async () => {
      submissions++;
      return job();
    },
    job: async () => {
      lookups++;
      return job();
    },
  };
  const first = new MediaOutbox(client, "a", saved);
  await first.send("mei", file(), "file");
  first.close();
  const reopened = new MediaOutbox(client, "a", saved);
  t.after(() => reopened.close());
  await eventually(() => lookups === 1);
  assert.equal(submissions, 1);
  assert.equal(saved.read()[0].sourceId, "source-1");
  assert.equal("bytes" in saved.read()[0], false);
});
test("late upload cancellation cannot submit an attachment", async (t) => {
  const late = deferred();
  let submissions = 0;
  const outbox = new MediaOutbox(
    {
      upload: () => late.promise,
      sendMedia: async () => {
        submissions++;
        return job();
      },
    },
    "a",
    storage(),
  );
  t.after(() => outbox.close());
  const send = outbox.send("mei", file(), "file");
  await outbox.cancel(outbox.snapshot()[0].key);
  late.resolve({ id: "source-1" });
  await send;
  assert.equal(submissions, 0);
  assert.equal(outbox.snapshot()[0].stage, "cancelled");
});
test("a receipt from another account is never installed", async (t) => {
  const outbox = new MediaOutbox(
    {
      upload: async () => ({ id: "source-1" }),
      sendMedia: async () => job("accepted", "b"),
    },
    "a",
    storage(),
  );
  t.after(() => outbox.close());
  await outbox.send("mei", file(), "file");
  assert.equal(outbox.snapshot()[0].job, undefined);
  assert.equal(outbox.snapshot()[0].uncertain, true);
});
test("needs-review media cannot be retried", async (t) => {
  let retries = 0;
  const outbox = new MediaOutbox(
    {
      upload: async () => ({ id: "source-1" }),
      sendMedia: async () => ({ ...job("needs_review"), retryable: true }),
      retryJob: async () => {
        retries++;
        return job();
      },
    },
    "a",
    storage(),
  );
  t.after(() => outbox.close());
  await outbox.send("mei", file(), "file");
  await outbox.retry(outbox.snapshot()[0].key);
  assert.equal(retries, 0);
});

test("image preparation failures explain the error without resending", async (t) => {
  for (const [reason, explanation] of [
    ["image_decode_limit", /Resize it or send it as a file/],
    ["image_decode_failed", /Try exporting it as JPEG or PNG/],
  ]) {
    let submissions = 0;
    const saved = storage();
    const client = {
      upload: async () => ({ id: "source-1" }),
      sendMedia: async () => {
        submissions++;
        return { ...job("failed"), reason };
      },
      job: async () => ({ ...job("failed"), reason }),
    };
    const first = new MediaOutbox(client, "a", saved);
    await first.send("mei", new File(["image"], "photo.jpg", { type: "image/jpeg" }), "image");
    assert.match(first.snapshot()[0].error, explanation);
    first.close();
    const reopened = new MediaOutbox(client, "a", saved);
    t.after(() => reopened.close());
    await reopened.check(reopened.snapshot()[0].key);
    assert.match(reopened.snapshot()[0].error, explanation);
    assert.equal(reopened.snapshot()[0].uncertain, false);
    assert.equal(submissions, 1);
  }
});
