import test from "node:test";
import assert from "node:assert/strict";
import { canRemoveProfile } from "../src/profile-removal.ts";

test("profile removal requires the complete current name and a signed-out profile", () => {
  const account = { id: "profile-1", alias: "Personal", authenticated: false };
  assert.equal(canRemoveProfile(account, "Personal"), true);
  for (const value of ["", "personal", "Personal ", "profile-1"])
    assert.equal(canRemoveProfile(account, value), false);
  assert.equal(
    canRemoveProfile({ ...account, authenticated: undefined }, "Personal"),
    false,
  );
  assert.equal(
    canRemoveProfile({ ...account, authenticated: true }, "Personal"),
    false,
  );
  assert.equal(
    canRemoveProfile({ ...account, alias: undefined }, "profile-1"),
    true,
  );
});
