import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

test("desktop windows let the shared composer receive HTML file drops", async () => {
  const config = JSON.parse(
    await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url))
  );
  const main = config.app.windows.find((window) => window.label === "main");
  assert.equal(
    main.dragDropEnabled,
    false,
    "Tauri's native handler would consume drops before the HTML composer, particularly on Windows"
  );
});
