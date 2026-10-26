// Native WebView regression checks against synthetic HTTP data, never WeChat.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { browserNavigationAcceptance } from "./browser-navigation.mjs";

export async function interfaceAcceptance({
  script,
  invoke,
  until,
  resize,
  capture,
}) {
  const png = Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAFElEQVR4nGPUrZjGgA0wYRUdtBIA+kEBS3hZwQIAAAAASUVORK5CYII=",
    "base64",
  );
  const peers = ["first", "second"];
  const threads = peers.map((id) => ({
    id,
    accountId: "fixture",
    peerId: id,
    kind: "direct",
    displayName: id,
    unreadCount: 0,
    muted: false,
    readStateVersion: "0",
    history: {},
  }));
  let avatarReads = 0;
  let originalLookups = 0;
  const base = {
    accountId: "fixture",
    threadId: "first",
    senderId: "first",
    direction: "incoming",
    createdAt: "2026-10-26T12:00:00Z",
    media: [],
  };
  const link = {
    title: "Shared video title",
    description: "Card description",
    url: "https://example.com/article",
    sourceName: "Example",
  };
  const messages = [
    {
      ...base,
      id: "link-original",
      serverId: "101",
      type: 49,
      appMessageType: 4,
      link,
    },
    {
      ...base,
      id: "link-reply",
      serverId: "102",
      type: 49,
      appMessageType: 57,
      body: "About this link",
      quote: {
        serverId: "101",
        type: 49,
        appMessageType: 4,
        state: "embedded",
        senderId: "first",
        embeddedLink: link,
      },
    },
    {
      ...base,
      id: "sticker-original",
      serverId: "103",
      type: 49,
      appMessageType: 8,
    },
    {
      ...base,
      id: "sticker-reply",
      serverId: "104",
      type: 49,
      appMessageType: 57,
      body: "About this sticker",
      quote: {
        serverId: "103",
        type: 49,
        appMessageType: 8,
        state: "embedded",
        senderId: "first",
        embeddedSticker: true,
      },
    },
  ];
  const assets = messages.map((message, i) => ({
    id: "fixture-asset-" + i,
    messageId: message.id,
    accountId: "fixture",
    kind: i < 2 ? "image" : "sticker",
    variant: i < 2 ? "thumbnail" : "original",
    actualVariant: i < 2 ? "thumbnail" : "original",
    state: "available",
    contentType: "image/png",
    byteLength: png.length,
    sha256: createHash("sha256").update(png).digest("hex"),
    retryable: false,
  }));
  const backend = createServer(async (req, res) => {
    const path = new URL(req.url, "http://localhost").pathname;
    if (path === "/browser" || /^\/assets\/[\w.-]+$/.test(path)) {
      const file = path === "/browser" ? "/index.html" : path;
      res.setHeader(
        "Content-Type",
        path === "/browser"
          ? "text/html"
          : path.endsWith(".css")
          ? "text/css"
          : "text/javascript",
      );
      return res.end(
        await readFile(new URL("../../client-ui/dist" + file, import.meta.url)),
      );
    }
    res.setHeader("Content-Type", "application/json");
    const json = (value) => res.end(JSON.stringify(value));
    if (path === "/v1/status")
      return json({
        serverId: "interface-fixture",
        version: { backend: "native" },
      });
    if (path === "/v1/accounts")
      return json({
        items: [
          { id: "fixture", alias: "Interface fixture", authenticated: true },
        ],
      });
    if (path === "/v1/accounts/fixture")
      return json({ authenticated: true, contactSyncReady: true });
    if (path.endsWith("/capabilities")) return json({});
    if (path.endsWith("/threads"))
      return json({ items: threads, nextCursor: null, asOfEventCursor: "0" });
    if (path.endsWith("/threads/first/messages"))
      return json({ items: messages, nextCursor: null, asOfEventCursor: "0" });
    if (path.endsWith("/contacts") || path.endsWith("/messages"))
      return json({ items: [], nextCursor: null, asOfEventCursor: "0" });
    if (/\/messages\/[^/]+$/.test(path)) originalLookups++;
    if (path.endsWith("/read-state")) return json({});
    const messageAssets = assets.filter((asset) =>
      path.endsWith(`/messages/${asset.messageId}/media`),
    );
    if (messageAssets.length) return json({ items: messageAssets });
    if (assets.some((asset) => path.endsWith(`/media/${asset.id}/content`))) {
      res.setHeader("Content-Type", "image/png");
      return res.end(png);
    }
    if (path.endsWith("/events/stream")) {
      res.setHeader("Content-Type", "text/event-stream");
      res.write(": ready\n\n");
      return;
    }
    if (peers.some((id) => path.endsWith("/avatars/" + id))) {
      avatarReads++;
      res.setHeader("Content-Type", "image/png");
      return res.end(png);
    }
    const thread = threads.find((item) => path.endsWith("/threads/" + item.id));
    if (thread) return json(thread);
    res.statusCode = 404;
    json({ error: { code: "not_found" } });
  });
  let connection;
  const click = (text) =>
    script(
      `[...document.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(
        text,
      )}).click(); return true`,
    );
  const avatars =
    "return [...document.querySelectorAll('.conversation-avatar img')].map(im => im.naturalWidth)";
  const imagesReady = (widths) =>
    widths.length >= 2 && widths.every((width) => width === 8);
  try {
    await new Promise((resolve) => backend.listen(0, "127.0.0.1", resolve));
    connection = await invoke("desktop_remote", {
      name: "Interface fixture hub",
      endpoint: "http://127.0.0.1:" + backend.address().port,
      key: "a".repeat(64),
      certificate: null,
    });
    await script("window.location.reload(); return true");
    await until(
      "return Boolean(document.querySelector('.login-account'))",
      Boolean,
    );
    await script(
      "document.querySelector('.login-account').click(); return true",
    );
    await until(avatars, imagesReady);
    assert.equal(avatarReads, 2);
    await script("window.location.reload(); return true");
    await until(avatars, imagesReady);
    assert.equal(
      avatarReads,
      2,
      "restart must reuse readable local image bytes",
    );
    // Recreate the older storage format to check upgrade recovery in real WebKit.
    await script(`return new Promise((resolve, reject) => {
      const open = indexedDB.open('agent-wechat-cache');
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result, tx = db.transaction('avatars', 'readwrite');
        const request = tx.objectStore('avatars').getAll();
        request.onsuccess = () => {
          for (const row of request.result) {
            if (!row.data.content) continue;
            row.data.blob = new Blob([row.data.content], { type: row.data.contentType });
            delete row.data.content; delete row.data.contentType;
            tx.objectStore('avatars').put(row);
          }
        };
        tx.oncomplete = () => { db.close(); resolve(true); };
        tx.onabort = () => reject(tx.error);
      };
    })`);
    await script("window.location.reload(); return true");
    await until(avatars, imagesReady);
    assert.ok(
      avatarReads <= 4,
      "legacy recovery must fetch cached bytes at most once per avatar",
    );
    await resize(1200, 1000);
    await script("document.querySelector('.chat-row').click(); return true");
    await until(
      "return [...document.querySelectorAll('article[data-message-id]')].map(el => el.dataset.messageId)",
      (ids) => messages.every((message) => ids.includes(message.id)),
    );
    for (const id of messages.map((message) => message.id)) {
      await script(
        `document.querySelector('[data-message-id="${id}"]').scrollIntoView({block:'center'}); return true`,
      );
      await until(
        `return document.querySelector('[data-message-id="${id}"] img.media-preview, [data-message-id="${id}"] .shared-link-art img')?.naturalWidth`,
        (width) => width === 8,
      );
    }
    const compact =
      await script(`const card = document.querySelector('.compact-link-card');
      return {text:card.innerText, links:document.querySelectorAll('.quote-preview a, .quote-preview button').length,
      width:card.getBoundingClientRect().width, bubble:card.closest('.bubble').getBoundingClientRect().width,
      stickers:document.querySelectorAll('.sticker-card img').length};`);
    assert.ok(
      compact.text.includes(link.title) && compact.text.includes("example.com"),
    );
    assert.ok(
      !compact.text.includes(link.description) &&
        !compact.text.includes(link.url),
    );
    assert.equal(
      compact.links,
      0,
      "quote previews must have a single navigation target",
    );
    assert.ok(
      compact.width <= compact.bubble,
      "compact cards must fit their reply bubble",
    );
    assert.equal(
      compact.stickers,
      2,
      "shared sticker and embedded quote must both render",
    );
    assert.equal(
      originalLookups,
      0,
      "rendering quotes must not fetch original messages",
    );
    if (capture) await capture();
    await resize(420, 820);
    const bounds =
      await script(`return [...document.querySelectorAll('.quote-preview')].map(el => {
      const rect = el.getBoundingClientRect(); return {right:rect.right, width:innerWidth}; });`);
    assert.ok(
      bounds.every((rect) => rect.right <= rect.width),
      "quote previews must fit narrow windows",
    );
    await resize(1200, 1000);
    await script(
      "document.querySelector('[data-message-id=link-reply] .quote-preview').click(); return true",
    );
    await until(
      "return document.querySelector('.quote-target')?.dataset.messageId",
      (id) => id === "link-original",
    );
    await script(
      "document.querySelector('button[aria-label=\"Settings\"]').click(); return true",
    );
    await click("Switch hub");
    await until("return document.body.innerText", (text) =>
      text.includes("Welcome to AgentWechat"),
    );
    await click("Desktop settings");
    await until("return document.body.innerText", (text) =>
      text.includes("Interface fixture hub"),
    );
    let graphicHeight;
    for (const width of [900, 1200]) {
      await resize(width, 650);
      const layout =
        await script(`const panel = document.querySelector('.onboarding-main');
        panel.scrollTop = panel.scrollHeight;
        const back = [...panel.querySelectorAll('button')].find(b => b.textContent === 'Back');
        const rect = back.getBoundingClientRect();
        return { scrollable: panel.scrollHeight > panel.clientHeight, top: rect.top, bottom: rect.bottom, height: innerHeight, graphic: document.querySelector('.app-illustration').getBoundingClientRect().height };`);
      assert.ok(
        layout.scrollable,
        "long settings must scroll inside their panel",
      );
      assert.ok(
        layout.top >= 0 && layout.bottom <= layout.height,
        "Back must be reachable",
      );
      if (graphicHeight !== undefined)
        assert.ok(
          Math.abs(layout.graphic - graphicHeight) < 1,
          "illustration size must follow height, not width",
        );
      graphicHeight = layout.graphic;
    }
    await script(
      "[...document.querySelectorAll('.setting')].find(row => row.textContent.includes('Interface fixture hub')).querySelector('button').click(); return true",
    );
    await until(
      "return document.body.innerText",
      (text) => !text.includes("Interface fixture hub"),
    );
    await click("Back");
    await until(
      "return [...document.querySelectorAll('.desktop-choices button')].map(b => b.textContent)",
      (labels) =>
        labels.length > 0 &&
        !labels.some((label) => label.includes("Interface fixture hub")),
    );
    await click("Connect to another hub");
    assert.equal(
      await script(
        "return Boolean(document.querySelector('.desktop-certificate'))",
      ),
      false,
    );
    const borders = await script(
      "const input = document.querySelector('input[type=url]'); return {width:getComputedStyle(input).borderTopWidth,background:getComputedStyle(input).backgroundColor}",
    );
    assert.equal(borders.width, "1px");
    assert.notEqual(borders.background, "rgba(0, 0, 0, 0)");
    await script(
      "const input=document.querySelector('input[type=url]'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'https://hub.example'); input.dispatchEvent(new Event('input',{bubbles:true})); return true",
    );
    await until(
      "return Boolean(document.querySelector('.desktop-certificate:not([open])'))",
      Boolean,
    );
    await click("Back");
    await invoke("desktop_select", { id: "local" });
    await script("window.location.reload(); return true");
    await until("return document.body.innerText", (text) =>
      text.includes("Add WeChat account"),
    );
    await browserNavigationAcceptance(
      "http://127.0.0.1:" + backend.address().port,
    );
    console.log(
      "Native interface: avatars survive reload and legacy storage, forgotten hubs refresh, settings scroll and certificate fields stay optional",
    );
  } finally {
    if (connection)
      await invoke("desktop_forget", { id: connection.id }).catch(() => {});
    backend.closeAllConnections();
    await new Promise((resolve) => backend.close(resolve));
  }
}
