import test from "node:test";
import assert from "node:assert/strict";
import type { WeChatClient } from "@agent-wechat/shared";
import { pollMedia } from "./media-poll.ts";

const image = (data?: string) => ({ type: "image" as const, data, format: "jpeg", filename: "photo.jpg" });

test("best image result returns its reported variant", async () => {
  const requested: Array<string | undefined> = [];
  const client = { getMedia: async (_chat: string, _id: number, quality?: string) => {
    requested.push(quality);
    return { ...image("bytes"), quality: "standard" as const };
  } } as WeChatClient;
  const result = await pollMedia(client, "peer", 42, "best", undefined, 2, 0);
  assert.equal(result?.quality, "standard");
  assert.deepEqual(requested, ["best"]);
});

test("older server falls back from rejected best to full then thumbnail", async () => {
  const requested: Array<string | undefined> = [];
  const client = { getMedia: async (_chat: string, _id: number, quality?: string) => {
    requested.push(quality);
    if (quality === "best") throw new Error("400 Bad Request: unknown variant `best`");
    return quality === "full" ? image() : image("preview");
  } } as WeChatClient;
  const result = await pollMedia(client, "peer", 42, "best", undefined, 2, 0);
  assert.equal(result?.quality, "thumbnail");
  assert.deepEqual(requested, ["best", "full", "thumbnail"]);
});

test("other media errors are not masked as compatibility fallbacks", async () => {
  const client = { getMedia: async () => { throw new Error("401 Unauthorized"); } } as unknown as WeChatClient;
  await assert.rejects(pollMedia(client, "peer", 42, "best", undefined, 2, 0), /401 Unauthorized/);
});
