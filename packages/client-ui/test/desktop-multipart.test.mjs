import test from "node:test";
import assert from "node:assert/strict";
import { desktopMultipart } from "../src/desktop-multipart.ts";

test("desktop multipart streaming preserves file bytes, name and MIME type without reading a FormData request body", async () => {
  const bytes = new Uint8Array([0, 1, 13, 10, 128, 255]);
  const form = new FormData();
  form.append("file", new File([bytes], "photo.png", { type: "image/png" }));
  form.append("caption", "first\nsecond");
  const { body, contentType } = desktopMultipart(form);
  assert.ok(body instanceof Blob);
  assert.match(contentType, /^multipart\/form-data; boundary=----agentwechat-/);
  const request = new Request("https://hub.test/v1/accounts/a/uploads", {
    method: "POST",
    body,
    headers: { "Content-Type": contentType },
  });
  const parsed = await request.formData();
  const file = parsed.get("file");
  assert.equal(file.name, "photo.png");
  assert.equal(file.type, "image/png");
  assert.deepEqual(new Uint8Array(await file.arrayBuffer()), bytes);
  assert.equal(parsed.get("caption"), "first\r\nsecond");
});

test("multipart headers escape quoted names and CRLF without turning them into headers", async () => {
  const form = new FormData();
  form.append('field"\r\nX-Test: evil', new File(["fixture"], 'note"\r\n.txt'));
  const { body, contentType } = desktopMultipart(form);
  const wire = await body.text();
  assert.match(wire, /name="field%22%0D%0AX-Test: evil"/);
  assert.match(wire, /filename="note%22%0D%0A.txt"/);
  assert.ok(!wire.includes("\r\nX-Test: evil"));
  assert.ok(wire.endsWith(`--${contentType.split("boundary=")[1]}--\r\n`));
  assert.notEqual(contentType, desktopMultipart(form).contentType);
});
