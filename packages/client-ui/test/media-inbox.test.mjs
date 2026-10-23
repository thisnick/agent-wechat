import test from "node:test";
import assert from "node:assert/strict";
import { IncomingMedia, MediaInbox } from "../src/media-inbox.ts";
const asset = (variant, state = "pending") => ({
  id: variant,
  accountId: "a",
  messageId: "m",
  variant,
  state,
  actualVariant: state === "available" ? variant : null,
  retryable: state === "pending",
});
const tick = () => new Promise((done) => setTimeout(done, 0));

test("conversation and viewer share discovery, transfer and progress without requesting originals automatically", async (t) => {
  let reads = 0,
    complete = false;
  const transfers = [];
  const model = new IncomingMedia(
    {
      mediaAssets: async () => {
        reads++;
        return {
          items: [
            asset("standard", complete ? "available" : "pending"),
            asset("original"),
          ],
        };
      },
      transferMedia: async (_account, input) => {
        transfers.push(input);
        return { id: "job", state: "downloading" };
      },
      job: async () => {
        complete = true;
        return { id: "job", state: "available" };
      },
    },
    "a",
    "m",
    0,
  );
  t.after(() => model.close());
  await Promise.all([model.load(), model.load()]);
  assert.equal(reads, 1);
  const standard = model.snapshot().assets[0];
  await Promise.all([model.retrieve(standard), model.retrieve(standard)]);
  assert.equal(transfers.length, 1);
  assert.equal(transfers[0].quality, "standard");
  assert.equal(model.snapshot().assets[0].state, "available");
  assert.equal(model.working(standard), false);
  model.ensure(model.snapshot().assets[0]);
  await tick();
  assert.equal(transfers.length, 1);
});
test("media notifications refresh only known matching assets, scoped to their account", async (t) => {
  const reads = [];
  const inbox = new MediaInbox({
    mediaAssets: async (account, message) => {
      reads.push([account, message]);
      return { items: [{ ...asset("standard"), id: `${account}:${message}` }] };
    },
  });
  t.after(() => inbox.close());
  const a = inbox.get("a", "m");
  assert.equal(inbox.get("a", "m"), a);
  assert.notEqual(inbox.get("b", "m"), a);
  await a.load();
  await inbox.get("b", "m").load();
  reads.length = 0;
  inbox.ready("a:m");
  await tick();
  assert.deepEqual(reads, [["a", "m"]]);
});
test("a transfer error does not loop automatic requests or restart on reopening the viewer", async (t) => {
  let calls = 0;
  const model = new IncomingMedia(
    {
      transferMedia: async () => {
        calls++;
        throw new Error("offline");
      },
    },
    "a",
    "m",
  );
  t.after(() => model.close());
  model.ensure(asset("standard"));
  await tick();
  model.ensure(asset("standard"));
  await tick();
  assert.equal(calls, 1);
  assert.match(model.snapshot().error, /reach the agent-wechat hub/);
  await model.retrieve(asset("standard"));
  assert.equal(calls, 2);
});
