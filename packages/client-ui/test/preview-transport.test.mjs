import test from "node:test";
import assert from "node:assert/strict";
import { previewFetch } from "../src/preview-transport.ts";

test("preview routes preserve API bodies, keys, queries, streaming and cancellation without cross-origin browser requests", async () => {
  const calls = [];
  const abort = new AbortController();
  const transport = previewFetch("https://hub.example:6174", "https://ui.example:8444", "/__hubs/work", async request => {
    calls.push(request);
    return new Response("data: event\n\n", { headers: { "Content-Type": "text/event-stream" } });
  });
  const response = await transport("https://hub.example:6174/v1/accounts/a/sends?key=fixture", {
    method: "POST", headers: { Authorization: "Bearer preview-key", "Idempotency-Key": "same-key" }, body: '{"text":"Hi"}', signal: abort.signal, redirect: "error",
  });
  assert.equal(calls[0].url, "https://ui.example:8444/__hubs/work/v1/accounts/a/sends?key=fixture");
  assert.equal(calls[0].headers.get("Authorization"), "Bearer preview-key");
  assert.equal(calls[0].headers.get("Idempotency-Key"), "same-key");
  assert.equal(calls[0].redirect, "error");
  assert.equal(await calls[0].text(), '{"text":"Hi"}');
  assert.equal(await response.text(), "data: event\n\n");
  abort.abort(); assert.equal(calls[0].signal.aborted, true);
  for (const url of ["https://other.example/v1/status", "https://hub.example:6174/__dev/session"]) {
    assert.throws(() => transport(url), /Unexpected preview hub/);
  }
  assert.equal(calls.length, 1);
});

test("preview routes cannot select another route or an arbitrary proxy URL", () => {
  for (const route of ["https://other.example", "/__hubs/../other", "/__hubs/a/v1", "/v1"]) {
    assert.throws(() => previewFetch("https://hub.example", "https://ui.example", route), /Invalid preview proxy/);
  }
});


test("preview preserves multipart boundaries and Request input bodies without streaming uploads", async () => {
  const calls = [];
  const transport = previewFetch("https://hub.example", "http://127.0.0.1:5189", "/__hubs/work", async request => { calls.push(request); return new Response("ok"); });
  const data = new FormData(); data.append("file", new Blob(["fixture bytes"], { type: "text/plain" }), "fixture.txt");
  await transport("https://hub.example/v1/uploads", { method: "POST", body: data });
  const form = await calls[0].formData(); assert.equal(await form.get("file").text(), "fixture bytes");
  const abort = new AbortController();
  await transport(new Request("https://hub.example/v1/accounts/a/contact-requests", { method: "POST", headers: { "Content-Type": "application/json", "Idempotency-Key": "original-key" }, body: '{"message":"hello"}', signal: abort.signal }));
  assert.equal(await calls[1].text(), '{"message":"hello"}'); assert.equal(calls[1].headers.get("Idempotency-Key"), "original-key");
  abort.abort(); assert.equal(calls[1].signal.aborted, true);
});
