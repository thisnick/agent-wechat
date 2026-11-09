import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { render } from "../scripts/interface-listing.mjs";

test("the committed route listing matches the route table", async () => {
  const { problems, files } = await render();
  assert.deepEqual(problems, []);
  const committed = await readFile(new URL("../../../interfaces/ui/routes.txt", import.meta.url), "utf8");
  assert.equal(committed.replaceAll("\r\n", "\n"), files["routes.txt"], "run `node packages/client-ui/scripts/interface-listing.mjs`");
});

test("route rows are sorted, unique and described", async () => {
  const { files } = await render();
  const rows = files["routes.txt"].split("\n").filter((line) => line && !line.startsWith("#"));
  assert.deepEqual(rows, [...rows].sort());
  assert.equal(new Set(rows.map((row) => row.split(" | ")[0])).size, rows.length);
  for (const row of rows) {
    const [item, attributes, description] = row.split(" | ");
    assert.match(item, /^route \/\S*$/);
    assert.ok(attributes === "-" || attributes.startsWith("params "), row);
    assert.ok(description && !description.startsWith("MISSING"), row);
  }
});
