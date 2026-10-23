import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { MediaOutbox } from "../packages/client-ui/src/media-outbox.ts";

export async function mediaJourney({ client, control, wait, state, account }) {
  const contents = new Map();
  for (const [kind, type, variant] of [
    ["image", 3, "standard"],
    ["file", 49, "original"],
    ["video", 43, "normal"],
    ["voice", 34, "normal"],
    ["sticker", 47, "original"],
  ]) {
    const before = new Set(
      (await client.messages(account, "sim_mei", { limit: 200 })).items.map(
        (message) => message.id,
      ),
    );
    const receiptCursor = (await client.threads(account, { limit: 1 }))
      .asOfEventCursor;
    await control({ op: "receive_media", account, peer: "sim_mei", kind });
    const message = await wait(`incoming ${kind} fixture`, async () =>
      (await client.messages(account, "sim_mei", { limit: 200 })).items.find(
        (message) => message.type === type && !before.has(message.id),
      ),
    );
    let automaticVoice;
    if (kind === "voice") {
      assert.ok(receiptCursor);
      // No UI, media discovery, or transfer request starts this download.
      automaticVoice = await wait(
        "daemon downloads received voice",
        async () => {
          const events = await client.events(account, receiptCursor, 200);
          for (const event of events.items.filter(
            (event) => event.kind === "media.ready",
          )) {
            const cached = await client.mediaAsset(account, event.data.assetId);
            if (
              cached.messageId === message.id &&
              cached.variant === "normal" &&
              cached.actualVariant === "normal" &&
              cached.state === "available"
            )
              return cached;
          }
        },
      );
      assert.ok(automaticVoice.jobId);
    }
    const assets = await client.mediaAssets(account, message.id);
    const asset =
      assets.items.find((asset) => asset.variant === variant) ??
      assets.items[0];
    assert.ok(asset, `missing ${kind} asset`);
    const transfer = automaticVoice
      ? await client.job(account, automaticVoice.jobId)
      : await client.transferMedia(account, {
          messageId: message.id,
          assetId: asset.id,
          quality: asset.variant,
        });
    const completed = await wait(`${kind} transfer`, async () => {
      const job = await client.job(account, transfer.id);
      assert.ok(
        !["failed", "cancelled"].includes(job.state),
        `${kind} transfer failed: ${job.reason}`,
      );
      return job.state === "available" && job;
    });
    assert.equal(completed.accountId, account);
    const cached = await client.mediaAsset(account, asset.id);
    const content = await client.mediaContent(account, asset.id);
    assert.equal(cached.state, "available");
    if (kind === "sticker") {
      assert.equal(cached.contentType, "image/gif");
      assert.equal(cached.animationFrames, 2);
      assert.equal(
        new TextDecoder().decode(content.bytes.slice(0, 6)),
        "GIF89a",
      );
    }
    assert.equal(
      createHash("sha256").update(content.bytes).digest("hex"),
      cached.sha256,
    );
    const ranged = await client.mediaContent(account, asset.id, {
      range: "bytes=0-15",
      maxBytes: 16,
    });
    assert.equal(ranged.status, 206);
    assert.deepEqual(ranged.bytes, content.bytes.slice(0, 16));
    await assert.rejects(client.mediaContent("work", asset.id));
    contents.set(kind, content);
  }
  assert.equal(
    new TextDecoder().decode(contents.get("voice").bytes.slice(0, 4)),
    "RIFF",
  );
  assert.equal(
    new TextDecoder().decode(contents.get("video").bytes.slice(4, 8)),
    "ftyp",
  );

  let saved = [];
  const storage = {
    read: () => saved,
    write: (items) => {
      saved = items;
    },
  };
  const outbox = new MediaOutbox(client, account, storage);
  try {
    const bigFile = new Uint8Array(2 * 1024 * 1024).fill(65);
    for (const kind of ["file", "image", "video", "voice"]) {
      const content = contents.get(kind);
      const file = new File(
        [kind === "file" ? bigFile : content.bytes],
        `outgoing.${kind === "image" ? "png" : kind === "video" ? "mp4" : kind === "voice" ? "wav" : "txt"}`,
        { type: content.contentType },
      );
      await outbox.send("sim_mei", file, kind);
      const pending = outbox.snapshot().at(-1);
      const completed = await wait(
        `outgoing ${kind} accepted`,
        () => {
          const item = outbox
            .snapshot()
            .find((item) => item.key === pending.key);
          assert.ok(
            !["failed", "cancelled", "needs_review"].includes(item.job?.state),
            `${kind} send failed: ${item.job?.reason ?? item.error}`,
          );
          return item.job?.state === "accepted" && item.job;
        },
        60000,
      );
      assert.equal(completed.retryable, false);
      const projected = await wait(`outgoing ${kind} history`, async () => {
        const job = await client.job(account, completed.id);
        return job.result?.messageIds?.length && job;
      });
      for (const id of projected.result.messageIds) {
        const message = await client.message(account, id);
        assert.equal(message.direction, "outgoing");
        const assets = await client.mediaAssets(account, id);
        assert.ok(assets.items.some((asset) => asset.state === "available"));
      }
      const lookedUp = await client.sendByKey(account, pending.key);
      assert.equal(lookedUp.id, completed.id);
    }
    await control({ op: "send_fault", account, fault: "reject" });
    await outbox.send(
      "sim_mei",
      new File(["rejected"], "rejected.txt"),
      "file",
    );
    const rejectedKey = outbox.snapshot().at(-1).key;
    await wait(
      "media rejection",
      () =>
        outbox.snapshot().find((item) => item.key === rejectedKey).job
          ?.state === "failed",
    );
    const before = (await state()).accounts.find(
      (item) => item.id === account,
    ).acceptedMediaSends;
    await control({ op: "send_fault", account, fault: "lost_acknowledgment" });
    await outbox.send(
      "sim_mei",
      new File(["uncertain"], "uncertain.txt"),
      "file",
    );
    const uncertainKey = outbox.snapshot().at(-1).key;
    await wait(
      "uncertain media outcome",
      () =>
        outbox.snapshot().find((item) => item.key === uncertainKey).job
          ?.state === "needs_review",
    );
    await outbox.check(uncertainKey);
    assert.equal(
      outbox.snapshot().find((item) => item.key === uncertainKey).job.retryable,
      false,
    );
    assert.equal(
      (await state()).accounts.find((item) => item.id === account)
        .acceptedMediaSends,
      before + 1,
    );
    outbox.close();
    const reopened = new MediaOutbox(client, account, storage);
    await wait("media receipts survive reload", () =>
      reopened
        .snapshot()
        .some(
          (item) =>
            item.key === uncertainKey && item.job?.state === "needs_review",
        ),
    );
    reopened.close();
    assert.equal(
      (await state()).accounts.find((item) => item.id === account)
        .acceptedMediaSends,
      before + 1,
    );
  } finally {
    outbox.close();
  }
}
