// Pinning UI acceptance uses synthetic HTTP data and never contacts WeChat.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { reloadPage } from "./webdriver-navigation.mjs";

export async function pinningAcceptance({
  script,
  invoke,
  until,
  resize,
  key,
  capture,
}) {
  const wait = async (ready) => {
    for (let attempt = 0; attempt < 200; attempt++) {
      if (ready()) return;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    assert.ok(
      ready(),
      "fixture admission must finish before changing its operation",
    );
  };
  const account = "pin-fixture";
  const ids = ["open", "unopened", "room@chatroom", "unknown", "filehelper"];
  const rows = ids.map((id, i) => ({
    id,
    accountId: account,
    peerId: id,
    kind: id.endsWith("@chatroom") ? "group" : "direct",
    displayName: id,
    pinned: id === "unknown" ? null : false,
    unreadCount: id === "unopened" ? 9 : 0,
    readStateVersion: "v",
    history: {},
    lastMessage: {
      id: id + "-60",
      text: "Saved message",
      direction: "incoming",
      type: 1,
      createdAt: String(100 - i),
    },
  }));
  const order = () => [
    ...rows.filter((x) => x.pinned === true),
    ...rows.filter((x) => x.pinned !== true),
  ];
  const operations = new Map(),
    sockets = new Set(),
    puts = [],
    histories = [],
    reads = [];
  let mode = "confirm",
    cursor = 0,
    connection;
  const changed = (id) => {
    cursor++;
    const event = {
      cursor: String(cursor),
      accountId: account,
      kind: "thread.updated",
      data: { threadId: id, source: "phone" },
    };
    for (const socket of sockets)
      socket.write(`id: ${cursor}\ndata: ${JSON.stringify(event)}\n\n`);
  };
  const apply = (op) => {
    rows.find((x) => x.id === op.result.targetId).pinned = op.result.pinned;
    op.state = "succeeded";
    op.result.remoteOutcome = "accepted";
    op.result.profileProjection = "complete";
    changed(op.result.targetId);
  };
  const backend = createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost"),
      path = url.pathname;
    const json = (value, status = 200) => {
      res.statusCode = status;
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify(value));
    };
    const page = (items) => ({
      items,
      nextCursor: null,
      asOfEventCursor: String(cursor),
      sourceGeneration: "fixture",
      coverage: {},
    });
    if (path === "/v1/status")
      return json({
        serverId: "pinning-fixture",
        version: { backend: "native" },
      });
    if (path === "/v1/accounts")
      return json(
        page([{ id: account, alias: "Pin fixture", authenticated: true }]),
      );
    if (path.endsWith("/capabilities"))
      return json({
        threadPinningTargets: ["direct", "group"],
        threadPinningScope: "admin",
      });
    if (path === `/v1/accounts/${account}`)
      return json({ authenticated: true, contactSyncReady: true });
    if (path.endsWith("/threads")) return json(page(order()));
    if (path.endsWith("/contacts")) return json(page([]));
    if (path.endsWith("/events/stream")) {
      res.setHeader("Content-Type", "text/event-stream");
      res.write(": connected\n\n");
      sockets.add(res);
      req.on("close", () => sockets.delete(res));
      return;
    }
    if (path.endsWith("/events")) return json(page([]));
    if (path.includes("/avatars/"))
      return json({ error: { code: "media_not_cached" } }, 404);
    const matched = path.match(
      /\/threads\/([^/]+)(?:\/(messages|pin|read-state))?$/,
    );
    if (matched) {
      const id = decodeURIComponent(matched[1]),
        target = rows.find((x) => x.id === id),
        action = matched[2];
      if (!action) return json(target);
      if (action === "messages") {
        histories.push(id);
        return json(
          page(
            id === "open"
              ? Array.from({ length: 60 }, (_, i) => ({
                  id: `open-${i + 1}`,
                  accountId: account,
                  threadId: id,
                  senderId: "open",
                  direction: "incoming",
                  type: 1,
                  body: `History row ${i + 1}`,
                  createdAt: String(i + 1),
                  media: [],
                }))
              : [],
          ),
        );
      }
      if (action === "read-state") {
        reads.push(id);
        return json({});
      }
      if (action === "pin") {
        let body = "";
        for await (const chunk of req) body += chunk;
        const input = JSON.parse(body),
          admission = req.headers["idempotency-key"];
        assert.equal(typeof input.pinned, "boolean");
        assert.ok(admission);
        if (mode === "forbidden")
          return json({ error: { code: "forbidden" } }, 403);
        if (operations.has(admission)) return json(operations.get(admission));
        puts.push({ id, key: admission, pinned: input.pinned });
        const op = {
          id: `op-${puts.length}`,
          accountId: account,
          kind: "group_mutation",
          state: "succeeded",
          createdAt: "1",
          updatedAt: "1",
          result: {
            action: "pin",
            targetId: id,
            pinned: input.pinned,
            remoteOutcome: "accepted",
            profileProjection: "complete",
          },
        };
        operations.set(admission, op);
        if (mode === "pending") {
          op.result.profileProjection = "pending";
          return json(op);
        }
        if (mode === "lost") {
          op.state = "needs_review";
          op.result.remoteOutcome = "unknown";
          op.result.profileProjection = "pending";
          req.socket.destroy();
          return;
        }
        if (mode === "reject") {
          op.state = "rejected";
          op.result.remoteOutcome = "rejected";
          op.result.profileProjection = "pending";
          return json(op);
        }
        apply(op);
        return json(op);
      }
    }
    if (path.endsWith("/group-mutations/by-idempotency-key")) {
      const op = operations.get(url.searchParams.get("key"));
      return json(op ?? { error: { code: "not_found" } }, op ? 200 : 404);
    }
    if (path.includes("/operations/")) {
      const op = [...operations.values()].find((op) =>
        path.endsWith("/" + op.id),
      );
      return json(op ?? { error: { code: "not_found" } }, op ? 200 : 404);
    }
    return json({ error: { code: "not_found" } }, 404);
  });
  const row = (id) => `document.querySelector('[data-thread-id="${id}"]')`;
  const more = (id) => row(id) + `.querySelector('.thread-row-more')`;
  const click = (source) => script(`${source}.click(); return true`);
  const menu = () =>
    until("return document.querySelector('[role=menu]')?.textContent", Boolean);
  const pinItem = () =>
    "[...document.querySelectorAll('[role=menuitem]')].find(x=>/^Unpin chat$|^Pin chat$/.test(x.textContent))";
  const pinned = (id) =>
    until(
      `return Boolean(${row(id)}.querySelector('.thread-pin-icon'))`,
      Boolean,
    );
  try {
    await new Promise((resolve) => backend.listen(0, "127.0.0.1", resolve));
    connection = await invoke("desktop_remote", {
      name: "Pinning fixture",
      endpoint: "http://127.0.0.1:" + backend.address().port,
      key: "c".repeat(64),
      certificate: null,
    });
    await reloadPage(script, until);
    await until(
      "return Boolean(document.querySelector('.login-account'))",
      Boolean,
    );
    await click("document.querySelector('.login-account')");
    await until(
      "return document.querySelectorAll('article[data-message-id]').length",
      (n) => n === 60,
    );
    await resize(1200, 1000);
    await until(
      "return document.querySelector('.side-footer').textContent",
      (text) => text.includes("connected"),
    );
    await until(
      "const view=document.querySelector('.messages');return !view.hasAttribute('data-pending-scroll') && view.scrollHeight>view.clientHeight && Math.abs(view.scrollHeight-view.clientHeight-view.scrollTop)<2;",
      Boolean,
    );
    await script(
      `const text=document.querySelector('textarea');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(text,'Keep this draft');text.dispatchEvent(new Event('input',{bubbles:true}));const view=document.querySelector('.messages');view.dispatchEvent(new WheelEvent('wheel',{deltaY:-100,bubbles:true}));view.scrollTop=100;return true;`,
    );
    await until(
      "return Math.round(document.querySelector('.messages').scrollTop)",
      (n) => n === 100,
    );
    const beforeHistories = [...histories],
      beforeReads = [...reads];
    await click(more("unopened"));
    await menu();
    assert.equal(
      await script(
        "return Boolean(document.querySelector('[role=menuitem]:focus'))",
      ),
      true,
    );
    await key("\uE007"); // Enter activates the focused menu item.
    await pinned("unopened");
    assert.deepEqual(
      await script(
        "return [...document.querySelectorAll('[data-thread-id]')].map(x=>x.dataset.threadId)",
      ),
      ["unopened", "open", "room@chatroom", "unknown", "filehelper"],
    );
    assert.equal(
      await script(
        "return document.querySelector('.chat-row[aria-current=true]').closest('[data-thread-id]').dataset.threadId",
      ),
      "open",
    );
    assert.equal(
      await script("return document.querySelector('textarea').value"),
      "Keep this draft",
    );
    assert.equal(
      await script(
        "return Math.round(document.querySelector('.messages').scrollTop)",
      ),
      100,
    );
    assert.equal(
      await script(
        `return ${row("unopened")}.querySelector('.count').textContent`,
      ),
      "9",
    );
    assert.deepEqual(histories, beforeHistories);
    assert.deepEqual(reads, beforeReads);
    assert.equal(
      await script(`return document.activeElement === ${more("unopened")}`),
      true,
      "focus follows the same trigger when its row moves",
    );
    assert.equal(
      await script(
        "return document.querySelectorAll('.pinned-heading').length",
      ),
      1,
    );
    const pinGap = await script(
      `const row=${row(
        "unopened",
      )};return row.querySelector('.thread-pin-icon').getBoundingClientRect().left-row.querySelector('.chat-name').getBoundingClientRect().right;`,
    );
    assert.ok(pinGap >= 0 && pinGap <= 8, "the pin icon sits beside the name");
    assert.equal(
      await script("return document.querySelectorAll('button button').length"),
      0,
    );
    await click(
      "document.querySelector('button[aria-label=\"Conversation details\"]')",
    );
    await until(
      "return document.querySelector('[role=switch][aria-label=\"Pin chat\"]')?.getAttribute('aria-checked')",
      (value) => value === "false",
    );
    if (capture) await capture();
    await script(
      `${row("open")}.querySelector('.chat-row').focus();return true;`,
    );
    await script(
      `${row(
        "open",
      )}.querySelector('.chat-row').dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true}));return true;`,
    );
    await menu();
    await key("\uE00C"); // Escape restores this row's trigger.
    assert.equal(
      await script(`return document.activeElement === ${more("open")}`),
      true,
    );
    await key("\uE015");
    await menu();
    await until(
      "return document.activeElement?.getAttribute('role')",
      (role) => role === "menuitem",
    );
    await key("\uE007");
    await until(
      "return document.querySelector('[role=switch][aria-label=\"Pin chat\"]')?.getAttribute('aria-checked')",
      (v) => v === "true",
    );
    await click(
      "document.querySelector('[role=switch][aria-label=\"Pin chat\"]')",
    );
    await until(
      "return document.querySelector('[role=switch][aria-label=\"Pin chat\"]')?.getAttribute('aria-checked')",
      (v) => v === "false",
    );
    await click(more("unopened"));
    await menu();
    await click(pinItem());
    await until(
      "return document.querySelectorAll('.pinned-heading').length",
      (n) => n === 0,
    );
    // A phone-originated event carries only an ID; the list must refetch its state/order.
    rows.find((x) => x.id === "room@chatroom").pinned = true;
    changed("room@chatroom");
    await pinned("room@chatroom");
    assert.equal(
      await script(
        "return document.querySelector('[data-thread-id]').dataset.threadId",
      ),
      "room@chatroom",
    );
    rows.find((x) => x.id === "room@chatroom").pinned = false;
    changed("room@chatroom");
    await until(
      "return document.querySelectorAll('.thread-pin-icon').length",
      (n) => n === 0,
    );
    const beforeContext = histories.length;
    await script(
      `${row(
        "room@chatroom",
      )}.querySelector('.chat-row').dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true}));return true;`,
    );
    await menu();
    await key("\uE00C");
    assert.equal(histories.length, beforeContext);
    assert.equal(
      await script(
        "return document.querySelector('.chat-row[aria-current=true]').closest('[data-thread-id]').dataset.threadId",
      ),
      "open",
    );
    // Unknown does not claim to be an unchecked switch.
    await click(row("unknown") + ".querySelector('.chat-row')");
    await until(
      "return Boolean(document.querySelector('[aria-label=\"Pin chat: state unavailable\"]'))",
      Boolean,
    );
    assert.equal(
      await script(
        "return document.querySelector('[aria-label=\"Pin chat: state unavailable\"]').getAttribute('aria-checked')",
      ),
      null,
    );
    rows.find((x) => x.id === "unknown").pinned = false;
    await click(
      "document.querySelector('button[aria-label=\"Refresh pin state\"]')",
    );
    await until(
      "return document.querySelector('[role=switch][aria-label=\"Pin chat\"]')?.getAttribute('aria-checked')",
      (v) => v === "false",
    );
    await click(row("open") + ".querySelector('.chat-row')");
    // Accepted, locally pending: both controls stay busy until operation readback and inventory agree.
    mode = "pending";
    const beforePending = puts.length;
    await click(more("unopened"));
    await menu();
    await click(pinItem());
    await until(
      `return Boolean(${row("unopened")}.querySelector('.pin-spinner'))`,
      Boolean,
    );
    assert.equal(
      await script(
        `return Boolean(${row("unopened")}.querySelector('.thread-pin-icon'))`,
      ),
      false,
    );
    await wait(() => puts.length > beforePending);
    const pending = [...operations.values()].at(-1);
    assert.equal(pending.result.profileProjection, "pending");
    apply(pending);
    await pinned("unopened");
    await until(
      `return Boolean(${row("unopened")}.querySelector('.pin-spinner'))`,
      (v) => v === false,
    );
    mode = "confirm";
    await click(more("unopened"));
    await menu();
    await click(pinItem());
    await until(
      "return document.querySelectorAll('.thread-pin-icon').length",
      (n) => n === 0,
    );
    // Rejection retains state and permits a deliberate new attempt.
    mode = "reject";
    await click(more("unopened"));
    await menu();
    await click(pinItem());
    await until(`return ${row("unopened")}?.textContent`, (text) =>
      text.includes("wasn’t applied"),
    );
    assert.equal(rows.find((x) => x.id === "unopened").pinned, false);
    mode = "lost";
    await click(more("unopened"));
    await menu();
    await click(pinItem());
    await until(`return ${row("unopened")}?.textContent`, (text) =>
      text?.includes("Check status"),
    );
    const lost = [...operations.values()].at(-1),
      submissions = puts.length;
    await reloadPage(script, until);
    await until(`return ${row("unopened")}?.textContent`, (text) =>
      text?.includes("Check status"),
    );
    assert.equal(
      puts.length,
      submissions,
      "reload must recover, never resubmit",
    );
    apply(lost);
    await click(row("unopened") + ".querySelector('.pin-feedback button')");
    await pinned("unopened");
    await until(
      `return Boolean(${row("unopened")}.querySelector('.pin-spinner'))`,
      (v) => v === false,
    );
    assert.equal(puts.length, submissions);
    // The overflow is visible and has a touch target without requiring hover.
    await resize(420, 820);
    await click(
      "document.querySelector('button[aria-label=\"Back to conversations\"]')",
    );
    const hit = await script(
      `const el=${more(
        "room@chatroom",
      )};const r=el.getBoundingClientRect();return {width:r.width,height:r.height,display:getComputedStyle(el).display};`,
    );
    assert.ok(hit.width >= 44 && hit.height >= 44 && hit.display !== "none");
    mode = "confirm";
    await click(more("room@chatroom"));
    await menu();
    await click(pinItem());
    await pinned("room@chatroom");
    await click(more("filehelper"));
    await menu();
    assert.equal(await script(`return ${pinItem()}.disabled`), true);
    await key("\uE00C");
    console.log(
      "Pinning interface: shared row/details state, keyboard/context/touch access, server order, unknown state, pending projection, rejection, reload recovery, phone updates, stable focus/drafts/history/unreads/scroll passed",
    );
  } finally {
    if (connection)
      await invoke("desktop_forget", { id: connection.id }).catch(() => {});
    await invoke("desktop_select", { id: "local" });
    await reloadPage(script, until);
    backend.closeAllConnections();
    await new Promise((resolve) => backend.close(resolve));
  }
}
