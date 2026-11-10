// Seeds a fresh simulator hub through the same HTTP API the UI uses and the
// simulator's control port. Everything a scene sees is created here, in a fixed
// order, on a simulator clock that only moves when a scene advances it.
import { NativeWeChatClient } from "@agent-wechat/shared";
import { stageMiniProgram } from "../../../scripts/fixtures/native-miniprogram.mjs";
import { ACCOUNT, SPARE } from "./accounts.mjs";

export { ACCOUNT, SPARE };

const wait = (ms) => new Promise((done) => setTimeout(done, ms));

/** Polls `read` until `done(value)` holds. Short intervals; fails instead of hanging. */
async function until(what, read, done, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await read();
    if (done(value)) return value;
    if (Date.now() > deadline) throw new Error(`Timed out waiting for ${what}`);
    await wait(50);
  }
}

/**
 * What a scene's `setup` receives. `server` and `account` are the identifiers the
 * scene's route needs; the other members change the simulator.
 */
export function seeder(stack) {
  const client = new NativeWeChatClient({ baseUrl: stack.hub.endpoint, token: stack.hub.token });
  let keys = 0;
  const key = (purpose) => `scenes-${purpose}-${++keys}`;
  const sim = {
    client,
    server: stack.hub.serverId,
    account: ACCOUNT.id,
    /** Creates an account on the hub without signing it in. */
    async addAccount(account) {
      await client.createAccount(account);
    },
    /** Signs an account in the way a person does: scan the QR code, then approve on the phone. */
    async signIn(account = ACCOUNT) {
      const operation = await client.login(
        account.id,
        { strategy: "auto", timeoutSeconds: 900, qrRefresh: { mode: "auto", maxRefreshes: 3 } },
        key("login"),
      );
      await until(
        "the sign-in QR code",
        () => client.operation(account.id, operation.id),
        (current) => current.result?.qrAvailable === true,
      );
      await stack.control({ op: "login", account: account.id, status: "scanned" });
      await stack.control({ op: "login", account: account.id, status: "approved" });
      await until(
        "sign-in to finish",
        () => client.operation(account.id, operation.id),
        (current) => current.state === "succeeded",
      );
      // Groups can only be created from contacts the hub has already synced.
      await until(
        "the contacts to sync",
        async () => (await client.contacts(account.id)).items.map((item) => item.id),
        (ids) => ["sim_mei", "sim_daniel"].every((id) => ids.includes(id)),
      );
    },
    /**
     * Avatars the hub has fetched and cached, as the initials the UI shows until the
     * image arrives. Fetching takes the hub a moment and the UI polls for it, so a
     * scene that shows contacts warms them first and the spec waits for the images.
     */
    avatarInitials: [],
    /** Has the hub fetch and cache the avatars of contacts, so the UI's first request for each succeeds. */
    async warmAvatars(usernames, initials) {
      for (const username of usernames) {
        const operation = await client.refreshAvatar(ACCOUNT.id, username);
        await until(
          `the avatar of ${username}`,
          () => client.operation(ACCOUNT.id, operation.id),
          (current) => current.state === "succeeded",
        );
        await client.avatar(ACCOUNT.id, username);
      }
      sim.avatarInitials.push(...initials);
    },
    /**
     * Has the hub download the photos of a conversation, as the UI would on opening it, so the
     * UI finds them cached. Until then it shows a placeholder and polls the download.
     */
    async warmPhotos(thread) {
      for (const message of await sim.messages(thread)) {
        if (message.type !== 3) continue;
        const { items } = await client.mediaAssets(ACCOUNT.id, message.id);
        for (const asset of items.filter((item) => item.state === "pending")) {
          const job = await client.transferMedia(ACCOUNT.id, { messageId: message.id, assetId: asset.id, quality: asset.variant });
          const done = await until(
            "the photo to download",
            () => client.job(ACCOUNT.id, job.id),
            (current) => ["available", "accepted", "failed", "cancelled", "needs_review", "partial", "expired", "unsupported"].includes(current.state),
          );
          if (done.state !== "available") throw new Error(`Photo download ${done.state}`);
        }
      }
    },
    /** Moves the simulator clock, which stamps every message created afterwards. */
    advance: (seconds) => stack.control({ op: "advance", seconds }),
    /** Stage one app and deliver two cards, including a card without a cached package. */
    async miniPrograms(peer) {
      await stageMiniProgram(stack.dataDir, ACCOUNT.id);
      for (const available of [true, false]) {
        await stack.control({ op: "mini_program_card", account: ACCOUNT.id, peer, available });
        await sim.advance(1);
      }
      await sim.settled(peer, 2);
    },
    /**
     * A message from a contact; `self` makes it a message from the account itself. In a
     * group, `sender` names the contact who speaks.
     */
    inbound: (peer, body, { self = false, notify = true, sender } = {}) =>
      stack.control({ op: "inbound", account: ACCOUNT.id, peer, body, self_sent: self, notify, sender }),
    /** An image, file, video, voice note or sticker from a contact, or from `sender` in a group. */
    media: (peer, kind, { sender } = {}) =>
      stack.control({ op: "receive_media", account: ACCOUNT.id, peer, kind, sender }),
    /** Creates a group and returns its conversation ID. */
    async createGroup(name, members) {
      const operation = await client.createGroup(ACCOUNT.id, { name, members }, key("group"));
      const done = await until(
        "the group to be created",
        () => client.operation(ACCOUNT.id, operation.id),
        (current) => ["succeeded", "failed", "rejected", "cancelled"].includes(current.state),
      );
      if (done.state !== "succeeded" || typeof done.result?.groupId !== "string")
        throw new Error(`Group creation ${done.state}`);
      return done.result.groupId;
    },
    /** Waits until the hub lists the conversation with a message, since the hub learns of new messages by sync. */
    async settled(thread, minimum = 1) {
      return until(
        `${thread} to hold ${minimum} message(s)`,
        async () => (await client.messages(ACCOUNT.id, thread, { limit: 100 })).items,
        (items) => items.length >= minimum,
      );
    },
    /** Messages of a conversation, oldest first. */
    async messages(thread) {
      const page = await client.messages(ACCOUNT.id, thread, { limit: 100 });
      return [...page.items].reverse();
    },
  };
  return sim;
}
