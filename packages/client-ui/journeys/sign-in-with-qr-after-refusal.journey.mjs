// Issue #609: WeChat refuses an existing account's saved sign-in. The sign-in
// screen offers a QR code for the same account, which signs back in with its
// chats in place. The simulator refuses the saved sign-in with a synthetic code.
import { ACCOUNT } from "../scenes/accounts.mjs";
import { standardWorld } from "../scenes/scenes.mjs";

export default {
  description: "WeChat refuses an account's saved sign-in; signing in with a QR code instead keeps the same account and chats.",
  viewport: "desktop",
  setup: async (sim) => {
    const world = await standardWorld(sim);
    // Signed out on the hub only, so the next sign-in tries the saved one.
    await sim.signOutLocally(ACCOUNT);
    await sim.refuseSavedLogin(ACCOUNT, -999);
    return world;
  },
  route: "accounts",
  target: () => ({}),
  ready: async ({ page, expect }) => {
    await expect(page.getByRole("heading", { name: "Choose an account" })).toBeVisible();
    await expect(page.locator(".login-account").filter({ hasText: "alex-morgan" })).toContainText("Sign in to WeChat");
  },
  run: async ({ page, expect, video }) => {
    await video.chapter("The saved sign-in is refused", "This account signed in before; the simulator now refuses its saved sign-in");
    await video.caption("alex-morgan has chats on this hub but needs to sign in again");
    await video.pause(1500);
    await page.locator(".login-account").filter({ hasText: "alex-morgan" }).click();
    await expect(page.getByRole("heading", { name: "Sign-in could not finish" })).toBeVisible();
    await expect(page.getByText("Details: login_failed · saved_device_login · -999")).toBeVisible();
    await video.caption("It fails at once, with no QR code. Details name the refused step and its code");
    await video.pause(2500);
    await video.caption("Try again would repeat the same saved sign-in");
    await video.pause(2000);
    await video.caption(null);

    await video.chapter("Sign in with a QR code", "For the same account, not a new one");
    const useQr = page.getByRole("button", { name: "Sign in with a QR code instead" });
    await useQr.hover();
    await video.pause(500);
    await useQr.click();
    await expect(page.getByRole("img", { name: /sign-in QR code/ })).toBeVisible();
    await video.caption("Scan the code and approve on the phone");
    await video.pause(1500);
    await page.getByRole("button", { name: /Simulate scan/ }).click();
    const approve = page.getByRole("button", { name: /Simulate phone approval/ });
    await expect(approve).toBeVisible();
    await video.pause(800);
    await approve.click();

    const mei = page.locator(".chat-row").filter({ has: page.getByText("Mei Chen", { exact: true }) });
    await expect(mei).toBeVisible();
    await video.caption("Signed back in to alex-morgan, with its chats");
    await video.pause(1000);
    await mei.click();
    await expect(page.getByRole("region", { name: "Conversation" }).getByText("I booked the place near the station")).toBeVisible();
    await video.pause(2500);
  },
};
