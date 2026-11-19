// Records every journey in this folder as a video. A journey is a short scripted
// walkthrough of the UI against a fresh simulator hub, the same synthetic stack the
// screenshot scenes use (see ../scenes). Unlike a scene it keeps animations, shows
// the pointer and every action, and can make the simulator do things mid-recording.
//
// `scripts/videos.mjs` runs this spec and turns each recording into an MP4, an
// animated preview and a contact sheet. JOURNEYS=a,b limits the run to those names.
import { mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { test as base, expect } from "@playwright/test";
import { formatRoute } from "../src/routes.ts";
import { FIXED_TIME, viewports } from "../scenes/scenes.mjs";
import { startUiServer } from "../scenes/server.mjs";
import { seeder } from "../scenes/seed.mjs";
import { settle, trackRequests } from "../scenes/settle.mjs";
import { startStack } from "../scenes/stack.mjs";

const here = dirname(fileURLToPath(import.meta.url));
/** Gitignored. Raw recordings and their metadata; scripts/videos.mjs encodes them into ../.videos/out. */
export const rawDir = resolve(here, "../.videos/raw");

/** How long a chapter card stays up, in milliseconds. */
const CHAPTER_MS = 1600;
/** The pause before and after the walkthrough, so the first and last frames can be read. */
const HOLD_MS = 700;

const selected = (process.env.JOURNEYS ?? "").split(",").map((name) => name.trim()).filter(Boolean);
const files = (await readdir(here)).filter((file) => file.endsWith(".journey.mjs")).sort();
const journeys = [];
for (const file of files) {
  const name = file.slice(0, -".journey.mjs".length);
  if (selected.length && !selected.includes(name)) continue;
  // The file name is the journey's name, whatever the module exports.
  journeys.push({ ...(await import(pathToFileURL(join(here, file)).href)).default, name });
}
const unknown = selected.filter((name) => !files.includes(`${name}.journey.mjs`));
if (unknown.length) throw new Error(`No journey named ${unknown.join(", ")} in ${here}`);

const test = base.extend({
  ui: [
    async ({}, use) => {
      const server = await startUiServer({ cachePrefix: ".vite-journeys" });
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
    // Offline like the scenes: only the scenes server on loopback may be reached.
    await context.route(
      (url) => ["http:", "https:", "ws:", "wss:"].includes(url.protocol) && !["127.0.0.1", "localhost"].includes(url.hostname),
      (route) => route.abort("blockedbyclient"),
    );
    // Dates and times follow the simulator's clock; timers keep running.
    await context.clock.setFixedTime(FIXED_TIME);
    await use(context);
  },
});

const escapeHtml = (text) =>
  String(text).replace(/[&<>"']/g, (character) => `&#${character.charCodeAt(0)};`);

/**
 * What a journey's `run` receives as `video`. Chapters and captions narrate the
 * recording; `pause` holds a finished state on screen for the viewer. Wait for the
 * UI with `expect` first: a pause is for people watching, never for synchronisation.
 */
function narrator(page, started) {
  const chapters = [];
  let caption = null;
  return {
    chapters,
    /** A title card over a blurred page. Recording continues while it shows. */
    async chapter(title, description) {
      chapters.push({ title, description: description ?? null, atMs: Date.now() - started });
      await page.screencast.showChapter(title, { description, duration: CHAPTER_MS });
    },
    /**
     * A subtitle until the next caption; `null` removes it. It sits along the top edge
     * unless `position` is "bottom", since the message box fills the bottom of most screens.
     */
    async caption(text, { position = "top" } = {}) {
      await caption?.dispose();
      caption = null;
      if (text == null) return;
      const edge = position === "bottom" ? "bottom:28px" : "top:12px";
      caption = await page.screencast.showOverlay(
        `<div style="position:absolute;left:50%;${edge};transform:translateX(-50%);max-width:56%;` +
          `background:rgba(17,17,17,.82);color:#fff;font:500 17px/1.4 system-ui,sans-serif;padding:8px 16px;` +
          `border-radius:10px;text-align:center">${escapeHtml(text)}</div>`,
      );
    },
    /** Types like a person, one key at a time, so the viewer can follow. */
    async type(locator, text, { delay = 45 } = {}) {
      await locator.pressSequentially(text, { delay });
    },
    /** Holds the current state on screen for `ms` milliseconds. */
    pause: (ms = 800) => page.waitForTimeout(ms),
  };
}

for (const journey of journeys) {
  const viewport = journey.viewport ?? "desktop";
  test.describe(journey.name, () => {
    test.use({ viewport: viewports[viewport], hasTouch: viewport === "compact" });
    test(journey.name, async ({ page, ui, stack }) => {
      const errors = [];
      page.on("pageerror", (error) => errors.push(error.message));
      const requests = trackRequests(page);
      const sim = seeder(stack);
      const world = (await journey.setup?.(sim)) ?? {};
      await journey.prepare?.({ page, world });
      const route = { name: journey.route, server: stack.hub.serverId, ...journey.target?.(world) };
      await page.goto(`${ui.origin}/${formatRoute(route)}`);
      await journey.ready({ page, expect, world });
      await settle(page, requests, sim.avatarInitials);

      await mkdir(rawDir, { recursive: true });
      const video = join(rawDir, `${journey.name}.webm`);
      const metadata = join(rawDir, `${journey.name}.json`);
      await rm(metadata, { force: true });
      await page.screencast.start({ path: video, size: viewports[viewport] });
      // Chapter times count from the first recorded frame.
      const started = Date.now();
      const narration = narrator(page, started);
      try {
        // Smaller action labels on phone widths, where a long locator would run off the edge.
        const fontSize = viewports[viewport].width < 600 ? 11 : 16;
        await page.screencast.showActions({ cursor: "pointer", position: "top-right", fontSize, duration: 700 });
        await narration.pause(HOLD_MS);
        await journey.run({ page, expect, sim, world, video: narration });
        await narration.pause(HOLD_MS);
      } finally {
        await page.screencast.stop();
      }
      expect(errors, "uncaught page errors").toEqual([]);
      // Written only after a complete recording; scripts/videos.mjs encodes only these.
      await writeFile(
        metadata,
        `${JSON.stringify(
          {
            name: journey.name,
            description: journey.description,
            viewport,
            size: viewports[viewport],
            durationMs: Date.now() - started,
            chapters: narration.chapters,
          },
          null,
          2,
        )}\n`,
      );
    });
  });
}
