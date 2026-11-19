// Playwright configuration for the recorded journeys. `pnpm videos` runs it; see
// scripts/videos.mjs. The browser settings match the screenshot scenes (fixed locale,
// time zone, colour scheme and pixel ratio, loopback only), except that motion is
// kept: a video should show the UI's transitions.
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";

const here = dirname(fileURLToPath(import.meta.url));
/** Gitignored: Playwright results and the HTML report. */
const outputRoot = resolve(here, "../.videos");

export default defineConfig({
  testDir: here,
  testMatch: "record.spec.mjs",
  outputDir: join(outputRoot, "results"),
  fullyParallel: true,
  // Recording is real time; a busy machine drops frames. Two workers suit CI's runners.
  workers: process.env.CI ? 2 : 1,
  retries: process.env.CI ? 1 : 0,
  timeout: 180_000,
  expect: { timeout: 15_000 },
  reporter: [[process.env.CI ? "dot" : "list"], ["html", { open: "never", outputFolder: join(outputRoot, "report") }]],
  use: {
    locale: "en-US",
    timezoneId: "UTC",
    colorScheme: "light",
    deviceScaleFactor: 1,
    // Trace screenshots share the browser's screencast and cap its frames at 800px,
    // which would shrink the recording; keep the DOM snapshots for debugging only.
    trace: { mode: "retain-on-failure", screenshots: false },
    launchOptions: { args: ["--force-color-profile=srgb"] },
  },
});
