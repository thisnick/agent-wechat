import test from "node:test";
import assert from "node:assert/strict";
import {
  formatRoute,
  isAccountRoute,
  isChatsRoute,
  isViewRoute,
  parseRoute,
  routeParams,
  routeTable,
} from "../src/routes.ts";

const samples = {
  servers: {},
  accounts: { server: "hub-1" },
  login: { server: "hub-1", account: "account-1" },
  chats: { server: "hub-1", account: "account-1" },
  chat: { server: "hub-1", account: "account-1", thread: "wxid_peer" },
  message: { server: "hub-1", account: "account-1", thread: "123@chatroom", message: "msg_9" },
  media: { server: "hub-1", account: "account-1", thread: "123@chatroom", message: "msg_9" },
  info: { server: "hub-1", account: "account-1", thread: "123@chatroom" },
  "mini-programs": { server: "hub-1", account: "account-1" },
  contacts: { server: "hub-1", account: "account-1" },
  "new-chat": { server: "hub-1", account: "account-1" },
  settings: { server: "hub-1", account: "account-1" },
};

test("every route in the table round-trips through its address", () => {
  assert.deepEqual(Object.keys(samples).sort(), routeTable.map((entry) => entry.name).sort());
  for (const entry of routeTable) {
    const route = { name: entry.name, ...samples[entry.name] };
    assert.deepEqual(Object.keys(route).slice(1), routeParams(entry.pattern), entry.pattern);
    const hash = formatRoute(route);
    assert.ok(hash.startsWith("#/"), hash);
    assert.deepEqual(parseRoute(hash), route, hash);
  }
});

test("addresses follow the published patterns", () => {
  assert.equal(formatRoute({ name: "servers" }), "#/servers");
  assert.equal(formatRoute({ name: "accounts", server: "s" }), "#/s/accounts");
  assert.equal(formatRoute({ name: "login", server: "s", account: "a" }), "#/s/a/login");
  assert.equal(formatRoute({ name: "chats", server: "s", account: "a" }), "#/s/a/chats");
  assert.equal(formatRoute({ name: "chat", server: "s", account: "a", thread: "t" }), "#/s/a/chats/t");
  assert.equal(formatRoute({ name: "message", server: "s", account: "a", thread: "t", message: "m" }), "#/s/a/chats/t/message/m");
  assert.equal(formatRoute({ name: "media", server: "s", account: "a", thread: "t", message: "m" }), "#/s/a/chats/t/media/m");
  assert.equal(formatRoute({ name: "info", server: "s", account: "a", thread: "t" }), "#/s/a/chats/t/info");
  assert.equal(formatRoute({ name: "mini-programs", server: "s", account: "a" }), "#/s/a/mini-programs");
  assert.equal(formatRoute({ name: "contacts", server: "s", account: "a" }), "#/s/a/contacts");
  assert.equal(formatRoute({ name: "new-chat", server: "s", account: "a" }), "#/s/a/new-chat");
  assert.equal(formatRoute({ name: "settings", server: "s", account: "a" }), "#/s/a/settings");
});

test("identifiers are percent-encoded, including group IDs and separators", () => {
  const hash = formatRoute({ name: "chat", server: "s", account: "a", thread: "123@chatroom" });
  assert.equal(hash, "#/s/a/chats/123%40chatroom");
  assert.equal(parseRoute(hash).thread, "123@chatroom");
  const odd = { name: "chat", server: "hub/1", account: "a b", thread: "x?y#z%/é" };
  const encoded = formatRoute(odd);
  assert.equal(encoded.split("/").length, 5, "separators inside an ID must not split the address");
  assert.ok(!/[?# ]/.test(encoded.slice(1)), encoded);
  assert.deepEqual(parseRoute(encoded), odd);
  // A hand-written address with the character left bare still reads the same.
  assert.equal(parseRoute("#/s/a/chats/123@chatroom").thread, "123@chatroom");
});

test("an identifier named like a route word is still an identifier", () => {
  assert.deepEqual(parseRoute("#/servers/accounts"), { name: "accounts", server: "servers" });
  assert.deepEqual(parseRoute("#/s/accounts/chats"), { name: "chats", server: "s", account: "accounts" });
  assert.deepEqual(parseRoute("#/s/a/chats/info"), { name: "chat", server: "s", account: "a", thread: "info" });
  assert.deepEqual(parseRoute("#/s/a/chats/chats/info"), { name: "info", server: "s", account: "a", thread: "chats" });
});

test("a trailing slash is ignored and the leading # is optional", () => {
  const route = { name: "chat", server: "s", account: "a", thread: "t" };
  assert.deepEqual(parseRoute("#/s/a/chats/t/"), route);
  assert.deepEqual(parseRoute("#/s/a/chats/t///"), route);
  assert.deepEqual(parseRoute("/s/a/chats/t"), route);
  assert.deepEqual(parseRoute("#/servers/"), { name: "servers" });
});

test("empty, unknown and malformed addresses are not routes", () => {
  for (const hash of [
    "",
    "#",
    "#/",
    "#//",
    "servers",
    "#servers",
    "#dev=abcdef",
    "#/s",
    "#/s/a",
    "#/s/a/unknown",
    "#/s/a/chats/t/unknown",
    "#/s/a/chats/t/message",
    "#/s/a/chats/t/info/extra",
    "#/s/a/chats/t/message/m/extra",
    "#/s//chats",
    "#//a/chats",
    "#/s/a//t",
    "#/s/a/chats//info",
    "#/s/a/chats/%E0%A4%A",
    "#/s/a/chats/%",
    "#/%ZZ/accounts",
    "#/s/a/chats/" + "x".repeat(2049),
    "#/s/a/Chats",
    "#/s/a/chats?x=1/y",
  ])
    assert.equal(parseRoute(hash), null, JSON.stringify(hash));
});

test("a long but plausible identifier is accepted", () => {
  const thread = "x".repeat(2048);
  assert.equal(parseRoute(`#/s/a/chats/${thread}`).thread, thread);
});

test("the table has unique patterns and names and a one-sentence description for each route", () => {
  assert.equal(new Set(routeTable.map((entry) => entry.pattern)).size, routeTable.length);
  assert.equal(new Set(routeTable.map((entry) => entry.name)).size, routeTable.length);
  for (const entry of routeTable) {
    assert.ok(entry.pattern.startsWith("/") && !entry.pattern.endsWith("/"), entry.pattern);
    assert.match(entry.description, /^[A-Z].*\.$/, entry.pattern);
    assert.ok(!entry.description.includes("\n"));
  }
});

test("route kinds separate account routes, signed-in routes and full views", () => {
  const kinds = (name) => {
    const route = { name, ...samples[name] };
    return [isAccountRoute(route), isChatsRoute(route), isViewRoute(route)];
  };
  assert.deepEqual(kinds("servers"), [false, false, false]);
  assert.deepEqual(kinds("accounts"), [false, false, false]);
  assert.deepEqual(kinds("login"), [true, false, false]);
  assert.deepEqual(kinds("settings"), [true, true, false]);
  assert.deepEqual(kinds("new-chat"), [true, true, false]);
  for (const name of ["chats", "chat", "message", "media", "info", "contacts"])
    assert.deepEqual(kinds(name), [true, true, true], name);
  assert.equal(isAccountRoute(null), false);
});
