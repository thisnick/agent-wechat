import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { render } from "../scripts/scene-listing.mjs";
import { routeTable } from "../src/routes.ts";
import { scenes } from "../scenes/scenes.mjs";

test("the committed scene listing matches the scenes", async () => {
  const { problems, files } = await render();
  assert.deepEqual(problems, []);
  const committed = await readFile(new URL("../../../interfaces/ui/scenes.txt", import.meta.url), "utf8");
  assert.equal(committed.replaceAll("\r\n", "\n"), files["scenes.txt"], "run `pnpm --filter @agent-wechat/client-ui scenes:list`");
});

test("every route of the UI has at least one scene", () => {
  const covered = new Set(scenes.map((scene) => scene.route));
  const uncovered = routeTable.filter((entry) => !covered.has(entry.name)).map((entry) => entry.pattern);
  assert.deepEqual(uncovered, [], "add a scene in scenes/scenes.mjs for each route listed here");
});

test("scene rows are sorted, unique and described", async () => {
  const { files } = await render();
  const rows = files["scenes.txt"].split("\n").filter((line) => line && !line.startsWith("#"));
  assert.deepEqual(rows, [...rows].sort());
  assert.equal(new Set(rows.map((row) => row.split(" | ")[0])).size, rows.length);
  for (const row of rows) {
    const [item, attributes, description] = row.split(" | ");
    assert.match(item, /^scene [a-z0-9-]+$/);
    assert.match(attributes, /^route \/\S*, viewports (desktop|narrow|desktop\+narrow)$/);
    assert.ok(description && !description.startsWith("MISSING"), row);
  }
});

test("scenes name their viewports, setup, target and a way to know they are ready", () => {
  for (const scene of scenes) {
    for (const member of ["setup", "target", "ready"]) assert.equal(typeof scene[member], "function", `${scene.name}.${member}`);
    assert.ok(scene.mask === undefined || typeof scene.mask === "function", `${scene.name}.mask`);
    assert.ok(scene.actions === undefined || typeof scene.actions === "function", `${scene.name}.actions`);
  }
});
