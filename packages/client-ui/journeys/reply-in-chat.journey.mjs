// Example journey: a message arrives from the simulated phone and the account replies.
// Copy this file to start a new journey; scripts/videos.mjs records every
// `*.journey.mjs` in this folder, or only the names it is given.
import { ACCOUNT } from "../scenes/accounts.mjs";
import { standardWorld } from "../scenes/scenes.mjs";

export default {
  description: "Mei's new message arrives in an open chat and the account replies to it.",
  /** A key of `viewports` in scenes/scenes.mjs: desktop, narrow or compact. */
  viewport: "desktop",
  /** Seeds the simulator before the page opens; returns what `target` and `run` need. */
  setup: standardWorld,
  /** The route the page opens (a name from src/routes.ts) and its parameters. */
  route: "chat",
  target: (world) => ({ account: ACCOUNT.id, thread: world.mei }),
  /** Waits until the first frame is worth showing. Recording starts after this. */
  ready: async ({ page, expect }) => {
    await expect(page.getByText("I booked the place near the station")).toBeVisible();
  },
  /** The recorded walkthrough. */
  run: async ({ page, expect, sim, world, video }) => {
    const conversation = page.getByRole("region", { name: "Conversation" });

    await video.chapter("A message arrives", "The simulator delivers a message from Mei's phone");
    await video.caption("New messages reach the open chat over the hub's event stream");
    await sim.advance(30);
    await sim.inbound(world.mei, "Running ten minutes late, sorry!");
    await expect(conversation.getByText("Running ten minutes late, sorry!")).toBeVisible();
    await video.pause(1500);
    await video.caption(null);

    await video.chapter("Reply", "Type in the message box and press Enter");
    const composer = page.getByRole("textbox", { name: "Message" });
    await composer.click();
    await video.type(composer, "No problem, I'll grab us a table");
    await video.pause(400);
    await composer.press("Enter");
    await expect(conversation.getByText("No problem, I'll grab us a table")).toBeVisible();
    await video.pause(2000);
  },
};
