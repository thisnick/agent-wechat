// The scenes the visual review captures: a name, the simulator data it needs, the
// route it opens and what to wait for. Plain data and functions with no browser
// dependency, so scripts/scene-listing.mjs can list them without Playwright.
//
// A scene opens `route` (a name from routeTable) with the parameters `target`
// returns, once `setup` has seeded the simulator. `ready` waits for the screen to be
// complete using roles and text; `actions` reaches states no address expresses.
// Screenshots are named `<scene>@<viewport>`, which is how Argos matches them to the
// baseline, so renaming a scene or a viewport shows up as removed plus added. Each
// scene is also captured in every translation as `<scene>@<viewport>@<language>`
// (see capture.spec.mjs); `ready` and `actions` stay in English.
import { ACCOUNT, SPARE } from "./accounts.mjs";
import { desktopBridge, LEFTOVER_HUB_SETTINGS } from "./desktop.mjs";
import { readFile } from "node:fs/promises";

/** Desktop, phone and compact phone sizes for visual review. */
export const viewports = {
  desktop: { width: 1280, height: 800 },
  narrow: { width: 390, height: 844 },
  compact: { width: 320, height: 700 },
};

/**
 * The browser clock for every scene, a quarter of an hour after the simulator's
 * fixed start (1700000000, 2023-11-14T22:13:20Z). Messages are stamped from the
 * simulator clock, so this makes "today" the day they were sent.
 */
export const FIXED_TIME = "2023-11-14T22:30:00Z";

/** Signs in the account and creates the conversations most scenes show. Returns the identifiers scenes need. */
async function standardWorld(sim) {
  await sim.addAccount(ACCOUNT);
  await sim.signIn(ACCOUNT);
  await sim.warmAvatars(["sim_mei", "sim_daniel"], ["MC", "DP"]);
  await sim.advance(120);
  await sim.inbound("sim_mei", "Are we still on for lunch tomorrow?");
  await sim.advance(45);
  await sim.inbound("sim_mei", "Yes, noon works. Where should we meet?", { self: true });
  await sim.advance(30);
  await sim.inbound("sim_mei", "I booked the place near the station");
  await sim.advance(20);
  await sim.media("sim_mei", "image");
  await sim.settled("sim_mei", 5);
  await sim.warmPhotos("sim_mei");
  await sim.advance(240);
  await sim.inbound("sim_daniel", "The draft is ready for review");
  await sim.advance(15);
  await sim.inbound("sim_daniel", "I left a few comments in the doc");
  await sim.settled("sim_daniel", 2);
  const group = await sim.createGroup("Weekend trip", ["sim_mei", "sim_daniel"]);
  await sim.advance(60);
  await sim.inbound(group, "Who is driving on Saturday?", { sender: "sim_mei" });
  await sim.advance(25);
  await sim.inbound(group, "I can take my car, it fits four", { sender: "sim_daniel" });
  await sim.advance(40);
  await sim.media(group, "image", { sender: "sim_mei" });
  await sim.settled(group, 4);
  await sim.warmPhotos(group);
  const mei = await sim.messages("sim_mei");
  return {
    mei: "sim_mei",
    daniel: "sim_daniel",
    group,
    booked: mei.find((message) => message.body === "I booked the place near the station").id,
    photo: mei.find((message) => message.type === 3).id,
  };
}

/** Real media from the docs gallery, staged through the composer's own file input. */
const composerFiles = {
  "afternoon-walk.jpg": "image/jpeg",
  "afternoon-walk.mp4": "video/mp4",
};
async function stageComposerFiles(page, names) {
  await page.locator("input.attachment-input").setInputFiles(await Promise.all(names.map(async (name) => ({
    name,
    mimeType: composerFiles[name],
    buffer: await readFile(new URL(`../../../docs/demo/gallery/fixtures/media/${name}`, import.meta.url)),
  }))));
}

/** A row of the chat or contact list, found by the conversation's name rather than its preview text. */
const row = (page, name) => page.locator(".chat-row").filter({ has: page.getByText(name, { exact: true }) });
/**
 * The sign-in code is random on every run; the countdown beside it is not. Masks
 * select by class, not text, so they also apply to the translated captures.
 */
const qr = (page) => [page.locator("img.login-qr")];
/** The hub address includes the port the scenes server happened to get. */
const hubAddress = (page) => [page.locator("#hub-address")];

// Synthetic request conversations for visual review; never contact the live service.
const requestSceneRows = [
  { id: "fr_scene_casey", peerId: "sim_casey", displayName: "Casey Morgan", direction: "incoming", message: "Hello, we met at the book club.", updatedAt: 1700000800, version: "scene-incoming-v1", isContact: false, actionable: true, attention: true, state: "request_received", history: [{ serverId: "123", message: "Hello, we met at the book club.", createdAt: 1700000800, direction: "incoming" }] },
  { id: "fr_scene_jordan", peerId: "sim_jordan", displayName: "Jordan Lee", direction: "outgoing", message: "Let's keep in touch.", updatedAt: 1700000700, version: "scene-outgoing-v1", isContact: false, actionable: false, attention: false, state: "request_sent", history: [{ serverId: "0", message: "Let's keep in touch.", createdAt: 1700000700, direction: "outgoing" }] },
];
async function prepareContactScenes({ page }) {
  await page.route("**/v1/accounts/*/contact-requests*", route => route.request().method() === "GET" ? route.fulfill({ json: { items: requestSceneRows, nextCursor: null, pendingIncomingCount: 1, sourceGeneration: "scene", coverage: {}, asOfEventCursor: "0" } }) : route.continue());
  await page.route("**/v1/accounts/*/search/contacts", route => route.fulfill({ json: { displayName: "avery.example", recipientRef: "scene-only-reference", expiresAt: 1700001400, source: "wechat", request: null, contactId: null } }));
}

export const scenes = [
  {
    name: "login-qr",
    route: "login",
    description: "Sign-in screen of an account that is not signed in, showing the QR code to scan.",
    viewports: ["desktop", "narrow"],
    setup: async (sim) => {
      await sim.addAccount(SPARE);
      return {};
    },
    target: () => ({ account: SPARE.id }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Sign in to WeChat" })).toBeVisible();
      await expect(page.getByRole("img", { name: /sign-in QR code/ })).toBeVisible();
    },
    mask: qr,
  },
  {
    name: "account-list",
    route: "accounts",
    description: "The accounts on a hub: one signed in and one that still needs a QR code.",
    viewports: ["desktop", "narrow"],
    setup: async (sim) => {
      await sim.addAccount(ACCOUNT);
      await sim.signIn(ACCOUNT);
      await sim.addAccount(SPARE);
      return {};
    },
    target: () => ({}),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Choose an account" })).toBeVisible();
      await expect(page.getByText(SPARE.alias)).toBeVisible();
      await expect(page.getByText("Open chats")).toBeVisible();
    },
  },
  {
    name: "chat-list",
    route: "chats",
    description: "The full inbox with category and conversation unread counts and the newest conversation open beside it.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      for (const name of ["Mei Chen", "Daniel Park", "Weekend trip"]) await expect(row(page, name)).toBeVisible();
      await expect(page.getByRole("button", { name: /^DMs.*6 unread$/ })).toBeVisible();
    },
  },
  {
    name: "chat-filters",
    route: "chats",
    description: "The conversation filter bar with All pinned beside the scrolling categories and unread badges for categories with new messages.",
    viewports: ["desktop", "compact"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      // The simulator has no official or service accounts. Add those rows at the
      // thread-list boundary while keeping the simulator's DM and group rows.
      await page.route(/\/threads(?:\?.*)?$/, async (route) => {
        const response = await route.fetch();
        if (!response.ok()) return route.fulfill({ response });
        const threads = await response.json();
        for (const [id, displayName, businessKind, businessType, unreadCount, text] of [
          ["gh_scene_library", "City Library", "official_account", 0, 3, "New events this weekend"],
          ["gh_scene_transit", "Metro Alerts", "service_account", 1, 12, "Your trip receipt is ready"],
        ]) {
          threads.items.push({
            id, peerId: id, accountId: ACCOUNT.id, displayName, unreadCount,
            kind: "direct", readStateVersion: "0", history: null,
            classification: { category: "business", namespace: "wechat", businessKind, businessType, openIm: null },
            lastMessage: { id: `${id}-1`, type: 1, text, senderId: id, senderName: displayName, direction: "incoming", createdAt: null },
          });
        }
        await route.fulfill({ response, json: threads });
      });
    },
    ready: async ({ page, expect }) => {
      for (const name of ["Mei Chen", "Daniel Park", "Weekend trip", "City Library", "Metro Alerts"])
        await expect(row(page, name)).toBeVisible();
    },
    actions: async ({ page, expect }) => {
      const bar = page.getByRole("group", { name: "Conversation categories" });
      const dm = bar.getByRole("button", { name: /^DMs/ });
      const group = bar.getByRole("button", { name: /^Groups/ });
      const official = bar.getByRole("button", { name: /^Official.*3 unread$/ });
      const service = bar.getByRole("button", { name: /^Service.*12 unread$/ });
      // The conversation initially open beside the list is marked read.
      const all = bar.getByRole("button", { name: "All conversations 21 unread", exact: true });
      await expect(all).toHaveAttribute("aria-pressed", "true");
      const library = row(page, "City Library");
      const metro = row(page, "Metro Alerts");
      const mei = row(page, "Mei Chen");
      const trip = row(page, "Weekend trip");

      await dm.click();
      await expect(all).toHaveAttribute("aria-pressed", "false");
      await expect(mei).toBeVisible();
      await expect(trip).toHaveCount(0);
      await expect(library).toHaveCount(0);
      await expect(metro).toHaveCount(0);
      await group.click();
      await expect(mei).toBeVisible();
      await expect(trip).toBeVisible();
      await page.reload();
      await expect(dm).toHaveAttribute("aria-pressed", "true");
      await expect(group).toHaveAttribute("aria-pressed", "true");
      await expect(trip).toBeVisible();
      await expect(library).toHaveCount(0);
      await all.click();
      await expect(dm).toHaveAttribute("aria-pressed", "false");
      await expect(group).toHaveAttribute("aria-pressed", "false");
      await expect(library).toBeVisible();
      await group.click();
      await group.click();
      await expect(all).toHaveAttribute("aria-pressed", "true");
      await expect(library).toBeVisible();
      await expect(metro).toBeVisible();

      const scroller = bar.locator(".filter-scroller");
      const right = bar.getByRole("button", { name: "Scroll categories right" });
      const left = bar.getByRole("button", { name: "Scroll categories left" });
      if (page.viewportSize().width === viewports.compact.width) {
        await expect(right).toBeHidden();
        await expect.poll(() => scroller.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeGreaterThan(0);
      } else {
        await page.setViewportSize({ ...viewports.desktop, width: 1100 });
        await expect(left).toBeHidden();
        await right.click();
        await expect(right).toBeHidden();
        await expect(left).toBeVisible();
        await expect(service).toBeInViewport();
        await page.setViewportSize(viewports.desktop);
      }
      await official.click();
      await expect(library).toBeVisible();
      await expect(metro).toHaveCount(0);
      await expect(mei).toHaveCount(0);
      await service.click();
      await expect(metro).toBeVisible();
      await expect(library).toBeVisible();
      await page.locator(".search-trigger").click();
      const search = page.getByRole("dialog", { name: "Search everything" });
      await expect(search).toBeVisible();
      await expect(search.getByRole("button", { name: "All", exact: true })).toHaveAttribute("aria-pressed", "true");
      await page.keyboard.press("Escape");
      await expect(official).toHaveAttribute("aria-pressed", "true");
      await expect(all).toHaveAttribute("aria-pressed", "false");
      await expect(library).toBeVisible();
      await expect(metro).toBeVisible();
    },
  },
  {
    name: "direct-chat",
    route: "chat",
    description: "A direct conversation with text from both sides and a photo.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.mei }),
    ready: async ({ page, expect }) => {
      await expect(page.getByText("I booked the place near the station")).toBeVisible();
      await expect(page.getByText("Yes, noon works. Where should we meet?")).toBeVisible();
      await expect(page.locator("img.media-preview")).toHaveCount(1);
    },
  },
  ...["composer-attachments", "composer-attachments-original"].map((name) => ({
    name,
    route: "chat",
    description: name === "composer-attachments"
      ? "A photo and a video staged in the message box with previews, sizes, remove buttons and the unchecked Original choice."
      : "Original checked for a staged photo and video, with help that explains what it adds for each.",
    viewports: ["desktop", "narrow", "compact"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.mei }),
    ready: async ({ page, expect }) => {
      await expect(page.getByText("I booked the place near the station")).toBeVisible();
    },
    actions: async ({ page, expect }) => {
      // Attachments open once the hub's capabilities and the conversation are loaded.
      await expect(page.getByRole("button", { name: "Attach file" })).toBeEnabled();
      await stageComposerFiles(page, ["afternoon-walk.jpg", "afternoon-walk.mp4"]);
      const tray = page.getByRole("region", { name: "Staged attachments" });
      await expect(tray.getByText("2 attachments")).toBeVisible();
      await expect(tray.getByRole("img", { name: "Staged image" })).toBeVisible();
      // Settling waits for images, not videos: wait for the first frame to show.
      await expect.poll(() => tray.locator("video").evaluate((video) => video.readyState)).toBeGreaterThanOrEqual(2);
      const original = tray.getByRole("checkbox", { name: "Original media quality" });
      await expect(original).not.toBeChecked();
      if (name === "composer-attachments") return;
      await original.check();
      // Focus, not hover: the screenshot moves the pointer away, which closes a hovered tip.
      await tray.getByRole("button", { name: "About original media quality" }).focus();
      await expect(page.getByRole("tooltip")).toContainText("full resolution");
      await expect(page.getByRole("tooltip")).toContainText("unchanged video file");
    },
  })),
  {
    name: "group-chat",
    route: "chat",
    description: "A group conversation where each message names its sender, ending in a photo.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.group }),
    ready: async ({ page, expect }) => {
      await expect(page.getByText("Who is driving on Saturday?")).toBeVisible();
      await expect(page.getByText("I can take my car, it fits four")).toBeVisible();
      await expect(page.locator("img.media-preview")).toHaveCount(1);
    },
  },
  {
    name: "unnamed-group-chat",
    route: "chat",
    description: "An unnamed group labeled with member names in the chat list and conversation header.",
    viewports: ["desktop", "narrow"],
    setup: async (sim) => {
      await sim.addAccount(ACCOUNT);
      await sim.signIn(ACCOUNT);
      await sim.warmAvatars(["sim_mei", "sim_daniel"], ["MC", "DP"]);
      const group = await sim.createGroup("", ["sim_mei", "sim_daniel"]);
      await sim.inbound(group, "Has everyone picked a time?", { sender: "sim_mei" });
      await sim.settled(group);
      return { group };
    },
    target: (world) => ({ account: ACCOUNT.id, thread: world.group }),
    ready: async ({ page, expect }) => {
      const title = "Group chat · Daniel Park, Mei Chen";
      await expect(page.getByRole("heading", { name: title, exact: true })).toBeVisible();
      if (page.viewportSize().width === viewports.desktop.width)
        await expect(row(page, title)).toBeVisible();
      await expect(page.locator(".workspace").getByText("Has everyone picked a time?", { exact: true })).toBeVisible();
    },
  },
  {
    name: "new-group-avatar",
    route: "chats",
    description: "A newly received group shows its room avatar while its member list is unavailable.",
    viewports: ["desktop"],
    setup: async (sim) => {
      await sim.addAccount(ACCOUNT);
      await sim.signIn(ACCOUNT);
      return {
        async receiveGroup() {
          const group = await sim.createGroup("Neighborhood group", ["sim_mei", "sim_daniel"]);
          await sim.advance(60);
          await sim.inbound(group, "See you at the park.", { sender: "sim_mei" });
          await sim.settled(group);
        },
      };
    },
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      await expect(row(page, "Mei Chen")).toBeVisible();
      await expect(row(page, "Neighborhood group")).toHaveCount(0);
    },
    actions: async ({ page, expect, world }) => {
      await page.route(/\/threads(?:\?.*)?$/, async (route) => {
        const response = await route.fetch();
        if (!response.ok()) return route.fulfill({ response });
        const threads = await response.json();
        // The hub can discover a room before its saved roster is available.
        for (const thread of threads.items) {
          if (thread.displayName === "Neighborhood group") thread.avatarMembers = [];
        }
        await route.fulfill({ response, json: threads });
      });
      await world.receiveGroup();
      const avatar = row(page, "Neighborhood group").locator(".group-avatar img");
      await expect(avatar).toBeVisible();
      await expect.poll(() => avatar.evaluate((image) => image.naturalWidth)).toBeGreaterThan(0);
    },
    // The simulator generates a different image color for each random room ID.
    mask: (page) => [row(page, "Neighborhood group").locator(".group-avatar img")],
  },
  {
    name: "quoted-sender-names",
    route: "chat",
    description: "Quoted replies use supplied author names, cached names for older replies, and You for own messages.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.group }),
    prepare: async ({ page }) => {
      // Synthetic API payloads exercise the browser's normal message-loading path.
      await page.route(/\/threads\/[^/]+\/messages(?:\?.*)?$/, async (route) => {
        const response = await route.fetch();
        if (!response.ok()) return route.fulfill({ response });
        const data = await response.json();
        const samples = [
          { senderId: "member@openim", senderName: "Example Member" },
          { senderId: "sim_mei" },
          { senderId: "sim_daniel", senderName: "Different supplied name", direction: "outgoing" },
        ];
        data.items = data.items.slice(0, 3).map((message, index) => ({
          ...message, type: 49, appMessageType: 57, body: `Reply ${index + 1}`,
          quote: { type: 1, state: "embedded", serverId: `9900${index}`, embeddedText: `Quoted excerpt ${index + 1}`, ...samples[index] },
        }));
        await route.fulfill({ response, json: data });
      });
    },
    ready: async ({ page, expect }) => {
      const authors = page.locator(".quote-preview > strong");
      await expect(authors).toHaveCount(3);
      for (const author of ["Example Member", "Mei Chen", "You"])
        await expect(authors.filter({ hasText: new RegExp(`^${author}$`) })).toHaveCount(1);
    },
  },
  {
    name: "message-highlight",
    route: "message",
    description: "A conversation opened at one message, which is highlighted.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.mei, message: world.booked }),
    ready: async ({ page, expect }) => {
      await expect(page.locator(".message.quote-target")).toHaveCount(1);
      await expect(page.locator(".message.quote-target")).toBeInViewport();
    },
  },
  {
    name: "media-viewer",
    route: "media",
    description: "The photo viewer open over a conversation, on the photo Mei sent.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.mei, message: world.photo }),
    ready: async ({ page, expect }) => {
      const viewer = page.getByRole("dialog", { name: "Media viewer" });
      await expect(viewer).toBeVisible();
      await expect(viewer.locator("img").first()).toBeVisible();
    },
  },
  {
    name: "channels-video-cards",
    route: "chat",
    description: "Received WeChat Channels cards with their cover, play button, publisher and length.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.mei }),
    prepare: async ({ page }) => {
      // Synthetic card and media payloads exercise the normal message and preview paths.
      const cover = await readFile(new URL("./fixtures/channels-cover.jpg", import.meta.url));
      const cards = [
        { description: "Sunset from the harbour ferry, all the way across #travel", sourceName: "Harbour Walks", durationSeconds: 96 },
        { description: "A three-minute noodle recipe for busy weeknights", sourceName: "Kitchen Notes", durationSeconds: 158 },
      ];
      const ids = new Map();
      const assets = (messageId, index) => [
        { id: `channels-cover-${index}`, accountId: ACCOUNT.id, messageId, kind: "image", variant: "thumbnail", actualVariant: "thumbnail",
          state: "available", contentType: "image/jpeg", byteLength: cover.length, sha256: "c".repeat(64), filename: "link-preview",
          expiresAt: null, reason: null, retryable: false, jobId: null, autoDownloadAllowed: true,
          contentUrl: `/v1/accounts/${ACCOUNT.id}/media/channels-cover-${index}/content`, officialWriteback: "unsupported" },
        { id: `channels-video-${index}`, accountId: ACCOUNT.id, messageId, kind: "video", variant: "normal", actualVariant: null,
          state: "pending", contentType: null, byteLength: null, sha256: null, filename: "channels-video.mp4", expiresAt: null,
          reason: null, retryable: true, jobId: null, autoDownloadAllowed: false, contentUrl: null, officialWriteback: "unsupported" },
      ];
      await page.route(/\/threads\/[^/]+\/messages(?:\?.*)?$/, async (route) => {
        const response = await route.fetch();
        if (!response.ok()) return route.fulfill({ response });
        const data = await response.json();
        data.items = data.items.map((message, index) => {
          if (index >= cards.length) return message;
          const card = cards[index];
          ids.set(message.id, index);
          return { ...message, type: 49, appMessageType: 51, quote: null,
            body: `[Channels video] ${card.sourceName}: ${card.description}`,
            link: { title: null, url: null, ...card } };
        });
        await route.fulfill({ response, json: data });
      });
      await page.route(/\/media\/lookup$/, async (route) => {
        const { messageIds } = route.request().postDataJSON();
        if (!messageIds.some((id) => ids.has(id))) return route.fallback();
        const response = await route.fetch();
        const data = response.ok() ? await response.json() : { items: messageIds.map((messageId) => ({ messageId, assets: [], error: null })) };
        data.items = data.items.map((item) => ids.has(item.messageId) ? { messageId: item.messageId, assets: assets(item.messageId, ids.get(item.messageId)), error: null } : item);
        await route.fulfill({ json: data });
      });
      await page.route(/\/messages\/[^/]+\/media(?:\?.*)?$/, async (route) => {
        const messageId = decodeURIComponent(new URL(route.request().url()).pathname.split("/").at(-2));
        if (!ids.has(messageId)) return route.fallback();
        await route.fulfill({ json: { items: assets(messageId, ids.get(messageId)) } });
      });
      await page.route(/\/media\/channels-cover-\d+\/content(?:\?.*)?$/, (route) => route.fulfill({ body: cover, contentType: "image/jpeg" }));
    },
    ready: async ({ page, expect }) => {
      const cards = page.locator(".channels-video-card");
      await expect(cards).toHaveCount(2);
      await expect(page.getByText("WeChat Channels · Harbour Walks", { exact: true })).toBeVisible();
      await expect(page.getByText("WeChat Channels · Kitchen Notes", { exact: true })).toBeVisible();
      await expect(cards.locator(".shared-link-duration")).toHaveText(["2:38", "1:36"]);
      await expect(cards.locator(".shared-link-art img")).toHaveCount(2);
    },
  },
  {
    name: "chat-info",
    route: "info",
    description: "Details of a group conversation with its pin, mute and group settings.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.group }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("complementary", { name: "Conversation details" })).toBeVisible();
      await expect(page.getByText("Group settings")).toBeVisible();
    },
    // The conversation ID of a group is random.
    mask: (page) => [page.locator(".peer-id")],
  },
  {
    name: "omni-progressive-search", route: "contacts",
    description: "Contacts opens prefiltered; fast contact results remain usable while message search is pending or fails.",
    viewports: ["desktop", "compact"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: prepareContactScenes,
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      let release;
      const gate = new Promise(resolve => { release = resolve; });
      let messageStarted = false;
      await page.route("**/v1/accounts/*/messages?*", async route => {
        messageStarted = true;
        await gate;
        await route.fulfill({ status: 503, json: { error: { code: "unavailable" } } });
      });
      await page.locator(".search-trigger").click();
      const search = page.getByRole("dialog", { name: "Search everything" });
      await expect(search.getByRole("button", { name: "Friends", exact: true })).toHaveAttribute("aria-pressed", "true");
      await search.getByRole("button", { name: "All", exact: true }).click();
      await search.getByRole("combobox").fill("Mei");
      try {
        await expect.poll(() => messageStarted).toBe(true);
        await expect(search.locator("[cmdk-item]").filter({ hasText: "Mei Chen" })).toBeVisible();
      } finally { release(); }
      await expect(search.getByRole("alert")).toBeVisible();
      await expect(search.locator("[cmdk-item]").filter({ hasText: "Mei Chen" })).toBeVisible();
    },
  },
  ...["friend-requests", "friend-request-review", "omni-search", "omni-message-search", "add-contact"].map(name => ({
    name,
    route: name.startsWith("friend-request") ? "friend-requests" : "contacts",
    description: ({ "friend-requests": "Incoming and sent request history with an actionable Contacts badge.", "friend-request-review": "The shared contact card with request history, privacy choices, acceptance and local-history deletion.", "omni-search": "One search surface for saved contacts, groups, messages and request history.", "omni-message-search": "Message search limits long snippets to two lines while keeping the conversation name, original timestamp and View action visible.", "add-contact": "The shared contact card with a greeting and privacy choices before sending a friend request." })[name],
    viewports: name === "omni-message-search" ? ["desktop", "compact"] : ["desktop", "narrow", "compact"],
    setup: standardWorld,
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      if (name === "omni-message-search") await page.route("**/v1/accounts/*/messages?*", async route => {
        const response = await route.fetch();
        const body = await response.json();
        body.items = body.items.map(message => message.body === "I booked the place near the station" ? {
          ...message, body: message.body + ". " + "We can meet there before we walk to the cafe and discuss the weekend plans together. ".repeat(8),
        } : message);
        await route.fulfill({ response, json: body });
      });
    },
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      if (name.startsWith("friend-request")) await expect(page.getByText("Casey Morgan", { exact: true })).toBeVisible();
      else await expect(row(page, "Mei Chen")).toBeVisible();
    },
    actions: async ({ page, expect }) => {
      if (name === "friend-request-review") {
        await page.getByRole("button", { name: "Review request", exact: true }).click();
        await expect(page.getByRole("dialog", { name: "Contact details" })).toBeVisible();
      } else if (name === "omni-search" || name === "omni-message-search" || name === "add-contact") {
        await page.locator(".search-trigger").focus();
        const search = page.getByRole("dialog", { name: "Search everything" });
        await expect(search).toBeVisible();
        if (name === "omni-message-search") {
          await search.getByRole("button", { name: "Messages", exact: true }).click();
          await search.getByRole("combobox").fill("station");
          const match = search.locator("[cmdk-item]").filter({ hasText: "I booked the place near the station" });
          await expect(match).toContainText("Mei Chen");
          await expect(match.locator("time")).toBeVisible();
          await expect(match.locator("time")).toHaveAttribute("datetime", /2023-11-14T/);
        }
        if (name === "add-contact") {
          await search.getByRole("button", { name: "Other people", exact: true }).click();
          await search.getByRole("combobox").fill("avery.example");
          await search.locator("[cmdk-item]").filter({ hasText: "Add contact" }).click();
          const add = page.getByRole("dialog", { name: "Contact details" });
          await expect(add).toBeVisible();
          await add.getByRole("textbox", { name: "Friend request message" }).fill("Hello, we met at the book club.");
        }
      }
    },
  })),
  ...[true, false].map(known => ({
    name: known ? "existing-contact-search" : "contact-search-without-reference",
    route: "contacts",
    description: known ? "A server match without a verification ticket opens the saved contact with Message." : "A server match without request material offers View and cannot send a friend request.",
    viewports: ["desktop", "compact"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route("**/v1/accounts/*/search/contacts", route => route.fulfill({ json: { displayName: "mei.example", recipientRef: null, expiresAt: 1700001400, source: "wechat", request: null, contactId: known ? "sim_mei" : null } }));
    },
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.locator(".search-trigger").focus();
      await page.getByRole("dialog", { name: "Search everything" }).getByRole("button", { name: "Other people", exact: true }).click();
      await page.getByRole("combobox").fill("mei.example");
      const match = page.locator("[cmdk-item]").filter({ hasText: "mei.example" });
      await expect(match).toContainText(known ? "Message" : "View");
      await expect(page.locator("[cmdk-item]").filter({ hasText: "Add contact" })).toHaveCount(0);
      if (!known) {
        await match.click();
        const dialog = page.getByRole("dialog", { name: "Contact details" });
        await expect(dialog).toContainText("This result has no friend-request reference.");
        await expect(dialog.getByRole("button", { name: "Send request", exact: true })).toHaveCount(0);
      }
    },
  })),
  {
    name: "contact-request-retry",
    route: "contacts",
    description: "A lost submission response keeps the original key and greeting until an inspected retry completes.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      const attempts = [];
      let sent = false;
      const sentRow = { id: "fr_scene_sent", peerId: "opaque_alias@stranger", displayName: "opaque_alias@stranger", direction: "outgoing", message: "Hello, we met at the book club.", updatedAt: 1700000800, version: "scene-sent-v1", isContact: false, actionable: false, attention: false, state: "request_sent", history: [{ serverId: "0", message: "Hello, we met at the book club.", createdAt: 1700000800, direction: "outgoing" }] };
      await page.route("**/v1/accounts/*/search/contacts", route => route.fulfill({ json: { displayName: "avery.example", recipientRef: "scene-only-reference", expiresAt: 1700001400, source: "wechat", request: sent ? sentRow : null, contactId: null } }));
      await page.route("**/v1/accounts/*/contact-mutations/by-idempotency-key?*", route => route.fulfill({ status: 404, json: { error: { code: "not_found" } } }));
      await page.route("**/v1/accounts/*/contact-requests", async route => {
        if (route.request().method() !== "POST") return route.fulfill({ json: { items: [...requestSceneRows, ...(sent ? [sentRow] : [])], nextCursor: null, pendingIncomingCount: 1, sourceGeneration: "scene", coverage: {}, asOfEventCursor: "0" } });
        if (route.request().postDataJSON().privacy?.hideMyPosts !== true) throw Error("Send omitted the chosen privacy");
        attempts.push({ key: route.request().headers()["idempotency-key"], body: route.request().postData() });
        if (attempts.length === 1) return route.abort("failed");
        if (attempts.length !== 2 || JSON.stringify(attempts[0]) !== JSON.stringify(attempts[1])) throw Error("Request retry changed its original intent");
        sent = true;
        return route.fulfill({ status: 202, json: { id: "scene-send", accountId: ACCOUNT.id, kind: "contact_mutation", state: "succeeded", result: { action: "send_request", remoteOutcome: "accepted", profileProjection: "complete" }, createdAt: FIXED_TIME, updatedAt: FIXED_TIME } });
      });
    },
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.locator(".search-trigger").focus();
      await page.getByRole("dialog", { name: "Search everything" }).getByRole("button", { name: "Other people", exact: true }).click();
      await page.getByRole("combobox").fill("avery.example");
      await page.locator("[cmdk-item]").filter({ hasText: "Add contact" }).click();
      const dialog = page.getByRole("dialog", { name: "Contact details" });
      await dialog.getByRole("textbox", { name: "Friend request message" }).fill("Hello, we met at the book club.");
      await dialog.getByRole("switch", { name: "Hide My Posts", exact: true }).check();
      await dialog.getByRole("button", { name: "Send request", exact: true }).click();
      await expect(dialog.getByRole("alert")).toBeVisible();
      await expect(dialog.getByRole("button", { name: "Send request", exact: true })).toHaveCount(0);
      await dialog.getByRole("button", { name: "Check status", exact: true }).click();
      await dialog.getByRole("button", { name: "Retry original request", exact: true }).click();
      await expect(dialog.getByText("Request sent", { exact: true })).toBeVisible();
      await dialog.getByRole("button", { name: "Close", exact: true }).last().click();
      await expect(page.getByRole("combobox")).toHaveValue("avery.example");
      const sentResult = page.locator("[cmdk-item]").filter({ hasText: "Request sent" });
      await expect(sentResult).toContainText("View");
      await expect(page.locator("[cmdk-item]").filter({ hasText: "Add contact" })).toHaveCount(0);
      await sentResult.click();
      await expect(page.getByRole("dialog", { name: "Contact details" })).toBeVisible();
    },
  },
  ...["omni-section-pagination", "omni-pagination-query-change"].map(name => ({
    name, route: "contacts",
    description: "Each search section loads its own cursor, keeps earlier results on retry, deduplicates pages and shows message timestamps; query changes discard late pages.",
    viewports: ["desktop", "compact"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: prepareContactScenes,
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      const reads = [];
      let releaseMessages, releaseStale;
      const messageGate = new Promise(resolve => { releaseMessages = resolve; });
      const staleGate = new Promise(resolve => { releaseStale = resolve; });
      let groupRetries = 0, messagePages = 0, stalePages = 0;
      const person = (id, relation, displayName) => ({ id, username: id, displayName, relation });
      const message = (id, body, createdAt) => ({ id, accountId: ACCOUNT.id, threadId: "sim_mei", body, createdAt, serverId: "123" });
      await page.route(/\/v1\/accounts\/[^/]+\/(contacts|groups|messages)\?.*/, async route => {
        const url = new URL(route.request().url()), query = url.searchParams.get("query"), cursor = url.searchParams.get("cursor");
        if (query !== "needle" && query !== "fresh") return route.continue();
        const resource = url.pathname.split("/").at(-1), relation = url.searchParams.get("relation");
        reads.push({ resource, relation, cursor, query });
        let items = [], nextCursor = null;
        if (query === "fresh") {
          if (resource === "messages") items = [message("fresh", "Fresh query result", null)];
        } else if (resource === "contacts" && relation === "friend") {
          items = [person("friend-one", "friend", "Needle Friend One")];
          if (cursor) { expect(cursor).toBe("friends-next"); items.push(person("friend-two", "friend", "Needle Friend Two")); }
          else nextCursor = "friends-next";
        } else if (resource === "contacts" && relation === "non_contact") {
          items = [person("other-one", "non_contact", "Needle Person One")];
        } else if (resource === "contacts" && relation === "unknown") {
          if (cursor) { expect(cursor).toBe("unknown-next"); items = [person("other-three", "unknown", "Needle Person Three")]; }
          else { items = [person("other-two", "unknown", "Needle Person Two")]; nextCursor = "unknown-next"; }
        } else if (resource === "groups") {
          items = [{ id: "needle-one@chatroom", displayName: "Needle Group One" }];
          if (cursor) {
            expect(cursor).toBe("groups-next");
            if (++groupRetries === 1) return route.fulfill({ status: 503, json: { error: { code: "unavailable" } } });
            items = [{ id: "needle-two@chatroom", displayName: "Needle Group Two" }];
          } else nextCursor = "groups-next";
        } else if (resource === "messages") {
          if (cursor === "messages-next") {
            messagePages++;
            await messageGate;
            items = [message("message-one", "First message match", "2023-11-14T20:03:00Z"), message("message-two", "Second message match", "1700000000"), message("message-no-time", "Message with no timestamp", null)];
            nextCursor = "messages-third";
          } else if (cursor === "messages-third") {
            stalePages++;
            await staleGate;
            items = [message("stale", "Stale page must disappear", null)];
          } else {
            expect(cursor).toBeNull();
            items = [message("message-one", "First message match", "2023-11-14T20:03:00Z")]; nextCursor = "messages-next";
          }
        }
        await route.fulfill({ json: { items, nextCursor } });
      });
      try {
        await page.locator(".search-trigger").click();
        const search = page.getByRole("dialog", { name: "Search everything" });
        await expect(search.getByRole("button", { name: "Friends", exact: true })).toHaveAttribute("aria-pressed", "true");
        await search.getByRole("button", { name: "All", exact: true }).click();
        await search.getByRole("combobox").fill("needle");
        const section = label => search.getByRole("group", { name: label, exact: true });
        const friends = section("Friends"), groups = section("Groups"), messages = section("Messages"), others = section("Other people");
        await expect(friends.locator("[cmdk-item]")).toHaveCount(1);
        await expect(groups.locator("[cmdk-item]")).toHaveCount(1);
        await expect(others.locator("[cmdk-item]")).toHaveCount(2);
        await expect(messages.locator("time")).toHaveAttribute("datetime", "2023-11-14T20:03:00.000Z");
        await expect(messages.locator("time")).toHaveText(/Nov 14, 2023.*8:03 PM/);
        const otherReads = reads.filter(read => read.resource !== "contacts").length;
        await friends.getByRole("button", { name: "Load more Friends", exact: true }).click();
        await expect(friends.locator("[cmdk-item]")).toHaveCount(2);
        await expect(friends.getByRole("button", { name: "Load more Friends", exact: true })).toHaveCount(0);
        expect(reads.filter(read => read.resource !== "contacts").length).toBe(otherReads);
        await others.getByRole("button", { name: "Load more Other people", exact: true }).click();
        await expect(others.locator("[cmdk-item]")).toHaveCount(3);
        await groups.getByRole("button", { name: "Load more Groups", exact: true }).click();
        await expect(groups.getByRole("alert")).toBeVisible();
        await expect(groups.locator("[cmdk-item]")).toHaveCount(1);
        await expect(friends.locator("[cmdk-item]")).toHaveCount(2);
        await groups.getByRole("button", { name: "Load more Groups", exact: true }).click();
        await expect(groups.locator("[cmdk-item]")).toHaveCount(2);
        await expect(groups.getByRole("alert")).toHaveCount(0);
        await messages.getByRole("button", { name: "Load more Messages", exact: true }).click();
        await expect.poll(() => messagePages).toBe(1);
        await expect(messages.getByRole("button", { name: "Load more Messages", exact: true })).toBeDisabled();
        await expect(friends.locator("[cmdk-item]")).toHaveCount(2);
        releaseMessages();
        await expect(messages.locator("[cmdk-item]")).toHaveCount(3);
        await expect(messages.locator("[cmdk-item]").filter({ hasText: "Second message match" }).locator("time")).toHaveAttribute("datetime", "2023-11-14T22:13:20.000Z");
        await expect(messages.locator("[cmdk-item]").filter({ hasText: "Message with no timestamp" }).locator("time")).toHaveCount(0);
        expect(messagePages).toBe(1);
        if (name === "omni-pagination-query-change") {
          await messages.getByRole("button", { name: "Load more Messages", exact: true }).click();
          await expect.poll(() => stalePages).toBe(1);
          await search.getByRole("combobox").fill("fresh");
          await expect(messages).toContainText("Fresh query result");
          releaseStale();
          await expect(search.locator("[cmdk-item]").filter({ hasText: "Stale page must disappear" })).toHaveCount(0);
          await expect(search.locator("[cmdk-item]").filter({ hasText: "First message match" })).toHaveCount(0);
          expect(reads.filter(read => read.query === "fresh").every(read => read.cursor === null)).toBe(true);
        }
      } finally { releaseMessages(); releaseStale(); }
    },
  })),
  ...["contact-search-saved-only", "contact-search-other-people"].map(name => ({
    name, route: "contacts",
    description: "Empty search shows saved contacts; typing adds a separate Other people section with clear source/membership labels and a separate WeChat discovery result.",
    viewports: ["desktop", "compact"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route(/\/v1\/accounts\/[^/]+\/contacts(?:\?.*)?$/, async route => {
        const response = await route.fetch();
        const body = await response.json();
        const url = new URL(route.request().url());
        const unknown = { id: "sim_mei", username: "sim_mei", displayName: "Mei Chen", relation: "unknown" };
        const nonContact = { id: "sim_outsider", username: "sim_outsider", displayName: "Mei Rivera", relation: "non_contact" };
        const relation = url.searchParams.get("relation");
        const query = url.searchParams.get("query") || "";
        if (relation === "unknown") body.items = query === "Mei" ? [unknown] : [];
        else if (relation === "non_contact") body.items = query === "Mei" ? [nonContact] : [];
        else {
          body.items = body.items.map(row => row.id === "sim_mei" ? unknown : row);
          body.items.push(nonContact);
        }
        await route.fulfill({ response, json: body });
      });
    },
    ready: async ({ page, expect }) => { await expect(row(page, "Daniel Park")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      const reads = [];
      page.on("request", request => {
        const url = new URL(request.url());
        if (/\/contacts$/.test(url.pathname)) reads.push(url);
      });
      await page.locator(".search-trigger").click();
      const search = page.getByRole("dialog", { name: "Search everything" });
      for (const scope of ["Friends", "All"]) {
        await search.getByRole("button", { name: scope, exact: true }).click();
        await expect(search.locator("[cmdk-item]").filter({ hasText: "Daniel Park" })).toContainText("Contact");
        await expect(search.locator("[cmdk-item]").filter({ hasText: "Mei Chen" })).toHaveCount(0);
        await expect(search.locator("[cmdk-item]").filter({ hasText: "Mei Rivera" })).toHaveCount(0);
      }
      expect(reads.length).toBeGreaterThan(0);
      expect(reads.every(url => url.searchParams.get("relation") === "friend")).toBe(true);
      await search.getByRole("combobox").fill("Mei");
      const otherPeople = search.getByRole("group", { name: "Other people", exact: true });
      const unknown = otherPeople.locator("[cmdk-item]").filter({ hasText: "Mei Chen" });
      const nonContact = otherPeople.locator("[cmdk-item]").filter({ hasText: "Mei Rivera" });
      await expect(unknown).toContainText("From local history");
      await expect(nonContact).toContainText("Not friends yet");
      await expect(unknown.getByRole("button", { name: "Message", exact: true })).toBeVisible();
      expect(new Set(reads.map(url => url.searchParams.get("relation")))).toEqual(new Set(["friend", "non_contact", "unknown"]));
      await unknown.getByRole("button", { name: "Details for Mei Chen", exact: true }).click();
      const cachedCard = page.getByRole("dialog", { name: "Contact details" });
      await expect(cachedCard.getByRole("button", { name: "Add contact", exact: true })).toBeVisible();
      await expect(cachedCard.getByRole("radio")).toHaveCount(0);
      await cachedCard.getByRole("button", { name: "Close", exact: true }).click();
      await expect(otherPeople).toBeVisible();
      await search.getByRole("button", { name: "Friends", exact: true }).click();
      await expect(otherPeople).toHaveCount(0);
      await expect(search.getByRole("group", { name: "Add someone on WeChat", exact: true })).toHaveCount(0);
      await search.getByRole("button", { name: "Other people", exact: true }).click();
      await expect(otherPeople).toBeVisible();
      await expect(search.getByRole("group", { name: "Friends", exact: true })).toHaveCount(0);
      if (name === "contact-search-other-people") return;
      await search.getByRole("combobox").fill("avery.example");
      const discovery = search.getByRole("group", { name: "Add someone on WeChat", exact: true });
      await expect(discovery.locator("[cmdk-item]")).toContainText("avery.example");
      await expect(discovery.locator("[cmdk-item]")).toContainText("Add contact");
      await expect(search).not.toContainText("On WeChat");
      await expect(search).not.toContainText("Saved contact");
      await discovery.locator("[cmdk-item]").click();
      const card = page.getByRole("dialog", { name: "Contact details" });
      await expect(card.getByRole("textbox", { name: "Friend request message" })).toBeVisible();
      await expect(card.locator(".contact-detail-identity p")).toHaveCount(0);
    },
  })),
  ...["unknown-contact", "unknown-contact-add", "non-contact"].map(name => ({
    name, route: "info",
    description: "A cached person opens from conversation details; confirmed non-contacts say Not friends yet, unknown membership has no label, and Add contact opens request review without submitting.",
    viewports: ["desktop", "compact"], setup: standardWorld,
    target: () => ({ account: ACCOUNT.id, thread: "sim_mei" }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route(/\/v1\/accounts\/[^/]+\/contacts(?:\?.*)?$/, async route => {
        const response = await route.fetch();
        const body = await response.json();
        body.items = body.items.map(row => row.id === "sim_mei" ? { ...row, relation: name === "non-contact" ? "non_contact" : "unknown" } : row);
        await route.fulfill({ response, json: body });
      });
    },
    ready: async ({ page, expect }) => { await expect(page.getByRole("complementary", { name: "Conversation details" })).toBeVisible(); },
    actions: async ({ page, expect }) => {
      let submits = 0, lookups = 0, privacyReads = 0;
      page.on("request", request => {
        const path = new URL(request.url()).pathname;
        if (path.endsWith("/contact-requests") && request.method() === "POST") submits++;
        if (path.endsWith("/search/contacts")) lookups++;
        if (path.endsWith("/privacy")) privacyReads++;
      });
      await page.getByRole("complementary", { name: "Conversation details" }).getByRole("button", { name: "Details", exact: true }).click();
      const card = page.getByRole("dialog", { name: "Contact details" });
      await expect(card.getByText("Contact", { exact: true })).toHaveCount(0);
      await expect(card.getByText("On WeChat", { exact: true })).toHaveCount(0);
      await expect(card.locator(".contact-detail-identity p")).toHaveCount(name === "non-contact" ? 1 : 0);
      if (name === "non-contact") await expect(card.locator(".contact-detail-identity p")).toHaveText("Not friends yet");
      await expect(card.getByRole("radio")).toHaveCount(0);
      await expect(card.getByRole("button", { name: "Remove contact", exact: true })).toHaveCount(0);
      await expect(card.getByRole("button", { name: "Message", exact: true })).toBeVisible();
      await expect(card.getByRole("button", { name: "Add contact", exact: true })).toBeVisible();
      if (name === "unknown-contact-add") {
        await card.getByRole("button", { name: "Add contact", exact: true }).click();
        await expect(card.getByRole("textbox", { name: "Friend request message" })).toBeVisible();
        await expect(card.getByRole("radio", { name: "Full access", exact: true })).toBeVisible();
        await expect(card.getByRole("button", { name: "Send request", exact: true })).toBeDisabled();
        expect(lookups).toBe(1);
      }
      expect(submits).toBe(0);
      expect(privacyReads).toBe(0);
    },
  })),
  ...["contact-details", "contact-details-search", "contact-privacy-saved", "contact-privacy-unavailable", "contact-privacy-retry", "contact-privacy-syncing"].map(name => ({
    name, route: "contacts",
    description: ({ "contact-details": "A saved contact card reached from the contact list by its keyboard-accessible Details action.", "contact-details-search": "Saved-contact search offers separate Message and Details actions; Details opens the shared card.", "contact-privacy-saved": "Moments selections survive Chats Only and save with the original privacy version.", "contact-privacy-unavailable": "Missing administrator permission disables privacy edits and submission.", "contact-privacy-retry": "An uncertain privacy save locks edits and retries only the same inspected intent.", "contact-privacy-syncing": "One accepted PATCH waits through pending canonical GETs before showing saved settings, without another mutation." })[name],
    viewports: name === "contact-privacy-retry" ? ["desktop"] : ["desktop", "compact"],
    setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      let privacy = { chatsOnly: false, hideMyPosts: false, hideTheirPosts: true };
      const attempts = [];
      let readbackReads = 0;
      await page.route("**/v1/accounts/*/contacts/sim_mei/privacy", async route => {
        if (route.request().method() !== "GET") throw Error("Privacy reads must use GET");
        if (name === "contact-privacy-unavailable") return route.fulfill({ status: 403, json: { error: { code: "forbidden" } } });
        if (name === "contact-privacy-syncing" && attempts.length) {
          if (++readbackReads === 1) return route.fulfill({ status: 409, json: { error: { code: "contact_privacy_pending" } } });
          await new Promise(resolve => setTimeout(resolve, 600));
        }
        return route.fulfill({ json: { privacy, version: attempts.length ? "scene-privacy-v2" : "scene-privacy-v1" } });
      });
      await page.route("**/v1/accounts/*/contacts/sim_mei", async route => {
        if (route.request().method() !== "PATCH") throw Error("Contact changes must use PATCH");
        if (name === "contact-privacy-unavailable") throw Error("Unavailable privacy must not submit a contact change");
        const body = route.request().postDataJSON();
        const expectedHide = name !== "contact-privacy-syncing";
        if (body.expectedVersion !== "scene-privacy-v1" || body.privacy.chatsOnly !== false || body.privacy.hideMyPosts !== expectedHide || body.privacy.hideTheirPosts !== expectedHide) throw Error("Privacy submission lost the selected settings or source version");
        attempts.push({ key: route.request().headers()["idempotency-key"], body: route.request().postData() });
        if (name === "contact-privacy-retry" && attempts.length === 1) return route.abort("failed");
        if (attempts.length > 1 && JSON.stringify(attempts[0]) !== JSON.stringify(attempts[1])) throw Error("Privacy retry changed its original intent");
        privacy = body.privacy;
        return route.fulfill({ status: 202, json: { id: "scene-privacy", accountId: ACCOUNT.id, kind: "contact_mutation", state: "succeeded", result: { action: "update_privacy" }, createdAt: FIXED_TIME, updatedAt: FIXED_TIME } });
      });
      await page.route("**/v1/accounts/*/contact-mutations/by-idempotency-key?*", route => route.fulfill({ status: 404, json: { error: { code: "not_found" } } }));
    },
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      if (name === "contact-details-search") {
        await page.locator(".search-trigger").focus();
        const search = page.getByRole("dialog", { name: "Search everything" });
        const match = search.locator("[cmdk-item]").filter({ hasText: "Mei Chen" });
        await expect(match.getByRole("button", { name: "Message", exact: true })).toBeVisible();
        await match.getByRole("button", { name: "Details for Mei Chen", exact: true }).click();
        await expect(search).toHaveCount(0);
      } else {
        const details = page.getByRole("button", { name: "Details for Mei Chen", exact: true });
        await details.focus();
        await expect(details).toBeVisible();
        await details.press("Enter");
      }
      const dialog = page.getByRole("dialog", { name: "Contact details" });
      await expect(dialog).toBeVisible();
      await expect(dialog.getByRole("button", { name: "Message", exact: true })).toBeVisible();
      if (name === "contact-privacy-unavailable") {
        await expect(dialog.getByRole("alert")).toContainText("Administrator access is required");
        await expect(dialog.getByRole("radio", { name: "Full access", exact: true })).toHaveCount(0);
        await expect(dialog.getByRole("button", { name: "Save privacy", exact: true })).toHaveCount(0);
        await expect(dialog.getByRole("button", { name: "Remove contact", exact: true })).toHaveCount(0);
        return;
      }
      const hideMine = dialog.getByRole("switch", { name: "Hide My Posts", exact: true });
      await expect(dialog.getByRole("switch", { name: "Hide Their Posts", exact: true })).toBeChecked();
      await expect(dialog.getByRole("button", { name: "Save privacy", exact: true })).toHaveCount(0);
      if (name === "contact-privacy-saved" || name === "contact-privacy-retry" || name === "contact-privacy-syncing") {
        let patchCount = 0;
        page.on("request", request => { if (request.method() === "PATCH" && new URL(request.url()).pathname.endsWith("/contacts/sim_mei")) patchCount++; });
        if (name === "contact-privacy-syncing") await dialog.getByRole("switch", { name: "Hide Their Posts", exact: true }).uncheck();
        else await hideMine.check();
        await dialog.getByRole("radio", { name: "Chats Only", exact: true }).check();
        await expect(hideMine).toHaveCount(0);
        await dialog.getByRole("radio", { name: "Full access", exact: true }).check();
        if (name === "contact-privacy-syncing") await expect(hideMine).not.toBeChecked();
        else await expect(hideMine).toBeChecked();
        await dialog.getByRole("button", { name: "Save privacy", exact: true }).click();
        if (name === "contact-privacy-syncing") {
          await expect(dialog.locator(".contact-change-status")).toContainText("Waiting for saved privacy settings");
          await expect(hideMine).toBeDisabled();
          await expect(dialog.getByRole("button", { name: "Save privacy", exact: true })).toHaveCount(0);
          await expect(dialog.getByRole("alert")).toHaveCount(0);
        }
        if (name === "contact-privacy-retry") {
          await expect(dialog.getByRole("alert")).toBeVisible();
          await expect(hideMine).toBeDisabled();
          await expect(dialog.getByRole("button", { name: "Save privacy", exact: true })).toBeDisabled();
          await dialog.getByRole("button", { name: "Check status", exact: true }).click();
          await dialog.getByRole("button", { name: "Retry original request", exact: true }).click();
        }
        await expect(dialog.locator(".contact-change-status")).toContainText("Privacy settings saved");
        if (name === "contact-privacy-syncing") {
          await expect(hideMine).not.toBeChecked();
          await expect(dialog.getByRole("switch", { name: "Hide Their Posts", exact: true })).not.toBeChecked();
          await expect(hideMine).toBeEnabled();
          await expect(dialog.getByRole("alert")).toHaveCount(0);
          expect(patchCount).toBe(1);
        } else await expect(hideMine).toBeChecked();
      }
    },
  })),
  {
    name: "friend-request-privacy-accept", route: "friend-requests",
    description: "Acceptance submits the selected privacy settings together with the exact request version.",
    viewports: ["desktop", "compact"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      let accepted = false;
      const selected = { chatsOnly: true, hideMyPosts: true, hideTheirPosts: false };
      await page.route("**/v1/accounts/*/contact-requests?*", route => route.fulfill({ json: { items: requestSceneRows.map(r => r.id === "fr_scene_casey" && accepted ? { ...r, isContact: true, actionable: false, version: "scene-accepted-v2" } : r), nextCursor: null, pendingIncomingCount: accepted ? 0 : 1 } }));
      await page.route("**/v1/accounts/*/contacts/sim_casey/privacy", route => route.fulfill({ json: { privacy: selected, version: "scene-privacy-v1" } }));
      await page.route("**/v1/accounts/*/contact-requests/fr_scene_casey/accept", route => {
        const body = route.request().postDataJSON();
        if (body.expectedVersion !== "scene-incoming-v1" || JSON.stringify(body.privacy) !== JSON.stringify({ chatsOnly: true, hideMyPosts: true, hideTheirPosts: false })) throw Error("Acceptance omitted the chosen privacy or current version");
        accepted = true;
        return route.fulfill({ status: 202, json: { id: "scene-accept", accountId: ACCOUNT.id, kind: "contact_mutation", state: "succeeded", result: { action: "accept_request" }, createdAt: FIXED_TIME, updatedAt: FIXED_TIME } });
      });
    },
    ready: async ({ page, expect }) => { await expect(page.getByText("Casey Morgan", { exact: true })).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.getByRole("button", { name: "Review request", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "Contact details" });
      await dialog.getByRole("switch", { name: "Hide My Posts", exact: true }).check();
      await dialog.getByRole("radio", { name: "Chats Only", exact: true }).check();
      await dialog.getByRole("button", { name: "Accept request", exact: true }).click();
      await expect(dialog.locator(".contact-change-status")).toContainText("Request accepted");
      await expect(dialog.getByRole("button", { name: "Accept request", exact: true })).toHaveCount(0);
      await expect(dialog.getByRole("button", { name: "Message", exact: true })).toBeVisible();
      await expect(dialog.getByRole("radio", { name: "Chats Only", exact: true })).toBeChecked();
      await expect(dialog.getByRole("radio", { name: "Full access", exact: true })).toBeEnabled();
    },
  },
  {
    name: "contact-details-sidebar", route: "info",
    description: "A direct-conversation sidebar opens the same contact card and its checked privacy/removal actions.",
    viewports: ["desktop"], setup: standardWorld, target: () => ({ account: ACCOUNT.id, thread: "sim_mei" }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route("**/v1/accounts/*/contacts/sim_mei/privacy", route => route.fulfill({ json: { privacy: { chatsOnly: false, hideMyPosts: false, hideTheirPosts: true }, version: "scene-privacy-v1" } }));
    },
    ready: async ({ page, expect }) => { await expect(page.getByRole("complementary", { name: "Conversation details" })).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.getByRole("complementary", { name: "Conversation details" }).getByRole("button", { name: "Details", exact: true }).click();
      const dialog = page.getByRole("dialog", { name: "Contact details" });
      await expect(dialog.getByRole("switch", { name: "Hide Their Posts", exact: true })).toBeChecked();
      await expect(dialog.getByRole("button", { name: "Message", exact: true })).toBeVisible();
      await dialog.locator(".contact-more summary").click();
      await expect(dialog.getByRole("button", { name: "Remove contact", exact: true })).toBeVisible();
    },
  },
  {
    name: "contact-details-remove", route: "contacts",
    description: "Remove contact opens a distinct confirmation that preserves chat history and recovers an uncertain result with the original key.",
    viewports: ["desktop", "compact"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route("**/v1/accounts/*/contacts/sim_mei/privacy", route => route.fulfill({ json: { privacy: { chatsOnly: false, hideMyPosts: false, hideTheirPosts: false }, version: "scene-privacy-v1" } }));
      await page.route("**/v1/accounts/*/contact-mutations/by-idempotency-key?*", route => route.fulfill({ status: 404, json: { error: { code: "not_found" } } }));
      const attempts = [];
      await page.route("**/v1/accounts/*/contacts/sim_mei", route => {
        if (route.request().method() !== "DELETE") return route.continue();
        attempts.push({ key: route.request().headers()["idempotency-key"], body: route.request().postData() });
        if (attempts.length === 1) return route.abort("failed");
        if (attempts.length !== 2 || JSON.stringify(attempts[0]) !== JSON.stringify(attempts[1])) throw Error("Remove contact retry changed the original intent");
        return route.fulfill({ status: 202, json: { id: "scene-remove", accountId: ACCOUNT.id, kind: "contact_mutation", state: "succeeded", result: { action: "remove_contact" }, createdAt: FIXED_TIME, updatedAt: FIXED_TIME } });
      });
    },
    ready: async ({ page, expect }) => { await expect(row(page, "Mei Chen")).toBeVisible(); },
    actions: async ({ page, expect }) => {
      const details = page.getByRole("button", { name: "Details for Mei Chen", exact: true });
      await details.focus(); await details.press("Enter");
      const card = page.getByRole("dialog", { name: "Contact details" });
      await card.locator(".contact-more summary").click();
      await card.getByRole("button", { name: "Remove contact", exact: true }).click();
      const confirmation = page.getByRole("dialog", { name: "Remove contact", exact: true });
      await expect(confirmation).toContainText("Your chat history will be kept.");
      await expect(confirmation.getByRole("button", { name: "Delete local history", exact: true })).toHaveCount(0);
      await confirmation.getByRole("button", { name: "Remove contact", exact: true }).click();
      await expect(confirmation.getByRole("alert")).toBeVisible();
      await expect(confirmation.getByRole("button", { name: "Remove contact", exact: true })).toHaveCount(0);
      await confirmation.getByRole("button", { name: "Check status", exact: true }).click();
      await confirmation.getByRole("button", { name: "Retry original request", exact: true }).click();
      await expect(confirmation.getByRole("status")).toContainText("Contact removed");
    },
  },
  {
    name: "friend-request-delete-history", route: "friend-requests",
    description: "Delete local history removes the request conversation without presenting it as a server decline.",
    viewports: ["desktop"], setup: standardWorld, target: () => ({ account: ACCOUNT.id }),
    prepare: async ({ page }) => {
      await prepareContactScenes({ page });
      await page.route("**/v1/accounts/*/contact-requests/fr_scene_casey", route => {
        if (route.request().method() !== "DELETE" || route.request().postDataJSON().expectedVersion !== "scene-incoming-v1") throw Error("History deletion lost its request version");
        return route.fulfill({ status: 202, json: { id: "scene-delete", accountId: ACCOUNT.id, kind: "contact_mutation", state: "succeeded", result: { action: "delete_request" }, createdAt: FIXED_TIME, updatedAt: FIXED_TIME } });
      });
    },
    ready: async ({ page, expect }) => { await expect(page.getByText("Casey Morgan", { exact: true })).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.getByRole("button", { name: "Review request", exact: true }).click();
      const card = page.getByRole("dialog", { name: "Contact details" });
      await card.locator(".contact-more summary").click();
      page.once("dialog", async confirmation => {
        expect(confirmation.message()).toContain("This does not decline the request on WeChat.");
        await confirmation.accept();
      });
      await card.getByRole("button", { name: "Delete local history", exact: true }).click();
      await expect(card.getByRole("status")).toContainText("Local history deleted");
      await expect(card.getByRole("button", { name: "Accept request", exact: true })).toHaveCount(0);
    },
  },
  {
    name: "contacts",
    route: "contacts",
    description: "The contact list of an account.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Contacts" })).toBeVisible();
      await expect(row(page, "Mei Chen")).toBeVisible();
    },
  },
  {
    name: "new-chat",
    route: "new-chat",
    description: "The dialog for starting a direct conversation from the contact list.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      const dialog = page.getByRole("dialog", { name: "New chat" });
      await expect(dialog).toBeVisible();
      await expect(dialog.getByText("Daniel Park")).toBeVisible();
    },
  },
  {
    name: "new-group",
    route: "new-chat",
    description: "The new chat dialog on its New group tab, with two contacts chosen.",
    viewports: ["desktop"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    actions: async ({ page, expect }) => {
      const dialog = page.getByRole("dialog", { name: "New chat" });
      await dialog.getByRole("tab", { name: "New group" }).click();
      await dialog.getByLabel("Select Mei Chen").check();
      await dialog.getByLabel("Select Daniel Park").check();
      await expect(dialog.getByLabel("Select Daniel Park")).toBeChecked();
    },
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("dialog", { name: "New chat" })).toBeVisible();
    },
  },
  {
    name: "settings",
    route: "settings",
    description: "Settings for the app, the hub and the signed-in account.",
    viewports: ["desktop", "narrow"],
    setup: standardWorld,
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
      await expect(page.getByText("Automatic downloads")).toBeVisible();
    },
    // The hub address carries the scenes server's port, and the cache totals depend on what was opened.
    mask: (page) => [page.getByText(/^http:\/\/127\.0\.0\.1:\d+$/), page.locator(".cache-usage")],
  },
  {
    name: "hub-picker",
    route: "servers",
    // The app leaves the hub picker's address when it starts with a hub open, so open
    // the hub first and then go to the picker in the app, as a user would.
    start: "accounts",
    description: "The screen for choosing which hub to connect to.",
    viewports: ["desktop", "narrow"],
    setup: async (sim) => {
      await sim.addAccount(ACCOUNT);
      await sim.signIn(ACCOUNT);
      return {};
    },
    target: () => ({}),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Connect to an agent-wechat hub" })).toBeVisible();
    },
    mask: hubAddress,
  },
  {
    name: "desktop-leftover-hub",
    route: "servers",
    description: "The desktop app's first screen when a hub left running by an earlier session holds the local port.",
    viewports: ["desktop", "narrow"],
    setup: async () => ({}),
    prepare: ({ page }) =>
      desktopBridge(page, {
        bootstrapError: LEFTOVER_HUB_SETTINGS.localHubError,
        settings: LEFTOVER_HUB_SETTINGS,
      }),
    target: () => ({}),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Welcome to AgentWechat" })).toBeVisible();
      await expect(page.getByRole("button", { name: "Stop leftover hub" })).toBeVisible();
    },
  },
  {
    name: "desktop-settings",
    route: "servers",
    description: "Desktop settings, including whether this device's hub keeps running while another hub is used.",
    viewports: ["desktop", "narrow"],
    setup: async () => ({}),
    prepare: ({ page }) =>
      desktopBridge(page, {
        bootstrapError: LEFTOVER_HUB_SETTINGS.localHubError,
        settings: LEFTOVER_HUB_SETTINGS,
      }),
    target: () => ({}),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Welcome to AgentWechat" })).toBeVisible();
    },
    actions: async ({ page, expect }) => {
      await page.getByRole("button", { name: "Desktop settings" }).click();
      await expect(page.getByRole("heading", { name: "Desktop settings" })).toBeVisible();
      await expect(page.getByRole("checkbox", { name: "Keep this device’s hub running" })).toBeVisible();
      await expect(page.getByRole("button", { name: "Stop leftover hub" })).toBeVisible();
    },
  },
];

// Synthetic media responses exercise the real picker, dialogs and composer.
// Native protocol/profile acceptance is covered independently in the isolated lab.
async function stickerFixtures(page, cold = false) {
  await page.route(/\/capabilities$/, async route => {const response=await route.fetch();await route.fulfill({response,json:{...await response.json(),stickers:true}});});
  const gif = await readFile(new URL('../../native-client-rust/crates/wechat-simulator/fixtures/sticker.gif', import.meta.url));
  const ids = Array.from({ length: cold ? 48 : 8 }, (_, index) => String(index + 1).padStart(32, '0'));
  const asset = id => ({ id: `sticker-${id}`, accountId: ACCOUNT.id, messageId: null, kind: 'sticker', variant: 'original', actualVariant: 'original', state: 'available', contentType: 'image/gif', byteLength: gif.length, sha256: 'a'.repeat(64), filename: `${id}.gif`, retryable: false, contentUrl: `/v1/accounts/${ACCOUNT.id}/media/sticker-${id}/content`, officialWriteback: 'complete' });
  const pack = (id, name, added) => ({ id, name, description: 'A little encouragement for everyday conversations.', membership: added ? 'added' : 'not_added', contentReadiness: 'available', eligible: true, stickerCount: 8, icon: asset(id), cover: null });
  const packs = [pack('pack.cats', 'Cozy cats', true), pack('pack.vibes', 'Good vibes', true), pack('pack.hello', 'Little hellos', false)];
  const downloaded = new Set();
  const saved = { items: ids.slice(0, cold ? 48 : 6).map(id => ({ id, caption: null, original: cold ? null : asset(id) })), revision: 'b'.repeat(64), freshness: 'profile' };
  const imports = new Map();
  await page.route(/\/jobs\/scene-add(?:-\d+)?$/, route => {
    const job = imports.get(new URL(route.request().url()).pathname.split('/').pop());
    const id = job.stickerId;
    saved.items = [{ id, caption: job.caption, original: asset(id) }, ...saved.items.filter(item => item.id !== id)];
    saved.revision = 'c'.repeat(64);
    return route.fulfill({json:{id:job.id,kind:'sticker_saved',state:'available',result:{profileWriteback:'complete'}}});
  });
  await page.route(/\/custom-stickers(?:\/[^/?]+)?(?:\?.*)?$/, route => {
    const tail = new URL(route.request().url()).pathname.split('/custom-stickers')[1];
    if (route.request().method() === 'POST') {
      const request = route.request();
      const preparationId = request.postDataJSON().source.preparationId;
      if (tail || !/^prep-scene(?:-\d+)?$/.test(preparationId) || !request.headers()['idempotency-key']) return route.fulfill({status:400,json:{error:{code:'invalid_sticker_source'}}});
      if ([...imports.values()].some(job => job.preparationId === preparationId)) throw Error('The UI submitted a successful import twice');
      const sequence = imports.size + 1, id = sequence === 1 ? 'scene-add' : `scene-add-${sequence}`;
      imports.set(id, {id, preparationId, stickerId:String(100 + sequence).padStart(32, '0'), caption:sequence === 1 ? 'Imported sticker' : `Imported sticker ${sequence}`});
      return route.fulfill({status:202,json:{id,kind:'sticker_saved',state:'queued'}});
    }
    if(route.request().method()==='PATCH') {
      const moving=tail.slice(1),after=route.request().postDataJSON().position.afterId;
      const item=saved.items.find(item=>item.id===moving);const rest=saved.items.filter(item=>item.id!==moving);
      rest.splice(after===null ? 0 : rest.findIndex(item=>item.id===after)+1,0,item);saved.items=rest;
      return route.fulfill({status:202,json:{id:'scene-move',kind:'sticker_saved',state:'available',result:{profileWriteback:'complete'}}});
    }
    const item = saved.items.find(item => item.id === tail.slice(1));
    return route.fulfill({ json: tail ? {...item, original: !cold || downloaded.has(item.id) ? asset(item.id) : null} : saved });
  });
  await page.route(/\/sticker-packs(?:\/[^/?]+(?:\/stickers)?)?(?:\?.*)?$/, route => {
    const url = new URL(route.request().url()), rest = url.pathname.split('/sticker-packs')[1];
    if (rest.endsWith('/stickers')) { const packId = rest.split('/')[1]; return route.fulfill({ json: { packId, items: ids.map((id, index) => ({ id, packId, ordinal: index + 1, caption: ['Hello!', 'Thank you', 'You got this', 'Nice work', 'Yes!', 'Good night', 'Please', 'Cheers'][index], captions: [], original: cold ? null : asset(id), thumbnail: cold ? {...asset(id),id:`sticker_preview_${id}`,state:downloaded.has(`sticker_preview_${id}`) ? "available" : "pending"} : null })) } }); }
    if (rest) return route.fulfill({ json: packs.find(pack => pack.id === rest.slice(1)) });
    return route.fulfill({ json: { items: url.searchParams.get('membership') === 'added' ? packs.filter(pack => pack.membership === 'added') : packs, nextCursor: null, coverage: url.searchParams.has('membership') ? 'account_added' : 'available_feed' } });
  });
  await page.route(/\/sticker-downloads$/, route => {
    const input=route.request().postDataJSON();downloaded.add(input.assetId || input.stickerId);
    return route.fulfill({status:202,json:{id:'scene-download',state:'available'}});
  });
  await page.route(/\/media\/sticker_preview_[^/]+$/, route => route.fulfill({json:{...asset('preview'),id:new URL(route.request().url()).pathname.split('/').pop()}}));
  await page.route(/\/media\/sticker(?:-|_preview_)[^/]+\/content(?:\?.*)?$/, route => route.fulfill({ body: gif, contentType: 'image/gif' }));
}

// Files the import scenes drop, and what the mocked hub knows about each. Files with
// a `file` are real fixtures the UI measures itself; the PNGs are drawn in the page.
const stickerGif = '../../native-client-rust/crates/wechat-simulator/fixtures/sticker.gif';
const importSources = {
  'afternoon-walk.mp4': { kind: 'video', type: 'video/mp4', width: 1280, height: 858, durationMs: 4000, file: '../../../docs/demo/gallery/fixtures/media/afternoon-walk.mp4' },
  'photo.png': { kind: 'image', type: 'image/png', width: 800, height: 600 },
  'poster.png': { kind: 'image', type: 'image/png', width: 1600, height: 1200 },
  'wave-1.gif': { kind: 'gif', type: 'image/gif', width: 96, height: 96, durationMs: 1000, file: stickerGif },
  'wave-2.gif': { kind: 'gif', type: 'image/gif', width: 96, height: 96, durationMs: 1000, file: stickerGif },
};

/**
 * Uploads and preparations that follow the hub's rules closely enough to show real
 * sizes: images keep their size unless shrunk, video defaults to 240 px and 10 fps, and
 * results over 1 MiB, 200 frames or 10 s fail with the hub's codes. Returns the requests.
 */
async function stickerPreparations(page) {
  const gif = await readFile(new URL(stickerGif, import.meta.url));
  const requests = [];
  await page.route(/\/uploads$/, route => {
    const name = /filename="([^"]+)"/.exec(route.request().postDataBuffer()?.toString('latin1') ?? '')?.[1] ?? 'unknown';
    return route.fulfill({ json: { id: `source-${name}`, name, mediaType: importSources[name]?.type ?? 'application/octet-stream', byteLength: 1, sha256: 'd'.repeat(64), createdAt: '2026-01-01T00:00:00Z' } });
  });
  await page.route(/\/sticker-preparations$/, route => {
    const input = route.request().postDataJSON();
    requests.push(input);
    const id = requests.length === 1 ? 'prep-scene' : `prep-scene-${requests.length}`;
    const fail = reason => route.fulfill({ json: { id, kind: 'sticker_prepare', state: 'failed', phase: 'preparing', reason, retryable: false } });
    const source = importSources[input.sourceId.replace(/^source-/, '')];
    const video = source.kind === 'video', longest = Math.max(source.width, source.height);
    if (!video && input.fps !== null) return fail('sticker_option_unsupported');
    if (!video && input.maxEdge === null && longest > 1024) return fail('sticker_adjustment_required');
    const scale = Math.min(1, (input.maxEdge ?? (video ? 240 : longest)) / longest);
    const width = Math.round(source.width * scale), height = Math.round(source.height * scale);
    const durationMs = input.trim?.durationMs ?? source.durationMs ?? 0;
    const frames = video ? Math.ceil((input.fps ?? 10) * durationMs / 1000) : source.kind === 'gif' ? 2 : 1;
    if (frames > 200 || durationMs > 10_000) return fail('sticker_adjustment_required');
    const byteLength = source.kind === 'gif' && scale === 1 && !input.trim ? gif.length : Math.round(width * height * frames * (video ? 0.12 : 0.6));
    if (byteLength > 1024 * 1024) return fail('sticker_output_size_limit');
    const asset = { id: `sticker-prepared-${id}`, state: 'available', contentType: 'image/gif', sha256: 'a'.repeat(64), byteLength };
    return route.fulfill({ json: { id, kind: 'sticker_prepare', state: 'available', result: { id, sourceId: input.sourceId, original: asset, preview: asset, contactSheet: asset, width, height, durationMs, transformations: [], warnings: [], expiresAt: 9999999999 } } });
  });
  return requests;
}

async function dropStickerFiles(page, names) {
  const files = await Promise.all(names.map(async name => {
    const source = importSources[name];
    return { name, type: source.type, width: source.width, height: source.height, encoded: source.file ? (await readFile(new URL(source.file, import.meta.url))).toString('base64') : null };
  }));
  await page.locator('.sticker-tray').evaluate(async (tray, files) => {
    const transfer = new DataTransfer();
    for (const file of files) {
      let blob;
      if (file.encoded) blob = new Blob([Uint8Array.from(atob(file.encoded), c => c.charCodeAt(0))]);
      else {
        const canvas = new OffscreenCanvas(file.width, file.height), context = canvas.getContext('2d');
        context.fillStyle = '#9fd3b8'; context.fillRect(0, 0, file.width, file.height);
        blob = await canvas.convertToBlob({ type: 'image/png' });
      }
      transfer.items.add(new File([blob], file.name, { type: file.type }));
    }
    tray.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }));
  }, files);
}

for (const mode of ['picker', 'edit', 'dragging', 'packs', 'pack-detail', 'import', 'import-adjust', 'import-saved', 'import-retry', 'lazy-loading', 'loading']) {
  scenes.push({
    name: `stickers-${mode}`,
    route: 'chat',
    description: { loading: 'Added pack contents show loading until the response arrives, with empty state only after an empty response.', 'lazy-loading': 'Missing Saved originals and pack thumbnails load on visibility, including after scrolling.', picker: 'Composer-anchored Saved picker with labeled added-pack tabs.', edit: 'Saved edit mode stays inside the composer tray with removal, visible drag feedback, Escape cancellation and keyboard placement.', dragging: 'Holding a Saved sticker shows its existing image under the pointer, dims the source and marks its drop position without staging a send.', packs: 'Official pack browser shows added and available packs in one dialog.', 'pack-detail': 'Official pack detail has complete contents, captions, membership and a copyable ID.', import: 'Add to Saved shows each prepared file with its size, bytes and length, a size slider with a phone-size mark, a note when an image is shrunk to 1024 px, and smoothness, start and length for video, with one primary Add action.', 'import-adjust': 'Moving a slider prepares again after a pause, returns to an earlier setting without preparing it again, keeps Add off until the row matches its controls, and explains a result too large to send.','import-saved': 'A completed asynchronous import closes the dialog and selects Saved without reopening the picker or resubmitting the item.', 'import-retry': 'A partial import keeps its per-file results open; retry submits only the remaining item before returning to Saved.' }[mode],
    viewports: ['desktop', 'narrow', 'compact'],
    preservePointer: mode === 'dragging',
    setup: standardWorld,
    target: world => ({ account: ACCOUNT.id, thread: world.mei }),
    prepare: async ({ page }) => stickerFixtures(page, mode === "lazy-loading"),
    ready: async ({ page, expect }) => { await expect(page.getByText('I booked the place near the station')).toBeVisible(); },
    actions: async ({ page, expect }) => {
      await page.getByRole('button', { name: 'Stickers', exact: true }).click();
      await expect(page.getByRole('region', { name: 'Sticker picker' })).toBeVisible();
      await expect(page.getByRole('tab', { name: 'Cozy cats' })).toBeVisible();
      if (mode === 'loading') {
        let finish;
        const response = new Promise(resolve => { finish = resolve; });
        await page.route(/\/sticker-packs\/pack.cats\/stickers(?:\?.*)?$/, async route => {
          await response;
          await route.fulfill({ json: { packId: 'pack.cats', items: [] } });
        });
        await page.getByRole('tab', { name: 'Cozy cats' }).click();
        await expect(page.getByRole('region', { name: 'Sticker picker' }).getByRole('status')).toHaveText('Loading stickers…');
        await expect(page.getByText('No stickers here yet')).toHaveCount(0);
        finish();
        await expect(page.getByRole('region', { name: 'Sticker picker' }).getByRole('status')).toHaveText('No stickers here yet');
        await page.getByRole('tab', { name: 'Cozy cats' }).click();
        await expect(page.getByRole('region', { name: 'Sticker picker' }).getByRole('status')).toHaveText('No stickers here yet');
        let finishNext;
        const nextResponse = new Promise(resolve => { finishNext = resolve; });
        await page.route(/\/sticker-packs\/pack.vibes\/stickers(?:\?.*)?$/, async route => {
          await nextResponse;
          await route.fulfill({ json: { packId: 'pack.vibes', items: [] } });
        });
        await page.getByRole('tab', { name: 'Good vibes' }).click();
        await expect(page.getByRole('region', { name: 'Sticker picker' }).getByRole('status')).toHaveText('Loading stickers…');
        await expect(page.getByText('No stickers here yet')).toHaveCount(0);
        finishNext();
        await expect(page.getByRole('region', { name: 'Sticker picker' }).getByRole('status')).toHaveText('No stickers here yet');
      }
      if (mode === 'lazy-loading') {
        await expect(page.locator('.sticker-cell img').first()).toBeVisible();
        await expect.poll(()=>page.locator('.sticker-cell img').count()).toBeGreaterThan(3);
        expect(await page.locator('.sticker-cell img').count()).toBeLessThan(25);
        await page.locator('.sticker-cell').nth(24).scrollIntoViewIfNeeded();
        await expect(page.locator('.sticker-cell').nth(24).locator('img')).toBeVisible();
        await page.getByRole('tab',{name:'Cozy cats'}).click();
        await page.locator('.sticker-grid').evaluate(el=>el.scrollTop=0);
        await expect(page.locator('.sticker-cell img').first()).toBeVisible();
        expect(await page.locator('.sticker-cell img').count()).toBeLessThan(48);
        await page.locator('.sticker-cell').last().scrollIntoViewIfNeeded();
        await expect(page.locator('.sticker-cell').last().locator('img')).toBeVisible();
        await expect(page.locator('.sticker-draft')).toHaveCount(0);
        await page.locator('.sticker-grid').evaluate(el=>el.scrollTop=0);
      }
      if (mode === 'edit' || mode === 'dragging') {
        await expect(page.getByRole('button', { name: 'Edit', exact: true })).toBeEnabled();
        if (mode === 'edit') await page.getByRole('button', { name: 'Edit', exact: true }).click();
        const first=page.locator('.sticker-cell').first(),last=page.locator('.sticker-cell').last();
        await expect(first.locator('img')).toBeVisible();
        const moving=await first.getAttribute('data-sticker-id'),src=await first.locator('img').getAttribute('src');
        const from=await first.boundingBox();
        await page.mouse.move(from.x+from.width/2,from.y+from.height/3);await page.mouse.down();
        await expect(page.getByText('Drag to reorder')).toBeVisible();
        const to=await last.boundingBox(),ghost=page.locator('.sticker-drag-preview');
        await page.mouse.move(to.x+to.width-4,to.y+to.height/3,{steps:8});
        await expect(ghost).toBeVisible();
        await expect(ghost).toHaveAttribute('data-sticker-drag-id',moving);
        await expect(ghost.locator('img')).toHaveAttribute('src',src);
        await expect(first).toHaveClass(/is-drag-source/);
        await expect(last).toHaveAttribute('data-drop-side','after');
        const previewBox = await ghost.boundingBox();
        expect(previewBox.x).toBeLessThan(to.x+to.width-4);
        expect(previewBox.x+previewBox.width).toBeGreaterThan(to.x+to.width-4);
        await expect(page.locator('.sticker-draft')).toHaveCount(0);
        if (mode === 'edit') {
          await page.keyboard.press('Escape'); await expect(ghost).toHaveCount(0); await page.mouse.up();
          await expect(page.locator('.sticker-cell').first()).toHaveAttribute('data-sticker-id',moving);
          await expect(page.getByRole('region',{name:'Sticker picker'})).toBeVisible();
          await page.mouse.move(from.x+from.width/2,from.y+from.height/3);await page.mouse.down();
          await page.mouse.move(to.x+to.width-4,to.y+to.height/3,{steps:8});await page.mouse.up();
          await expect(ghost).toHaveCount(0);
          await expect(page.locator('.sticker-cell').last()).toHaveAttribute('data-sticker-id',moving);
          await expect(page.locator('.sticker-draft')).toHaveCount(0);
          await page.getByRole('button',{name:'Done',exact:true}).focus();await page.keyboard.press('Enter');
          await page.locator('.sticker-tile').first().focus();await page.keyboard.press('Enter');
          await expect(page.locator('.sticker-draft')).toHaveCount(1);
          await page.locator('.sticker-draft button').click();
          await page.getByRole('button',{name:'Stickers',exact:true}).click();
          await expect(page.getByRole('button',{name:'Edit',exact:true})).toBeEnabled();
          await page.getByRole('button',{name:'Edit',exact:true}).click();
        }
      }
      if (mode === 'packs' || mode === 'pack-detail') {
        await page.getByRole('button', { name: 'Browse packs', exact: true }).click();
        await expect(page.getByRole('dialog', { name: 'Browse packs' })).toBeVisible();
        if (mode === 'pack-detail') {
          let detailReads=0;
          page.on('request',request=>{if(new URL(request.url()).pathname.includes('/sticker-packs/pack.hello'))detailReads++;});
          await page.getByRole('button', { name: /Little hellos Preview pack/ }).click();
          await expect(page.getByRole('dialog', { name: 'Little hellos' })).toBeVisible();
          const initialReads=detailReads;expect(initialReads).toBe(2);
          await page.getByRole('button',{name:'Back',exact:true}).click();
          await page.getByRole('button', { name: /Little hellos Preview pack/ }).click();
          await expect(page.getByRole('dialog', { name: 'Little hellos' })).toBeVisible();
          expect(detailReads).toBe(initialReads);
          await page.locator('.sticker-pack-item').first().click(); await expect(page.locator('.sticker-draft')).toHaveCount(0); await expect(page.locator('.sticker-detail-cover img')).toBeVisible(); }
      }
      if (mode.startsWith('import')) {
        if (mode === 'import-saved') await page.getByRole('tab', { name: 'Cozy cats' }).click();
        const prepared = await stickerPreparations(page);
        const files = { import: ['afternoon-walk.mp4', 'photo.png', 'poster.png', 'wave-1.gif'], 'import-adjust': ['afternoon-walk.mp4'], 'import-retry': ['wave-1.gif', 'wave-2.gif'] }[mode] ?? ['wave-1.gif'];
        await dropStickerFiles(page, files);
        await expect(page.locator('.composer-attachments')).toHaveCount(0);
        const dialog = page.getByRole('dialog', { name: 'Add to Saved' });
        await expect(dialog.getByRole('button', { name: 'Done', exact: true })).toHaveCount(0);
        await expect(dialog.getByRole('button', { name: `Add ${files.length}`, exact: true })).toBeEnabled();
        const options = name => dialog.getByRole('group', { name: `Options for ${name}` });
        const requestFor = name => prepared.find(request => request.sourceId === `source-${name}`);
        if (mode === 'import') {
          const video = options('afternoon-walk.mp4');
          await expect(video.getByRole('slider', { name: 'Size' })).toHaveAttribute('aria-valuetext', '240 pixels');
          await expect(video.getByRole('slider', { name: 'Smoothness' })).toHaveAttribute('aria-valuetext', '10 frames per second');
          await expect(video.getByRole('slider', { name: 'Start' })).toHaveAttribute('aria-valuetext', '0.0 seconds');
          await expect(video.getByRole('slider', { name: 'Length' })).toHaveAttribute('aria-valuetext', '4.0 seconds');
          await expect(options('photo.png').getByRole('slider', { name: 'Size' })).toHaveAttribute('aria-valuetext', 'Original size, 800 pixels');
          await expect(options('poster.png').getByRole('slider', { name: 'Size' })).toHaveAttribute('aria-valuetext', '1024 pixels');
          await expect(options('poster.png').getByText("Shrunk to WeChat's 1024 px limit.")).toBeVisible();
          await expect(options('wave-1.gif').getByRole('slider')).toHaveCount(0);
          for (const text of ['240 × 161 · 181 KB · 4.0s', '800 × 600 · 281 KB', '1024 × 768 · 461 KB', '96 × 96 · 5 KB · 1.0s']) await expect(dialog.getByText(text, { exact: true })).toBeVisible();
          expect(prepared).toHaveLength(4);
          expect(requestFor('afternoon-walk.mp4')).toMatchObject({ fit: false, maxEdge: 240, fps: 10, trim: null });
          expect(requestFor('photo.png')).toMatchObject({ fit: false, maxEdge: null, fps: null, trim: null });
          expect(requestFor('poster.png')).toMatchObject({ fit: false, maxEdge: 1024, fps: null, trim: null });
          expect(requestFor('wave-1.gif')).toMatchObject({ fit: false, maxEdge: null, fps: null, trim: null });
          await dialog.locator('.sticker-import-list').evaluate(list => { list.scrollTop = 0; });
        }
        if (mode === 'import-adjust') {
          const size = dialog.getByRole('slider', { name: 'Size' }), add = dialog.locator('.sticker-add-submit');
          await expect(dialog.getByText('240 × 161 · 181 KB · 4.0s', { exact: true })).toBeVisible();
          await size.focus();
          for (let step = 0; step < 4; step++) await page.keyboard.press('ArrowRight');
          await expect(size).toHaveAttribute('aria-valuetext', '640 pixels');
          await expect(add).toBeDisabled();
          await expect(dialog.getByRole('alert')).toHaveText(/^Too large to send/);
          // Only the size the slider rested on was prepared, not the stops passed on the way.
          expect(prepared).toHaveLength(2);
          expect(prepared[1]).toMatchObject({ maxEdge: 640, fps: 10, trim: null });
          for (let step = 0; step < 4; step++) await page.keyboard.press('ArrowLeft');
          await expect(add).toHaveText('Add 1');
          await expect(add).toBeEnabled();
          await expect(dialog.getByRole('alert')).toHaveCount(0);
          await dialog.getByRole('slider', { name: 'Smoothness' }).focus();
          await page.keyboard.press('ArrowRight');
          await expect(dialog.getByText('240 × 161 · 217 KB · 4.0s', { exact: true })).toBeVisible();
          await dialog.getByRole('slider', { name: 'Length' }).focus();
          for (let step = 0; step < 10; step++) await page.keyboard.press('ArrowLeft');
          await expect(dialog.getByText('240 × 161 · 163 KB · 3.0s', { exact: true })).toBeVisible();
          // Returning to 240 px reused its result, so only the two new settings were prepared.
          expect(prepared).toHaveLength(4);
          expect(prepared[2]).toMatchObject({ maxEdge: 240, fps: 12, trim: null });
          expect(prepared[3]).toMatchObject({ maxEdge: 240, fps: 12, trim: { startMs: 0, durationMs: 3000 } });
          await size.focus();
          await page.keyboard.press('End');
          await expect(size).toHaveAttribute('aria-valuetext', '1024 pixels');
          await expect(dialog.getByRole('alert')).toHaveText(/^Too large to send/);
          await expect(add).toBeDisabled();
          await size.blur();
        }
        if (mode === 'import-saved') {
          let finish;
          await page.route(/\/jobs\/scene-add$/, async route => { await new Promise(resolve => { finish = resolve; }); await route.fallback(); });
          await page.getByRole('button', { name: 'Add 1', exact: true }).click();
          await expect(dialog.getByRole('button', { name: 'Saving…', exact: true })).toBeDisabled();
          await dialog.press('Escape'); await expect(dialog).toBeVisible();
          await expect.poll(() => typeof finish).toBe('function'); finish();
          await expect(dialog).toHaveCount(0);
          await expect(page.getByRole('tab', { name: 'Saved', exact: true })).toHaveAttribute('aria-selected', 'true');
          await expect(page.locator('.sticker-cell')).toHaveCount(7);
          await expect(page.getByRole('button', { name: 'Imported sticker', exact: true })).toBeVisible();
          await expect(page.locator('.sticker-draft')).toHaveCount(0);
          await expect(page.getByRole('alert')).toHaveCount(0);
        }
        if (mode === 'import-retry') {
          let attempts = 0, retryKey;
          await page.route(/\/custom-stickers$/, async route => {
            const request = route.request();
            if (request.method() === 'POST' && request.postDataJSON().source.preparationId === 'prep-scene-2') {
              attempts++;
              if (attempts === 1) { retryKey = request.headers()['idempotency-key']; return route.fulfill({status:503,json:{error:{code:'service_unavailable'}}}); }
              expect(request.headers()['idempotency-key']).toBe(retryKey);
            }
            await route.fallback();
          });
          await dialog.getByRole('button', { name: 'Add 2', exact: true }).click();
          await expect(dialog.getByText('Added', { exact: true })).toHaveCount(1);
          await expect(dialog.getByRole('alert')).toHaveCount(1);
          await expect(dialog.getByRole('button', { name: 'Add 1', exact: true })).toBeEnabled();
          await dialog.getByRole('button', { name: 'Add 1', exact: true }).click();
          await expect(dialog).toHaveCount(0);
          await expect(page.locator('.sticker-cell')).toHaveCount(8);
          expect(attempts).toBe(2);
          await expect(page.locator('.sticker-draft')).toHaveCount(0);
        }
      }
    },
  });
}
