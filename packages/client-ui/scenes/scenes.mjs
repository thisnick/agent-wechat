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

/** A row of the chat or contact list, found by the conversation's name rather than its preview text. */
const row = (page, name) => page.locator(".chat-row").filter({ has: page.getByText(name, { exact: true }) });
/**
 * The sign-in code is random on every run; the countdown beside it is not. Masks
 * select by class, not text, so they also apply to the translated captures.
 */
const qr = (page) => [page.locator("img.login-qr")];
/** The hub address includes the port the scenes server happened to get. */
const hubAddress = (page) => [page.locator("#hub-address")];

async function miniProgramWorld(sim) {
  await sim.addAccount(ACCOUNT);
  await sim.signIn(ACCOUNT);
  await sim.miniPrograms("sim_mei");
  return { thread: "sim_mei" };
}

export const scenes = [
  {
    name: "mini-program-cards",
    route: "chat",
    description: "Mini Program cards with an available shared page and a missing package.",
    viewports: ["desktop", "narrow"],
    setup: miniProgramWorld,
    target: (world) => ({ account: ACCOUNT.id, thread: world.thread }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("button", { name: "Open Seasonal menu" })).toBeEnabled();
      await expect(page.getByRole("button", { name: "Open A weekend hike" })).toBeDisabled();
    },
  },
  {
    name: "mini-program-list",
    route: "mini-programs",
    description: "Programs from the account's staged cache and received cards.",
    viewports: ["desktop", "narrow"],
    setup: miniProgramWorld,
    target: () => ({ account: ACCOUNT.id }),
    ready: async ({ page, expect }) => {
      await expect(page.getByRole("heading", { name: "Mini Programs" })).toBeVisible();
      await expect(page.getByText("Experimental", { exact: true })).toBeVisible();
      await expect(page.getByRole("button", { name: "Open Cafe" })).toBeEnabled();
      await expect(page.getByRole("button", { name: "Open Trail guide" })).toBeDisabled();
    },
  },
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
      const search = page.getByRole("textbox", { name: "Search conversations" });
      await search.fill("Metro");
      await expect(library).toHaveCount(0);
      await expect(metro).toBeVisible();
      await expect(official).toHaveAttribute("aria-pressed", "true");
      await expect(all).toHaveAttribute("aria-pressed", "false");
      await expect(all).toBeInViewport();
      await expect(bar.getByRole("button", { name: "Groups 0 unread", exact: true }).locator(".filter-unread")).toHaveCount(0);
      await search.clear();
      await search.blur();
      await expect(library).toBeVisible();
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
];
