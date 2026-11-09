#!/usr/bin/env node
// Runs the UI scenes with Playwright and prints how long they took. Screenshots stay
// in .scenes/screenshots for debugging; they reach Argos only when CI is set.
// Usage: node scripts/scenes.mjs [playwright test arguments, e.g. --grep chat-list]
//
// Needs, once per machine: the Playwright Chromium that matches the pinned
// @playwright/test (`npx playwright install chromium`; an existing copy in
// ~/.cache/ms-playwright is used when its revision matches) and, for the CJK glyphs
// CI renders, the Noto CJK and Noto Color Emoji fonts.
// WX_DEV_BINARY names a prebuilt wx-dev; otherwise the simulator is built here.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryDir = resolve(packageDir, "../..");
const require = createRequire(import.meta.url);
const started = Date.now();
// `pnpm scenes -- --grep x` forwards the separator too.
const args = process.argv.slice(2);
if (args[0] === "--") args.shift();

function run(command, args, options = {}) {
  return new Promise((done) => {
    const child = spawn(command, args, { stdio: "inherit", ...options });
    child.once("error", (error) => {
      console.error(`${command}: ${error.message}`);
      done(1);
    });
    child.once("exit", (code, signal) => done(code ?? (signal ? 1 : 0)));
  });
}

const { chromium } = require("@playwright/test");
if (!existsSync(chromium.executablePath())) {
  console.error(
    `Playwright's Chromium is not installed (expected ${chromium.executablePath()}).\n` +
      "Install the revision that matches the pinned @playwright/test with:\n  npx playwright install chromium",
  );
  process.exit(1);
}

if (!existsSync(join(repositoryDir, "packages/shared/dist/index.js"))) {
  console.log("Building @agent-wechat/shared first (the scenes use its SDK).");
  const code = await run("pnpm", ["--filter", "@agent-wechat/shared", "build"], { cwd: repositoryDir });
  if (code !== 0) process.exit(code);
}

if (!process.env.WX_DEV_BINARY) {
  console.log("Building the simulator (wx-dev).");
  const code = await run("cargo", [
    "build",
    "--profile",
    "test",
    "--locked",
    "--manifest-path",
    join(repositoryDir, "packages/native-client-rust/Cargo.toml"),
    "-p",
    "wechat-simulator",
  ]);
  if (code !== 0) process.exit(code);
}

const code = await run(
  process.execPath,
  [require.resolve("@playwright/test/cli"), "test", "--config", join(packageDir, "scenes/playwright.config.mjs"), ...args],
  { cwd: packageDir },
);
const seconds = (Date.now() - started) / 1000;
console.log(`\nScenes ${code === 0 ? "finished" : "failed"} in ${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s (including builds).`);
if (!process.env.CI) console.log(`Screenshots: ${join(packageDir, ".scenes/screenshots")}`);
process.exit(code);
