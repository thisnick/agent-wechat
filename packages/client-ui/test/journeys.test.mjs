import test from "node:test";
import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { routeTable } from "../src/routes.ts";
import { viewports } from "../scenes/scenes.mjs";

const folder = new URL("../journeys/", import.meta.url);
const files = (await readdir(folder)).filter((file) => file.endsWith(".journey.mjs"));

test("there is at least one journey to copy from", () => {
  assert.ok(files.includes("reply-in-chat.journey.mjs"));
});

test("journeys are named, described and open a known route", async () => {
  const routes = new Set(routeTable.map((entry) => entry.name));
  for (const file of files) {
    const name = file.slice(0, -".journey.mjs".length);
    // CI passes the names to a shell and the comment publisher accepts only this form.
    assert.match(name, /^[a-z0-9]+(-[a-z0-9]+)*$/, file);
    const journey = (await import(new URL(file, folder))).default;
    assert.equal(typeof journey.description, "string", `${name}.description`);
    assert.ok(journey.description.trim() && !/[.!?]\s+\S/.test(journey.description), `${name} needs a one-sentence description`);
    assert.ok(journey.viewport === undefined || journey.viewport in viewports, `${name}.viewport`);
    assert.ok(routes.has(journey.route), `${name} opens unknown route ${journey.route}`);
    for (const member of ["ready", "run"]) assert.equal(typeof journey[member], "function", `${name}.${member}`);
    for (const member of ["setup", "target", "prepare"])
      assert.ok(journey[member] === undefined || typeof journey[member] === "function", `${name}.${member}`);
  }
});
