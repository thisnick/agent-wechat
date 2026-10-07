import test from "node:test";
import assert from "node:assert/strict";
import { expiredMediaMessage } from "./media-status.ts";

test("CLI explains confirmed expiry using Unix seconds", () => {
  const message = expiredMediaMessage({ type: "expired", filename: "report.pdf", format: "",
    expiresAt: 1_700_000_200, retryable: false });
  assert.match(message, /expired.*2023-11-14T22:16:40.000Z/);
  assert.match(message, /Ask the sender to resend/);
});

test("missing or invalid timestamps do not prevent an expiry explanation", () => {
  for (const expiresAt of [undefined, NaN, 1e30]) {
    assert.match(expiredMediaMessage({ type: "expired", filename: "", format: "", expiresAt }), /Media has expired/);
  }
});

test("cached or pending media are not presented as expired", () => {
  assert.equal(expiredMediaMessage({ type: "file", filename: "report.pdf", format: "pdf", data: "bytes" }), undefined);
  assert.equal(expiredMediaMessage({ type: "pending", filename: "", format: "" }), undefined);
});
