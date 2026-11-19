// Captures every scene at each of its viewports. A fresh simulator hub backs every
// test, so a scene sees only the data its own setup created, in the same order each
// run. The UI itself is served once per worker (see server.mjs).
import { copyFile, mkdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test as base, expect } from "@playwright/test";
import { argosScreenshot } from "@argos-ci/playwright";
import { formatRoute } from "../src/routes.ts";
import { FIXED_TIME, scenes, viewports } from "./scenes.mjs";
import { startUiServer } from "./server.mjs";
import { seeder } from "./seed.mjs";
import { settle, trackRequests } from "./settle.mjs";
import { startStack } from "./stack.mjs";

/**
 * Translations captured after English. Each scene runs and is checked in English,
 * then switches language in place (as another window changing the setting would)
 * and is captured again as `<scene>@<viewport>@<language>`.
 */
const TRANSLATIONS = ["zh-CN"];

/** Gitignored, next to the Playwright results; see playwright.config.mjs. */
const outputRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../.scenes");

/** Frozen text and motion: nothing may change between the page settling and the screenshot. */
const FREEZE_CSS = `
*, *::before, *::after {
  animation: none !important;
  transition: none !important;
  scroll-behavior: auto !important;
  caret-color: transparent !important;
}
`;

const test = base.extend({
  ui: [
    async ({}, use) => {
      const server = await startUiServer();
      await use(server);
      await server.close();
    },
    { scope: "worker" },
  ],
  stack: async ({ ui }, use) => {
    const stack = await startStack();
    ui.attach(stack);
    try {
      await use(stack);
    } finally {
      ui.attach(null);
      await stack.stop();
    }
  },
  context: async ({ context }, use) => {
    // The scenes are offline: only the scenes server on loopback may be reached.
    await context.route(
      (url) => ["http:", "https:", "ws:", "wss:"].includes(url.protocol) && !["127.0.0.1", "localhost"].includes(url.hostname),
      (route) => route.abort("blockedbyclient"),
    );
    // Dates and times follow the simulator's clock; timers keep running.
    await context.clock.setFixedTime(FIXED_TIME);
    await context.addInitScript((css) => {
      const style = document.createElement("style");
      style.textContent = css;
      document.addEventListener("DOMContentLoaded", () => document.head.append(style), { once: true });
    }, FREEZE_CSS);
    await use(context);
  },
});

/**
 * Marks animated images so the screenshot shows their first frame, not whichever
 * frame happened to be playing.
 *
 * Argos pauses GIFs before each screenshot, but it recognises one only by a `.gif` or
 * `data:image/gif` URL, or by `data-image-type="gif"`. The UI shows stickers and other
 * media from `blob:` URLs, so each GIF among those is marked here by its file header.
 */
async function markBlobGifs(page) {
  await page.evaluate(() =>
    Promise.all(
      [...document.images].map(async (image) => {
        const source = image.currentSrc || image.src;
        if (!source.startsWith("blob:") || image.getAttribute("data-image-type") === "gif") return;
        try {
          const header = await (await fetch(source)).blob().then((blob) => blob.slice(0, 6).text());
          if (header === "GIF87a" || header === "GIF89a") image.setAttribute("data-image-type", "gif");
        } catch {
          // A revoked URL can no longer be read; leave that image unmarked.
        }
      }),
    ),
  );
}

for (const scene of scenes) {
  for (const viewport of scene.viewports) {
    test.describe(scene.name, () => {
      test.use({ viewport: viewports[viewport], hasTouch: viewport === "compact" });
      test(`${scene.name}@${viewport}`, async ({ page, ui, stack }, testInfo) => {
        const errors = [];
        page.on("pageerror", (error) => errors.push(error.message));
        const requests = trackRequests(page);
        const sim = seeder(stack);
        const world = await scene.setup(sim);
        await scene.prepare?.({ page });
        const route = { name: scene.route, server: stack.hub.serverId, ...scene.target(world) };
        if (scene.start) {
          await page.goto(`${ui.origin}/${formatRoute({ name: scene.start, server: stack.hub.serverId, ...scene.target(world) })}`);
          await page.waitForFunction(() => !!document.querySelector(".shell, .onboarding-shell") && !document.body.innerText.includes("Opening AgentWechat"));
          await page.evaluate((hash) => { location.hash = hash; }, formatRoute(route));
        } else {
          await page.goto(`${ui.origin}/${formatRoute(route)}`);
        }
        await scene.ready({ page, expect, world });
        await scene.actions?.({ page, expect, world });
        await settle(page, requests, sim.avatarInitials);
        const capture = async (name) => {
          await markBlobGifs(page);
          const attachments = await argosScreenshot(page, name, {
            // Moving the pointer to suppress hover would move an active drag too.
            disableHover: !scene.preservePointer,
            mask: scene.mask?.(page, world) ?? [],
            maskColor: "#000000",
          });
          // Keep a copy outside Playwright's result folders for looking at locally.
          const png = attachments.find((attachment) => attachment.contentType === "image/png");
          if (png) {
            await mkdir(join(outputRoot, "screenshots"), { recursive: true });
            await copyFile(png.path, join(outputRoot, "screenshots", `${name}.png`));
          }
        };
        const name = `${scene.name}@${viewport}`;
        await capture(name);
        for (const language of TRANSLATIONS) {
          await page.evaluate((language) => {
            localStorage.setItem("agentwechat-lang", language);
            dispatchEvent(new StorageEvent("storage", { key: "agentwechat-lang", newValue: language }));
          }, language);
          await page.waitForFunction((language) => document.documentElement.lang === language, language);
          await settle(page, requests, sim.avatarInitials);
          await capture(`${name}@${language}`);
        }
        expect(errors, "uncaught page errors").toEqual([]);
      });
    });
  }
}
