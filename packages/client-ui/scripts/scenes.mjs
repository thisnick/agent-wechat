#!/usr/bin/env node
// Runs the UI scenes with Playwright and prints how long they took. Screenshots stay
// in .scenes/screenshots for debugging; they reach Argos only when CI is set.
// Usage: node scripts/scenes.mjs [playwright test arguments, e.g. --grep chat-list]
// Prerequisites are in scripts/harness.mjs.
import { join } from "node:path";
import { elapsed, packageDir, playwright, prepare } from "./harness.mjs";

const started = Date.now();
// `pnpm scenes -- --grep x` forwards the separator too.
const args = process.argv.slice(2);
if (args[0] === "--") args.shift();

const prepared = await prepare();
if (prepared !== 0) process.exit(prepared);

const code = await playwright("scenes/playwright.config.mjs", args);
console.log(`\nScenes ${code === 0 ? "finished" : "failed"} in ${elapsed(Date.now() - started)} (including builds).`);
if (!process.env.CI) console.log(`Screenshots: ${join(packageDir, ".scenes/screenshots")}`);
process.exit(code);
