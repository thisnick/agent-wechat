// Playwright configuration for the UI scenes. `pnpm scenes` runs it; see scripts/scenes.mjs.
//
// Everything that could make two runs differ is pinned here or in capture.spec.mjs:
// one worker, one fixed locale, time zone, colour scheme, pixel ratio and motion
// setting, and no network beyond loopback. Screenshots upload to Argos only in CI.
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";
import { createArgosReporterOptions } from "@argos-ci/playwright/reporter";

const here = dirname(fileURLToPath(import.meta.url));
/** Gitignored: results, the HTML report and the PNGs kept for local debugging. */
const outputRoot = resolve(here, "../.scenes");

export default defineConfig({
  testDir: here,
  testMatch: "capture.spec.mjs",
  outputDir: join(outputRoot, "results"),
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  reporter: [
    [process.env.CI ? "dot" : "list"],
    ["html", { open: "never", outputFolder: join(outputRoot, "report") }],
    ["@argos-ci/playwright/reporter", createArgosReporterOptions({ uploadToArgos: !!process.env.CI })],
  ],
  use: {
    locale: "en-US",
    timezoneId: "UTC",
    colorScheme: "light",
    reducedMotion: "reduce",
    deviceScaleFactor: 1,
    trace: "retain-on-failure",
    // Reduce text rendering differences between machines.
    launchOptions: { args: ["--disable-lcd-text", "--font-render-hinting=none", "--force-color-profile=srgb", "--disable-gpu", "--disable-gpu-compositing"] },
  },
});
