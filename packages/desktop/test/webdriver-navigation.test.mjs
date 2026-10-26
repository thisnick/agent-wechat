import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { reloadPage } from "./webdriver-navigation.mjs";

test("reload waits past the old ready document and the new loading document", async () => {
  let reloads = 0;
  let window = { location: { reload: () => reloads++ } };
  const document = { readyState: "complete" };
  const script = async (source) =>
    runInNewContext(`(function () { ${source} })()`, { window, document });
  let polls = 0;
  await reloadPage(script, async (source, ready) => {
    assert.equal(reloads, 1);
    // The outgoing document is still complete, with its cached images loaded.
    assert.equal(ready(await script(source)), false);
    polls++;
    window = {};
    document.readyState = "loading";
    assert.equal(ready(await script(source)), false);
    polls++;
    document.readyState = "complete";
    assert.equal(ready(await script(source)), true);
    polls++;
  });
  assert.equal(polls, 3);
  assert.equal(reloads, 1);
});

test("reload cannot succeed if the outgoing document remains active", async () => {
  const window = { location: { reload() {} } };
  const document = { readyState: "complete" };
  const script = async (source) =>
    runInNewContext(`(function () { ${source} })()`, { window, document });
  await assert.rejects(
    reloadPage(script, async (source, ready) => {
      for (let attempt = 0; attempt < 3; attempt++)
        assert.equal(ready(await script(source)), false);
      throw new Error("Navigation timed out");
    }),
    /Navigation timed out/,
  );
});
