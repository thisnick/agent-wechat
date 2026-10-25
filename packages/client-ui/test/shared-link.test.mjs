import test from "node:test";
import assert from "node:assert/strict";
import { sharedLinkTarget } from "../src/shared-link-target.ts";

test("shared cards preserve the destination and disclose the actual source host", () => {
  assert.deepEqual(sharedLinkTarget("https://www.example.com/a?x=1#part"), {
    href: "https://www.example.com/a?x=1#part", host: "example.com",
  });
  assert.equal(sharedLinkTarget("http://example.com").href, "http://example.com/");
});

test("missing, invalid and executable URLs remain non-interactive cards", () => {
  for (const value of [null, "", "/relative", "javascript:alert(1)", "data:text/html,test", "https://", "https://user:password@example.com"]) {
    assert.equal(sharedLinkTarget(value), null);
  }
});
