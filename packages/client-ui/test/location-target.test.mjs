import test from "node:test";
import assert from "node:assert/strict";
import { googleMapsLocationTarget } from "../src/location-target.ts";

test("location cards search supplied place text on the fixed Google Maps origin", () => {
  const target = new URL(googleMapsLocationTarget({ name: "Example & park", address: "Example street, Example city" }));
  assert.equal(target.origin, "https://www.google.com");
  assert.equal(target.pathname, "/maps/search/");
  assert.equal(target.searchParams.get("api"), "1");
  assert.equal(target.searchParams.get("query"), "Example & park, Example street, Example city");
  assert.equal(new URL(googleMapsLocationTarget({ name: "Example", address: "Example" })).searchParams.get("query"), "Example");
  assert.equal(new URL(googleMapsLocationTarget({ name: null, address: "示例街道" })).searchParams.get("query"), "示例街道");
});

test("coordinate-only and unusable place text do not invent a map coordinate conversion", () => {
  for (const location of [{name: null, address: null}, {name: "  ", address: ""}, {name: "bad\naddress", address: null}, {name: "字".repeat(1000), address: null}])
    assert.equal(googleMapsLocationTarget(location), null);
});
