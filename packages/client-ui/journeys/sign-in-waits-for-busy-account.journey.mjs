// The sign-in screen's request is refused because the account is busy, while an
// earlier sign-in has already succeeded on the hub. The screen waits, sees the
// account signed in and opens the chats instead of stopping on an error.
import { ACCOUNT } from "../scenes/accounts.mjs";
import { standardWorld } from "../scenes/scenes.mjs";

const wait = (ms) => new Promise((done) => setTimeout(done, ms));

export default {
  description: "A sign-in refused because the account is busy waits, then opens the chats once the account is signed in.",
  viewport: "desktop",
  setup: standardWorld,
  route: "chat",
  target: (world) => ({ account: ACCOUNT.id, thread: world.mei }),
  prepare: async ({ page }) => {
    // The busy window after a sign-in lasts a few seconds and can't be held open
    // on demand, so the screen's own sign-in requests get the hub's busy refusal.
    await page.route(`**/v1/accounts/${ACCOUNT.id}/auth/login`, (route) =>
      route.fulfill({ status: 409, json: { error: { code: "operation_in_progress", retryable: false } } }));
  },
  ready: async ({ page, expect }) => {
    await expect(page.getByText("I booked the place near the station")).toBeVisible();
  },
  run: async ({ page, expect, sim, video }) => {
    await video.chapter("Signed out", "WeChat ends the session and the app asks to sign in");
    await sim.signOutLocally(ACCOUNT);
    await expect(page.getByText("WeChat signed out. Sign in again.")).toBeVisible();
    const status = page.getByText("This account is still finishing other work. Trying again in a moment…");
    await expect(status).toBeVisible();
    await video.caption("The hub refuses this sign-in because the account is busy. No error: it waits and tries again");
    await video.pause(2500);

    await video.chapter("The account finishes signing in", "An earlier sign-in completes on the hub");
    const op = await sim.client.login(ACCOUNT.id, { strategy: "auto", timeoutSeconds: 60 }, "journey-earlier-sign-in");
    for (let i = 0; i < 100; i++) {
      if ((await sim.client.operation(ACCOUNT.id, op.id)).state === "succeeded") break;
      await wait(100);
    }
    await video.caption("On its next try the screen sees the account is signed in and opens the chats");
    await expect(page.getByRole("region", { name: "Conversation" }).getByText("I booked the place near the station")).toBeVisible();
    await expect(page.getByText("WeChat signed out. Sign in again.")).toHaveCount(0);
    await video.pause(2500);
  },
};
