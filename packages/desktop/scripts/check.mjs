import { readFile } from "node:fs/promises";
const root = new URL("../", import.meta.url);
const config = JSON.parse(
  await readFile(new URL("src-tauri/tauri.conf.json", root)),
);
const cli = JSON.parse(await readFile(new URL("../cli/package.json", root)));
const pkg = JSON.parse(await readFile(new URL("package.json", root)));
const rust = await readFile(new URL("src-tauri/Cargo.toml", root), "utf8");
const rustVersion = rust.match(
  /\[package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
)?.[1];
if (
  config.version !== cli.version ||
  pkg.version !== cli.version ||
  rustVersion !== cli.version
)
  throw new Error("Desktop, native CLI and app versions must match");
if (config.build.frontendDist !== "../../client-ui/dist")
  throw new Error("Desktop must package the existing UI");
if (!config.bundle.externalBin.includes("binaries/wx"))
  throw new Error("Desktop must include the native CLI");
if (
  cli.dependencies?.["@agent-wechat/desktop"] ||
  cli.dependencies?.["@agent-wechat/client-ui"]
)
  throw new Error("CLI must not depend on desktop or UI");
console.log("Desktop package and native version agree");
