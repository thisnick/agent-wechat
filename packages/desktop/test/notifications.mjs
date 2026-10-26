// HTTP/SSE and OS-service acceptance; no real account or WeChat endpoint.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function wait(check) {
  for (let attempt = 0; attempt < 100; attempt++) {
    const value = await check();
    if (value) return value;
    await pause(100);
  }
  throw new Error("Notification acceptance timed out");
}
export async function notificationsAcceptance(invoke) {
  const window = (action = "state") =>
    JSON.parse(execFileSync("python3", [
      fileURLToPath(new URL("./window-control.py", import.meta.url)), action,
    ], { encoding: "utf8" }));
  const log = join(process.env.HOME, "notifications.jsonl");
  const service = spawn(
    "python3",
    [fileURLToPath(new URL("./notification-service.py", import.meta.url)), log],
    { stdio: ["ignore", "pipe", "inherit"] }
  );
  let ready = false;
  service.stdout.on("data", () => {
    ready = true;
  });
  const streams = new Set(),
    fetched = new Set(),
    messages = new Map();
  let muted = false,
    contactReady = true,
    cursor = 0,
    rejectStream = false,
    expireStream = false,
    baselineReads = 0;
  const events = [];
  const account = "fixture",
    thread = "peer",
    serverId = "desktop-notifications-fixture";
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://localhost");
    const path = url.pathname;
    if (request.headers.authorization !== "Bearer " + "a".repeat(64)) {
      response.writeHead(401);
      response.end();
      return;
    }
    const json = (value) => {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify(value));
    };
    if (path === "/v1/status")
      return json({ serverId, version: { backend: "native" } });
    if (path === "/v1/accounts")
      return json({ items: [{ id: account, authenticated: true }] });
    if (path === "/v1/accounts/fixture")
      return json({ authenticated: true, contactSyncReady: contactReady });
    if (path === "/v1/accounts/fixture/threads") {
      baselineReads++;
      return json({
        items: [],
        nextCursor: null,
        asOfEventCursor: String(cursor),
      });
    }
    if (path === "/v1/accounts/fixture/threads/peer")
      return json({ id: thread, displayName: "Fixture contact", muted });
    if (path.startsWith("/v1/accounts/fixture/messages/")) {
      const id = decodeURIComponent(path.split("/").at(-1));
      fetched.add(id);
      return json(messages.get(id));
    }
    if (path === "/v1/accounts/fixture/events/stream") {
      if (expireStream) {
        expireStream = false;
        response.statusCode = 410;
        return json({ error: { code: "event_cursor_expired" } });
      }
      if (rejectStream) {
        rejectStream = false;
        response.writeHead(503);
        response.end();
        return;
      }
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      response.write(": ready\n\n");
      for (const event of events.filter(
        (event) => Number(event.cursor) > Number(url.searchParams.get("after"))
      ))
        response.write("data: " + JSON.stringify(event) + "\n\n");
      streams.add(response);
      request.on("close", () => streams.delete(response));
      return;
    }
    response.writeHead(404);
    json({});
  });
  const entries = async () => {
    try {
      return (await readFile(log, "utf8"))
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(JSON.parse);
    } catch {
      return [];
    }
  };
  const send = (id, options = {}) => {
    messages.set(id, {
      id,
      accountId: account,
      threadId: thread,
      direction: "incoming",
      type: 1,
      body: "Fixture message",
      ...options.message,
    });
    const event = {
      cursor: String(++cursor),
      kind: "message.committed",
      createdAt: new Date().toISOString(),
      data: {
        messageId: id,
        threadId: thread,
        origin: "live",
        direction: "incoming",
        ...options.event,
      },
    };
    events.push(event);
    for (const stream of streams)
      stream.write("data: " + JSON.stringify(event) + "\n\n");
  };
  let connection;
  try {
    await wait(() => ready);
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    connection = await invoke("desktop_remote", {
      name: "Notification fixture",
      endpoint: "http://127.0.0.1:" + server.address().port,
      key: "a".repeat(64),
      certificate: null,
    });
    await wait(() => streams.size);
    send("history", { event: { origin: "catchup" } });
    send("own", { event: { direction: "outgoing" } });
    muted = true;
    send("muted");
    await wait(() => fetched.has("muted"));
    await pause(250);
    assert.equal(
      (await entries()).length,
      0,
      "muted/history/outgoing messages must not notify"
    );
    muted = null;
    send("unknown-mute");
    await wait(() => fetched.has("unknown-mute"));
    await pause(250);
    assert.equal(
      (await entries()).length,
      0,
      "unknown mute state must remain silent"
    );
    muted = false;
    contactReady = false;
    send("contact-not-ready");
    await pause(400);
    assert.equal(
      (await entries()).length,
      0,
      "unready contact sync must remain silent"
    );
    contactReady = true;
    window("close");
    await wait(() => !window().visible);
    const reopened = spawn(process.env.DESKTOP_BINARY, [], { stdio: "ignore" });
    try {
      await wait(() => window().visible);
      await wait(() => reopened.exitCode === 0);
    } finally {
      if (reopened.exitCode === null) reopened.kill("SIGTERM");
    }
    window("close");
    await wait(() => !window().visible);
    await invoke("notifications_context", {
      focus: { connectionId: connection.id, accountId: account, threadId: thread },
    });
    send("new");
    const first = await wait(async () => (await entries())[0]);
    assert.equal(first.title, "Fixture contact");
    assert.equal(first.body, "Fixture message");
    assert.equal(
      window().visible,
      false,
      "a background notification must not reopen the window"
    );
    send("new");
    await pause(700);
    assert.equal(
      (await entries()).length,
      1,
      "duplicate message must not notify again"
    );
    execFileSync("gdbus", [
      "call",
      "--session",
      "--dest",
      "org.freedesktop.Notifications",
      "--object-path",
      "/org/freedesktop/Notifications",
      "--method",
      "io.github.thisnick.AgentWechat.Test.Click",
      String(first.id),
    ]);
    const target = await wait(() => invoke("notifications_take_target"));
    assert.deepEqual(target, {
      connectionId: connection.id,
      serverId,
      accountId: account,
      threadId: thread,
    });
    await wait(() => window().visible);
    await invoke("notifications_context", { focus: null });
    rejectStream = true;
    for (const stream of streams) stream.destroy();
    await wait(() => streams.size === 0);
    send("reconnected");
    await wait(async () => (await entries()).length === 2);
    assert.equal(
      (await entries())[1].body,
      "Fixture message",
      "transport reconnect must replay from its cursor"
    );
    const priorBaselines = baselineReads;
    expireStream = true;
    for (const stream of streams) stream.destroy();
    await wait(() => streams.size === 0);
    send("expired-history");
    await wait(() => baselineReads > priorBaselines && streams.size > 0);
    assert.equal(
      (await entries()).length,
      2,
      "expired cursors must start a fresh boundary without old alerts"
    );
    send("after-expiry");
    await wait(async () => (await entries()).length === 3);
    send("quoted-reply", {
      message: {
        type: 49,
        appMessageType: 57,
        body: "Yes, that works",
        quote: {
          serverId: "42",
          type: 1,
          embeddedText: "Original quoted message",
        },
      },
    });
    const quoted = await wait(async () => (await entries())[3]);
    assert.equal(
      quoted.body,
      "Yes, that works",
      "quoted reply alerts must show the new comment"
    );
    await invoke("notifications_enabled", { enabled: false });
    send("disabled");
    await pause(700);
    assert.equal(
      (await entries()).length,
      4,
      "disabled notifications must remain silent"
    );
    await invoke("notifications_enabled", { enabled: true });
    console.log(
      "Native notifications: close hides the window, relaunch restores it, hidden SSE/D-Bus delivery, click restore, mute/history/outgoing suppression, deduplication and settings passed"
    );
  } finally {
    await invoke("desktop_select", { id: "local" }).catch(() => {});
    if (connection)
      await invoke("desktop_forget", { id: connection.id }).catch(() => {});
    for (const stream of streams) stream.destroy();
    await new Promise((resolve) => server.close(resolve));
    service.kill("SIGTERM");
  }
}
