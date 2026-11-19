// Waits for a scene's page to stop changing. Shared by the screenshot scenes
// (capture.spec.mjs) and the recorded journeys (journeys/record.spec.mjs).
import { expect } from "@playwright/test";

/** Resolves once nothing the UI asked the hub for is still in flight. Open event streams do not count. */
export function trackRequests(page) {
  const open = new Set();
  const relevant = (request) =>
    new URL(request.url()).pathname.startsWith("/v1/") && !(request.headers().accept ?? "").includes("text/event-stream");
  page.on("request", (request) => relevant(request) && open.add(request));
  page.on("requestfinished", (request) => open.delete(request));
  page.on("requestfailed", (request) => open.delete(request));
  return {
    idle: () => expect.poll(() => open.size, { message: "hub requests still in flight" }).toBe(0),
  };
}

/**
 * The page has stopped changing: warmed avatars and photos shown, fonts loaded, images
 * decoded, no hub requests pending, two frames painted.
 *
 * An avatar shows initials until its image arrives, and a photo a spinner or
 * placeholder, so initials of a contact whose avatar the hub has cached, or any
 * spinner or placeholder (every photo the scenes show is cached too), mean "not
 * yet". The UI only fetches what is on screen (an IntersectionObserver), so what a
 * collapsed pane hides, or what scrolled out of view, is not waited for.
 */
export async function settle(page, requests, avatarInitials) {
  await page.waitForFunction((initials) => {
    const onScreen = (element) => {
      const box = element.getBoundingClientRect();
      return box.width > 0 && box.height > 0 && box.bottom > 0 && box.right > 0 && box.top < innerHeight && box.left < innerWidth;
    };
    const avatarPending = [...document.querySelectorAll(".avatar")].some(
      (avatar) => initials.includes(avatar.textContent.trim()) && !avatar.querySelector("img") && onScreen(avatar),
    );
    const photoPending = [...document.querySelectorAll(".media-spinner, .media-placeholder")].some(onScreen);
    return !avatarPending && !photoPending;
  }, avatarInitials);
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(() => Promise.all([...document.images].map((image) => image.decode().catch(() => {}))));
  await requests.idle();
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  await requests.idle();
}
