import test from "node:test";
import assert from "node:assert/strict";
import { createRouter } from "../src/router.ts";

// A tiny window: a list of history entries, a cursor, and popstate on Back.
function fakeWindow(start = "") {
  const listeners = new Map();
  const entries = [{ hash: start, state: null }];
  let at = 0;
  const win = {
    location: {
      get hash() {
        return entries[at].hash;
      },
    },
    history: {
      get state() {
        return entries[at].state;
      },
      pushState(state, _title, url) {
        entries.splice(at + 1, entries.length, { hash: url, state });
        at++;
      },
      replaceState(state, _title, url) {
        entries[at] = { hash: url, state };
      },
      back() {
        if (at > 0) {
          at--;
          queueMicrotask(() => (listeners.get("popstate") ?? new Set()).forEach((fn) => fn()));
        }
      },
    },
    addEventListener(type, fn) {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(fn);
    },
    removeEventListener(type, fn) {
      listeners.get(type)?.delete(fn);
    },
    listeners,
    entries,
    get at() {
      return at;
    },
  };
  return win;
}
const chats = { name: "chats", server: "s", account: "a" };
const chat = { name: "chat", server: "s", account: "a", thread: "t" };
const settings = { name: "settings", server: "s", account: "a" };

test("push adds an entry, replace rewrites it, and repeating the address does nothing", () => {
  const win = fakeWindow();
  const router = createRouter(win);
  router.push(chats);
  router.push(chats);
  router.push(chat);
  assert.deepEqual(win.entries.map((entry) => entry.hash), ["", "#/s/a/chats", "#/s/a/chats/t"]);
  router.replace(chats);
  assert.deepEqual(win.entries.map((entry) => entry.hash), ["", "#/s/a/chats", "#/s/a/chats"]);
  router.replace(chats);
  assert.equal(win.entries.length, 3);
  assert.equal(router.hash(), "#/s/a/chats");
});

test("subscribers hear about every change and the window listeners follow them", async () => {
  const win = fakeWindow();
  const router = createRouter(win);
  let heard = 0;
  assert.equal(win.listeners.size, 0);
  const stop = router.subscribe(() => heard++);
  assert.equal(win.listeners.get("popstate").size, 1);
  router.push(chats);
  router.replace(chat);
  assert.equal(heard, 2);
  win.history.back();
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(heard, 3, "Back reports through popstate");
  stop();
  assert.equal(win.listeners.get("popstate").size, 0);
  router.push(settings);
  assert.equal(heard, 3);
});

test("up uses real Back when the previous entry is the wanted view, else replaces", async () => {
  const win = fakeWindow("#/s/a/chats/t");
  const router = createRouter(win);
  router.push(settings);
  assert.equal(win.entries.length, 2);
  router.up(chat);
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(win.at, 0, "closing a dialog opened from the chat goes Back");
  assert.equal(win.entries.length, 2, "Forward stays available");

  // An entry opened directly has nothing to go back to: replace it.
  const direct = fakeWindow("#/s/a/settings");
  createRouter(direct).up(chat);
  assert.deepEqual(direct.entries.map((entry) => entry.hash), ["#/s/a/chats/t"]);

  // The previous entry is a different view: replace instead of stacking a duplicate.
  const other = fakeWindow("#/s/a/contacts");
  const router3 = createRouter(other);
  router3.push(settings);
  router3.up(chat);
  assert.deepEqual(other.entries.map((entry) => entry.hash), ["#/s/a/contacts", "#/s/a/chats/t"]);
  router3.up(chat);
  assert.equal(other.entries.length, 2);
});

test("back returns to where an entry was opened from, or to the fallback", async () => {
  const win = fakeWindow("#/servers");
  const router = createRouter(win);
  router.back(chats);
  assert.equal(router.hash(), "#/s/a/chats", "an entry opened directly is replaced");
  router.push({ name: "servers" });
  router.back(chat);
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(router.hash(), "#/s/a/chats");
});

test("a replaced entry keeps the place it was opened from", async () => {
  const win = fakeWindow("#/s/a/chats/t");
  const router = createRouter(win);
  router.push({ name: "media", server: "s", account: "a", thread: "t", message: "m1" });
  router.replace({ name: "media", server: "s", account: "a", thread: "t", message: "m2" });
  assert.equal(win.entries.length, 2);
  router.up(chat);
  await new Promise((resolve) => queueMicrotask(resolve));
  assert.equal(router.hash(), "#/s/a/chats/t");
  assert.equal(win.at, 0);
});
