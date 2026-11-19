// What `pnpm scenes` and `pnpm videos` need before Playwright starts, and how they run it.
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

export const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const repositoryDir = resolve(packageDir, "../..");
const require = createRequire(import.meta.url);

/** Runs a command with inherited output and resolves to its exit code. */
export function run(command, args, options = {}) {
  return new Promise((done) => {
    const child = spawn(command, args, { stdio: "inherit", ...options });
    child.once("error", (error) => {
      console.error(`${command}: ${error.message}`);
      done(1);
    });
    child.once("exit", (code, signal) => done(code ?? (signal ? 1 : 0)));
  });
}

/** Checks Chromium and builds the SDK and simulator when missing. Resolves to 0 or a failing exit code. */
export async function prepare() {
  const { chromium } = require("@playwright/test");
  if (!existsSync(chromium.executablePath())) {
    console.error(
      `Playwright's Chromium is not installed (expected ${chromium.executablePath()}).\n` +
        "Install the revision that matches the pinned @playwright/test with:\n  npx playwright install chromium",
    );
    return 1;
  }

  if (!existsSync(join(repositoryDir, "packages/shared/dist/index.js"))) {
    console.log("Building @agent-wechat/shared first (the scenes use its SDK).");
    const code = await run("pnpm", ["--filter", "@agent-wechat/shared", "build"], { cwd: repositoryDir });
    if (code !== 0) return code;
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
    if (code !== 0) return code;
  }
  return 0;
}

/** Runs `playwright test` with a config file relative to the package. */
export function playwright(config, args, env = process.env) {
  return run(process.execPath, [require.resolve("@playwright/test/cli"), "test", "--config", join(packageDir, config), ...args], {
    cwd: packageDir,
    env,
  });
}

/** "3m 07s" for a duration in milliseconds. */
export function elapsed(ms) {
  const seconds = Math.round(ms / 1000);
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, "0")}s`;
}
