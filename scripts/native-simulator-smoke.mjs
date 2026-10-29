// Real hub processes and the existing SDK; all account/profile/upstream state is synthetic.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import { zstdDecompressSync } from "node:zlib";
import { NativeWeChatClient } from "../packages/shared/src/native-client.ts";
import { assertSameUnavailableMedia } from "./native-ci-media-assertions.mjs";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binary = process.env.WX_DEV_BINARY ?? join(repository, "packages/native-client-rust/target/debug", process.platform === "win32" ? "wx-dev.exe" : "wx-dev");
const cliBinary = process.env.WX_BINARY ?? join(dirname(binary), process.platform === "win32" ? "wx.exe" : "wx");
const execute = promisify(execFile);
const temporary = await mkdtemp(join(tmpdir(), "agent-wechat-simulator-"));
const data = join(temporary, "synthetic hub state");
const processes = new Set();
const subscriptions = [];
let remote;
let hub;
let a;
let b;
let controlToken;

async function waitFor(label, condition, timeout = 20000) {
  const deadline = Date.now() + timeout;
  let lastError;
  while (Date.now() < deadline) {
    try { const value = await condition(); if (value) return value; }
    catch (error) { lastError = error; }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(`Timed out: ${label}`, { cause: lastError });
}
async function start(command, args = [], directory = data) {
  const child = spawn(binary, [command, ...(directory ? ["--data-dir", directory] : []), ...args], { stdio: ["ignore", "pipe", "pipe"] });
  processes.add(child);
  child.once("exit", () => processes.delete(child));
  let diagnostics = "";
  child.stderr.on("data", chunk => { diagnostics = (diagnostics + chunk).slice(-4000); });
  const ready = await new Promise((resolve, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error(`Startup timeout: ${command}`)), 30000);
    child.once("error", error => { clearTimeout(timer); reject(error); });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`${command} exited ${code}: ${diagnostics}`)); });
    child.stdout.on("data", chunk => {
      output += chunk;
      if (output.includes("\n")) {
        clearTimeout(timer);
        try { resolve(JSON.parse(output.slice(0, output.indexOf("\n")))); } catch (error) { reject(error); }
      }
    });
  });
  assert.equal(ready.state, "ready");
  return { child, ready };
}
async function control(input) {
  const response = await fetch(`http://${remote.ready.upstream}/control`, {
    method: "POST", headers: { Authorization: `Bearer ${controlToken}`, "Content-Type": "application/json" },
    body: JSON.stringify(input), signal: AbortSignal.timeout(5000),
  });
  assert.equal(response.status, 200, `Control ${input.op}: ${await response.clone().text()}`);
  return response.json();
}
async function remoteState() {
  const response = await fetch(`http://${remote.ready.upstream}/state`, { headers: { Authorization: `Bearer ${controlToken}` } });
  assert.equal(response.status, 200);
  return response.json();
}
async function attach() {
  const token = (await readFile(hub.ready.tokenFile, "utf8")).trim();
  a = new NativeWeChatClient({ baseUrl: hub.ready.endpoint, token });
  b = new NativeWeChatClient({ baseUrl: hub.ready.endpoint, token });
  const status = await a.status();
  assert.equal(status.version.backend, "simulator");
  assert.equal(status.version.nativeLogin, false);
  return status;
}
async function runCli(args) {
  const { stdout } = await execute(cliBinary, ["--json", "--config", join(temporary, "client", "client.toml"), ...args], {
    timeout: 10000,
    env: Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith("AGENT_WECHAT_"))),
  });
  return JSON.parse(stdout);
}
async function cli(...args) {
  return runCli(["--server", hub.ready.endpoint, "--token-file", hub.ready.tokenFile, "--account", "personal", ...args]);
}
async function login(account, scanned = false) {
  await a.createAccount(account);
  const operation = await a.login(account, { strategy: "qr", qrRefresh: { mode: "manual", maxRefreshes: 0 } }, `login-${account}`);
  await waitFor(`${account} QR ready`, async () => (await a.operation(account, operation.id)).state === "awaiting_scan");
  const png = await a.qr(account, operation.id);
  assert.deepEqual([...png.subarray(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
  if (scanned) {
    await control({ op: "login", account, status: "scanned" });
    await waitFor(`${account} awaiting approval`, async () => (await a.operation(account, operation.id)).state === "awaiting_approval");
  }
  await control({ op: "login", account, status: "approved" });
  await waitFor(`${account} signed in`, async () => {
    const current = await a.operation(account, operation.id);
    if (["failed", "expired", "rejected"].includes(current.state)) throw new Error(JSON.stringify(current));
    return current.state === "succeeded";
  });
  await waitFor(`${account} push connected`, async () => (await a.account(account)).incomingTransport === "simulator_push");
  await waitFor(`${account} baseline`, async () => (await a.messages(account, "sim_mei")).items.length === 1);
  assert.ok((await a.contacts(account)).items.some(contact => contact.id === "sim_mei"));
  for (const path of ["threads", "threads/sim_mei/messages"]) {
    const response = await fetch(`${hub.ready.endpoint}/v1/accounts/${account}/${path}`, {
      headers: { Authorization: `Bearer ${(await readFile(hub.ready.tokenFile, "utf8")).trim()}` },
    });
    assert.equal(response.status, 200);
    const timing = response.headers.get("server-timing");
    assert.match(timing, /read_queue;dur=\d+\.\d+/);
    assert.match(timing, /account_wait;dur=\d+\.\d+/);
    assert.match(timing, /snapshot;dur=\d+\.\d+/);
    assert.match(timing, /query;dur=\d+\.\d+/);
    assert.match(timing, /read_mode;desc="(?:parallel|serialized)"/);
    await response.json();
  }
}
function subscribe(client, account, cursor = "0") {
  const controller = new AbortController();
  const events = [];
  const subscription = { controller, events, error: null, done: null };
  subscription.done = (async () => {
    try { for await (const event of client.watch(account, cursor, controller.signal)) events.push(event); }
    catch (error) { if (!controller.signal.aborted) subscription.error = error; }
  })();
  subscriptions.push(subscription);
  return subscription;
}
async function unsubscribe(subscription) { subscription.controller.abort(); await subscription.done; }
async function committed(subscription, direction, after = "0") {
  return waitFor(`${direction} SSE message`, () => {
    if (subscription.error) throw subscription.error;
    return subscription.events.find(event => event.kind === "message.committed" && event.data.direction === direction && BigInt(event.cursor) > BigInt(after));
  });
}
async function stopHub() {
  const status = await a.status();
  await a.request("v1/daemon/shutdown", { method: "POST", body: JSON.stringify({ pid: status.pid, serverId: status.serverId, ownershipGeneration: status.ownershipGeneration }) });
  await waitFor("hub shutdown", () => hub.child.exitCode !== null || hub.child.signalCode !== null);
}
async function finishOperation(label, operation) {
  return waitFor(label, async () => {
    const current = await a.operation("personal", operation.id);
    if (["failed", "needs_review", "cancelled"].includes(current.state)) throw new Error(`${label}: ${JSON.stringify(current)}`);
    return current.state === "succeeded" && current;
  });
}
async function pinJourney(streams) {
  const created = await finishOperation("create pin regression group", await a.createGroup("personal", {
    members: ["sim_mei", "sim_daniel"], name: "Simulator pin regression",
  }, "pin-regression-create"));
  const room = created.result.groupId;
  assert.equal((await a.group("personal", room)).canMentionAll, true);
  await finishOperation("refresh owner and administrator metadata", await a.refreshGroupMembers("personal", room));
  const groupInfo = await b.group("personal", room);
  assert.deepEqual(groupInfo.administratorIds, []);
  assert.equal(groupInfo.canMentionAll, true);
  assert.equal((await cli("groups", "get", room)).canMentionAll, true);
  await waitFor("pin regression group thread", async () => (await b.thread("personal", room)).pinned === false);
  await waitFor("both clients see created group", () => streams.every(stream => stream.events.some(event => event.kind === "message.committed" && event.data.threadId === room)));
  for (const threadId of ["sim_mei", room]) {
    const key = `pin-cli-${threadId}`;
    await finishOperation("mute before pin", await a.setThreadMuted("personal", threadId, true, `pin-mute-${threadId}`));
    const pinned = await cli("chats", "pin", threadId, "--idempotency-key", key);
    assert.equal(pinned.state, "succeeded");
    assert.equal((await b.setThreadPinned("personal", threadId, true, key)).id, pinned.id);
    await finishOperation("refresh pinned remote contact", await a.refreshContact("personal", threadId));
    let thread = await b.thread("personal", threadId);
    assert.equal(thread.pinned, true);
    assert.equal(thread.muted, true);
    assert.equal((await b.threads("personal")).items.find(item => item.id === threadId).pinned, true);
    await waitFor("both clients see committed pin event", () => streams.every(stream => stream.events.some(event => event.kind === "thread.updated" && event.data.threadId === threadId && event.data.operationId === pinned.id)));

    // A separate full-contact notification write must keep the saved pin bit.
    await finishOperation("unmute pinned thread", await a.setThreadMuted("personal", threadId, false, `pin-unmute-${threadId}`));
    await finishOperation("refresh unmuted pinned contact", await a.refreshContact("personal", threadId));
    thread = await b.thread("personal", threadId);
    assert.equal(thread.pinned, true);
    assert.equal(thread.muted, false);
    await finishOperation("API unpin", await a.setThreadPinned("personal", threadId, false, `unpin-api-${threadId}`));
    await finishOperation("refresh unpinned remote contact", await a.refreshContact("personal", threadId));
    assert.equal((await b.thread("personal", threadId)).pinned, false);
    await finishOperation("API repin", await a.setThreadPinned("personal", threadId, true, `repin-api-${threadId}`));
    const unpinned = await cli("chats", "unpin", threadId, "--idempotency-key", `unpin-cli-${threadId}`);
    assert.equal(unpinned.state, "succeeded");
    await finishOperation("refresh CLI unpin", await a.refreshContact("personal", threadId));
    thread = await b.thread("personal", threadId);
    assert.equal(thread.pinned, false);
    assert.equal(thread.muted, false);
    // Reopening the original accepted intent cannot submit that old pin again.
    assert.equal((await b.setThreadPinned("personal", threadId, true, key)).id, pinned.id);
    await finishOperation("refresh after original pin receipt replay", await a.refreshContact("personal", threadId));
    assert.equal((await b.thread("personal", threadId)).pinned, false);
  }
  await assert.rejects(a.setThreadPinned("personal", "filehelper", true, "pin-unsupported-filehelper"), error => error.status === 409 && ["ordinary_contact_required", "ordinary_thread_required"].includes(error.code));
  await finishOperation("leave pin regression group", await a.leaveGroup("personal", room, "pin-regression-leave"));
  assert.equal((await b.group("personal", room)).canMentionAll, false);
  await assert.rejects(a.sendText("personal", room, "Synthetic rejected mention", "all-after-leave", {
    mentions: { all: true },
  }), error => error.status === 403 && error.code === "mention_all_forbidden");
  await assert.rejects(a.sendByKey("personal", "all-after-leave"), error => error.status === 404);
}


// This journey owns a separate synthetic account so send counts and lost-ACK
// faults cannot affect the existing personal/work account acceptance checks.
async function quoteMentionJourney() {
  const account = "quote_mentions";
  await login(account);
  const creation = await a.createGroup(account, { members: ["sim_mei", "sim_daniel"], name: "Synthetic quoted mentions" }, "quote-mentions-create");
  const created = await waitFor("quoted mention group", async () => {
    const operation = await a.operation(account, creation.id);
    if (["failed", "needs_review", "cancelled"].includes(operation.state)) throw new Error(JSON.stringify(operation));
    return operation.state === "succeeded" && operation;
  });
  const room = created.result.groupId;
  assert.equal((await a.group(account, room)).canMentionAll, true);
  const settle = async (intent, state = "accepted") => waitFor(`quoted mention send ${state}`, async () => {
    const current = await a.send(account, intent.id);
    if (["failed", "cancelled"].includes(current.state)) throw new Error(JSON.stringify(current));
    // Acceptance precedes the local projection receipt used by message reads.
    return current.state === state && (state !== "accepted" || current.localMessageId) && current;
  });
  const source = await settle(await a.sendText(account, room, "Original synthetic quote target", "quote-mentions-source"));
  const original = await a.message(account, source.localMessageId);
  const quote = { messageId: original.id, serverId: original.serverId };
  const cliSend = (text, key, mentions, reference = quote, threadId = room) => runCli(["--server", hub.ready.endpoint, "--token-file", hub.ready.tokenFile, "--account", account,
    "messages", "send", threadId, "--text", text, "--quote-message-id", reference.messageId, "--quote-server-id", reference.serverId,
    ...(mentions ? mentions.all ? ["--mention-all"] : mentions.userIds.flatMap(id => ["--mention", id]) : []), "--idempotency-key", key, "--detach"]);
  const accepted = [];
  for (const [index, path, text, mentions] of [
    [0, "sdk", "SDK quoted members", { userIds: ["sim_mei", "sim_daniel"] }],
    [1, "sdk", "Hi @old name\u2005SDK quoted inline member", { userIds: ["sim_mei"], positions: [{ start: 3, end: 13, index: 0 }] }],
    [2, "sdk", "SDK quoted everyone", { all: true }],
    [3, "cli", "CLI quoted member", { userIds: ["sim_mei"] }],
    [4, "cli", "CLI quoted everyone", { all: true }],
  ]) {
    const key = `quote-mentions-${index}`;
    const intent = await settle(path === "sdk" ? await a.sendText(account, room, text, key, { quote, mentions }) : await cliSend(text, key, mentions));
    const message = await b.message(account, intent.localMessageId);
    assert.equal(message.type, 49);
    assert.equal(message.appMessageType, 57);
    assert.equal(message.quote.serverId, quote.serverId);
    assert.equal(message.quote.embeddedText, original.body);
    assert.equal(message.mentions.state, "known");
    if (mentions.all) assert.equal(message.mentions.all, true);
    else assert.deepEqual(message.mentions.userIds, mentions.userIds);
    assert.ok(message.body.includes(path === "sdk" && mentions.positions ? "SDK quoted inline member" : text));
    assert.ok(!message.body.includes("<appmsg>"));
    assert.equal((await b.sendText(account, room, text, key, { quote, mentions })).id, intent.id);
    if (path === "cli") assert.equal((await cliSend(text, key, mentions)).id, intent.id);
    accepted.push({ key, id: intent.id, localMessageId: intent.localMessageId, threadId: room, quote, quoteType: 1, mentions });
  }
  // The media control emits a direct incoming message without a group-member
  // envelope. Quote its stored descriptor without requesting sticker bytes.
  const stickerPeer = "sim_mei";
  await control({ op: "receive_media", account, peer: stickerPeer, kind: "sticker" });
  const sticker = await waitFor("stored synthetic sticker quote target", async () =>
    (await a.messages(account, stickerPeer)).items.find(message => message.type === 47));
  const stickerQuote = { messageId: sticker.id, serverId: sticker.serverId };
  for (const path of ["sdk", "cli"]) {
    const text = `${path} quoted sticker reference`, key = `quote-sticker-${path}`;
    const intent = await settle(path === "sdk" ? await a.sendText(account, stickerPeer, text, key, { quote: stickerQuote })
      : await cliSend(text, key, undefined, stickerQuote, stickerPeer));
    const message = await b.message(account, intent.localMessageId);
    assert.equal(message.threadId, stickerPeer);
    assert.equal(message.type, 49);
    assert.equal(message.appMessageType, 57);
    assert.equal(message.quote.type, 47);
    assert.equal(message.quote.serverId, stickerQuote.serverId);
    assert.equal(message.body, text);
    assert.equal((await b.sendByKey(account, key)).id, intent.id);
    assert.equal((await b.sendText(account, stickerPeer, text, key, { quote: stickerQuote })).id, intent.id);
    if (path === "cli") assert.equal((await cliSend(text, key, undefined, stickerQuote, stickerPeer)).id, intent.id);
    accepted.push({ key, id: intent.id, localMessageId: intent.localMessageId, threadId: stickerPeer, quote: stickerQuote, quoteType: 47 });
  }
  const uncertain = [];
  for (const path of ["sdk", "cli"]) {
    const text = `Lost acknowledgment for ${path} quoted mentions`, key = `quote-mentions-lost-${path}`, mentions = { userIds: ["sim_mei"] };
    await control({ op: "send_fault", account, fault: "lost_acknowledgment" });
    const intent = await settle(path === "sdk" ? await a.sendText(account, room, text, key, { quote, mentions }) : await cliSend(text, key, mentions), "needs_review");
    assert.equal((await b.sendByKey(account, key)).id, intent.id);
    assert.equal((await b.sendText(account, room, text, key, { quote, mentions })).id, intent.id);
    if (path === "cli") assert.equal((await cliSend(text, key, mentions)).id, intent.id);
    uncertain.push({ key, id: intent.id });
  }
  // The remote accepted each logical send exactly once, including lost ACKs.
  const remoteAccount = (await remoteState()).accounts.find(item => item.id === account);
  assert.equal(remoteAccount.acceptedSends, 10);
  assert.equal(remoteAccount.acceptedMediaSends, 0);
  return { account, quote, accepted, uncertain };
}
async function verifyQuoteMentionRestart(journey) {
  for (const retained of journey.accepted) {
    const intent = await b.sendByKey(journey.account, retained.key);
    assert.equal(intent.id, retained.id);
    const message = await b.message(journey.account, retained.localMessageId);
    assert.equal(message.threadId, retained.threadId);
    assert.equal(message.type, 49);
    assert.equal(message.quote.serverId, retained.quote.serverId);
    assert.equal(message.quote.type, retained.quoteType);
    assert.equal(message.appMessageType, 57);
    if (retained.mentions?.all) assert.equal(message.mentions.all, true);
    else if (retained.mentions) assert.deepEqual(message.mentions.userIds, retained.mentions.userIds);
  }
  for (const retained of journey.uncertain) {
    const intent = await b.sendByKey(journey.account, retained.key);
    assert.equal(intent.id, retained.id);
    assert.equal(intent.state, "needs_review");
  }
  const remoteAccount = (await remoteState()).accounts.find(item => item.id === journey.account);
  assert.equal(remoteAccount.acceptedSends, 10);
  assert.equal(remoteAccount.acceptedMediaSends, 0);
}

try {
  remote = await start("upstream");
  controlToken = (await readFile(remote.ready.controlTokenFile, "utf8")).trim();
  assert.equal((await fetch(`http://${remote.ready.upstream}/state`)).status, 401);
  hub = await start("hub", ["--upstream", remote.ready.upstream]);
  await runCli(["config", "server", "add", "synthetic", "--url", hub.ready.endpoint, "--token-file", hub.ready.tokenFile]);
  const initialStatus = await attach();
  await login("personal", true);
  const capabilities = await a.capabilities("personal");
  assert.equal(capabilities.upstream, "simulator");
  assert.deepEqual(capabilities.mediaKinds, ["image", "file", "video", "voice"]);
  await assert.rejects(a.prepareOfficialProfile({ path: "/not-a-real-profile" }), error => error.code === "unsupported_simulator_operation");

  let streamA = subscribe(a, "personal");
  let streamB = subscribe(b, "personal");
  await waitFor("both event subscriptions", () => streamA.events.length > 0 && streamB.events.length > 0);
  const initialRemoteHead = (await remoteState()).accounts.find(item => item.id === "personal").head;
  const send = await a.sendText("personal", "sim_mei", "Sent from client A", "send-from-a");
  await waitFor("accepted text with local history ID", async () => {
    const intent = await a.send("personal", send.id);
    return intent.state === "accepted" && intent.localMessageId;
  });
  const eventA = await committed(streamA, "outgoing");
  const eventB = await committed(streamB, "outgoing");
  assert.equal(eventA.data.messageId, eventB.data.messageId);
  assert.equal(eventA.data.origin, "local_send");
  assert.equal(eventB.data.origin, "local_send");
  const message = await b.message("personal", eventB.data.messageId);
  assert.equal(message.body, "Sent from client A");
  assert.equal(message.direction, "outgoing");
  assert.ok(BigInt(message.serverId) > BigInt(Number.MAX_SAFE_INTEGER));
  for (const stream of [streamA, streamB]) {
    assert.ok(stream.events.some(event => event.kind === "send.updated" && event.data.id === send.id && event.data.state === "queued"));
    await waitFor("both clients see acceptance", () => stream.events.some(event => event.kind === "send.updated" && event.data.id === send.id && event.data.state === "accepted"));
  }
  assert.equal((await b.sendText("personal", "sim_mei", "Sent from client A", "send-from-a")).id, send.id);
  assert.equal((await remoteState()).accounts.find(item => item.id === "personal").head, initialRemoteHead);
  const beforeEcho = streamA.events.at(-1).cursor;
  await control({ op: "echo_send", account: "personal", client_id: Number(send.clientId) });
  await control({ op: "notify", account: "personal", count: 2 });
  await waitFor("explicit fault-injected echo caught up", () => streamA.events.some(event => event.kind === "sync.received" && BigInt(event.cursor) > BigInt(beforeEcho)));
  assert.equal((await a.messages("personal", "sim_mei")).items.filter(item => item.body === "Sent from client A").length, 1);
  assert.equal((await remoteState()).accounts.find(item => item.id === "personal").acceptedSends, 1);

  const beforeSelfSync = streamA.events.at(-1).cursor;
  await control({ op: "inbound", account: "personal", peer: "filehelper", body: "Synthetic send from another device", self_sent: true });
  const selfA = await committed(streamA, "outgoing", beforeSelfSync);
  const selfB = await committed(streamB, "outgoing", beforeSelfSync);
  assert.equal(selfA.data.messageId, selfB.data.messageId);
  assert.equal(selfA.data.origin, "live");
  assert.equal(selfB.data.origin, "live");
  const selfMessage = await b.message("personal", selfB.data.messageId);
  assert.equal(selfMessage.body, "Synthetic send from another device");
  assert.equal(selfMessage.threadId, "filehelper");
  assert.equal(selfMessage.senderId, "sim_personal");
  assert.equal((await b.thread("personal", "filehelper")).unreadCount, 0);
  assert.equal((await cli("status")).serverId, initialStatus.serverId);
  assert.equal((await cli("messages", "get", "filehelper", selfMessage.id)).body, selfMessage.body);

  await pinJourney([streamA, streamB]);
  const quoteMentionReceipts = await quoteMentionJourney();

  const beforeInbound = streamA.events.at(-1).cursor;
  await control({ op: "inbound", account: "personal", peer: "sim_mei", body: "Incoming through real sync" });
  const incomingA = await committed(streamA, "incoming", beforeInbound);
  const incomingB = await committed(streamB, "incoming", beforeInbound);
  assert.equal(incomingA.data.messageId, incomingB.data.messageId);
  assert.equal(incomingA.data.origin, "live");
  assert.equal((await b.message("personal", incomingB.data.messageId)).body, "Incoming through real sync");

  const resumeCursor = streamB.events.at(-1).cursor;
  await unsubscribe(streamB);
  await control({ op: "inbound", account: "personal", peer: "sim_mei", body: "Arrived while client B was offline" });
  const missed = await committed(streamA, "incoming", resumeCursor);
  streamB = subscribe(b, "personal", resumeCursor);
  assert.equal((await committed(streamB, "incoming", resumeCursor)).data.messageId, missed.data.messageId);
  await unsubscribe(streamA);
  await unsubscribe(streamB);
  await stopHub();
  await control({ op: "inbound", account: "personal", peer: "sim_mei", body: "Arrived while the hub was stopped", notify: false });
  hub = await start("hub", ["--upstream", remote.ready.upstream]);
  const restarted = await attach();
  assert.equal(restarted.serverId, initialStatus.serverId);
  assert.notEqual(restarted.ownershipGeneration, initialStatus.ownershipGeneration);
  await verifyQuoteMentionRestart(quoteMentionReceipts);
  await waitFor("durable catch-up after hub restart", async () => (await a.messages("personal", "sim_mei")).items.some(item => item.body === "Arrived while the hub was stopped"));
  const caughtUp = (await a.messages("personal", "sim_mei")).items.find(item => item.body === "Arrived while the hub was stopped");
  assert.equal((await a.events("personal", resumeCursor, 200)).items.find(event => event.kind === "message.committed" && event.data.messageId === caughtUp.id).data.origin, "catchup");
  await waitFor("restored push", async () => (await a.account("personal")).incomingTransport === "simulator_push");

  streamA = subscribe(a, "personal");
  streamB = subscribe(b, "personal");
  await control({ op: "send_fault", account: "personal", fault: "reject" });
  const rejected = await a.sendText("personal", "sim_mei", "Rejected synthetic send", "send-rejected");
  await waitFor("rejected send", async () => (await a.send("personal", rejected.id)).state === "failed");
  assert.equal((await a.messages("personal", "sim_mei")).items.filter(item => item.body === "Rejected synthetic send").length, 0);
  await control({ op: "send_fault", account: "personal", fault: "lost_acknowledgment" });
  const uncertain = await a.sendText("personal", "sim_mei", "Accepted with lost acknowledgment", "send-uncertain");
  await waitFor("uncertain send stops for review", async () => (await a.send("personal", uncertain.id)).state === "needs_review");
  assert.equal((await b.sendText("personal", "sim_mei", "Accepted with lost acknowledgment", "send-uncertain")).id, uncertain.id);
  for (const stream of [streamA, streamB]) {
    await waitFor("both clients see uncertainty", () => stream.events.some(event => event.kind === "send.updated" && event.data.id === uncertain.id && event.data.state === "needs_review"));
  }
  assert.equal((await remoteState()).accounts.find(item => item.id === "personal").acceptedSends, 2);

  await login("work");
  await control({ op: "inbound", account: "work", peer: "sim_mei", body: "Work account only" });
  await waitFor("work account message", async () => (await a.messages("work", "sim_mei")).items.some(item => item.body === "Work account only"));
  assert.ok(!(await a.messages("personal", "sim_mei")).items.some(item => item.body === "Work account only"));
  await control({ op: "revoke", account: "work" });
  // Session validation uses the hub's normal heartbeat/fallback scheduling.
  await waitFor("revoked session disconnects", async () => (await a.account("work")).connection === "disconnected", 60000);

  await a.createAccount("expired");
  const expiring = await a.login("expired", { strategy: "qr", qrRefresh: { mode: "manual", maxRefreshes: 0 } });
  await waitFor("expiring QR issued", async () => (await a.operation("expired", expiring.id)).state === "awaiting_scan");
  await control({ op: "advance", seconds: 121 });
  await waitFor("virtual-clock QR expiry", async () => (await a.operation("expired", expiring.id)).state === "expired");

  await control({ op: "advertise_image", account: "personal", peer: "sim_mei" });
  const image = await waitFor("advertised image committed", async () => (await a.messages("personal", "sim_mei")).items.find(item => item.type === 3));
  const media = await a.mediaAssets("personal", image.id);
  const batch = await a.mediaAssetsBatch("personal", [image.id, "invalid-message-id"]);
  assert.equal(batch.items[0].messageId, image.id);
  assertSameUnavailableMedia(batch.items[0].assets, media.items);
  assert.equal(batch.items[1].messageId, "invalid-message-id");
  assert.ok(batch.items[1].error);
  assert.deepEqual(batch.items[1].assets, []);
  const candidate = media.items.find(asset => asset.state !== "unsupported");
  if (candidate) {
    const transfer = await a.transferMedia("personal", { messageId: image.id, assetId: candidate.id, quality: candidate.variant });
    await waitFor("unsupported simulator transfer fails closed", async () => ["failed", "unsupported"].includes((await a.job("personal", transfer.id)).state));
    assert.notEqual((await a.mediaAsset("personal", candidate.id)).state, "available");
  }

  const photo = zstdDecompressSync(await readFile(join(repository, "packages/native-client-rust/crates/wechat-core/tests/fixtures/send-images/50mp-baseline.jpg.zst")));
  const photoSource = await a.upload("personal", photo, "synthetic-50mp.jpg", "image/jpeg");
  const photoRequest = { threadId: "sim_mei", kind: "image", sourceId: photoSource.id,
    standardSourceId: null, thumbnailSourceId: null, playSeconds: null, durationMs: null };
  const photoSend = await a.sendMedia("personal", photoRequest, "large-photo-send");
  const photoDone = await waitFor("50 MP image accepted and projected", async () => {
    const job = await a.job("personal", photoSend.id);
    assert.ok(!["failed", "cancelled", "needs_review"].includes(job.state), job.reason);
    return job.state === "accepted" && job.result?.messageIds?.length && job;
  });
  assert.equal(photoDone.result.imagePreparation.sourceSha256, createHash("sha256").update(photo).digest("hex"));
  assert.equal(photoDone.result.imagePreparation.sourcePreserved, false);
  assert.ok(photoDone.result.imagePreparation.width <= 1280);
  assert.ok(photoDone.result.imagePreparation.height <= 1280);
  for (const id of photoDone.result.messageIds) {
    const assets = await a.mediaAssets("personal", id);
    for (const quality of ["original", "standard", "thumbnail"]) {
      const asset = assets.items.find(asset => asset.variant === quality);
      assert.equal(asset?.state, "available");
      const content = await a.mediaContent("personal", asset.id);
      assert.equal(content.contentType, "image/jpeg");
      assert.ok(content.bytes.length <= 65520);
      assert.equal(createHash("sha256").update(content.bytes).digest("hex"), asset.sha256);
    }
  }
  assert.equal((await a.sendMedia("personal", photoRequest, "large-photo-send")).id, photoDone.id);
  assert.equal((await a.sendByKey("personal", "large-photo-send")).id, photoDone.id);

  const moviePath = join(repository, "packages/native-client-rust/crates/wechat-core/tests/fixtures/send-videos/legacy-mjpeg.mov");
  const movie = await readFile(moviePath);
  assert.notEqual(movie.subarray(4, 8).toString(), "ftyp");
  const movieHash = createHash("sha256").update(movie).digest("hex");
  const movieSource = await a.upload("personal", movie, "legacy.mov", "video/quicktime");
  const movieRequest = { threadId: "sim_mei", kind: "video", sourceId: movieSource.id,
    standardSourceId: null, thumbnailSourceId: null, playSeconds: null, durationMs: null,
    videoQuality: "normal" };
  for (const quality of ["normal", "original"]) {
    const key = `legacy-mov-${quality}`;
    const submitted = quality === "original"
      ? await cli("messages", "send", "sim_mei", "--video", moviePath, "--video-quality", quality, "--idempotency-key", key, "--detach")
      : await a.sendMedia("personal", movieRequest, key);
    const done = await waitFor(`${quality} MOV accepted and projected`, async () => {
      const current = await a.job("personal", submitted.id);
      assert.ok(!["failed", "cancelled", "needs_review"].includes(current.state), current.reason);
      return current.state === "accepted" && current.result?.messageIds?.length && current;
    });
    assert.equal(done.result.uploadReadbackVerified, true);
    assert.equal(done.result.profileProjection, "complete");
    assert.equal(done.result.officialMediaWriteback, "complete");
    assert.equal(done.result.videoPreparation.quality, quality);
    assert.equal(done.result.videoPreparation.sourceSha256, movieHash);
    assert.equal(done.result.videoPreparation.originalIncluded, quality === "original");
    assert.notEqual(done.result.videoPreparation.normalSha256, movieHash);
    for (const id of done.result.messageIds) {
      const assets = (await a.mediaAssets("personal", id)).items;
      assert.equal(assets.some(asset => asset.variant === "original"), quality === "original");
      for (const variant of quality === "original" ? ["normal", "thumbnail", "original"] : ["normal", "thumbnail"]) {
        const asset = assets.find(asset => asset.variant === variant);
        assert.equal(asset?.state, "available");
        const content = await a.mediaContent("personal", asset.id);
        assert.equal(createHash("sha256").update(content.bytes).digest("hex"), asset.sha256);
        assert.equal(content.contentType, variant === "original" ? "video/quicktime" : variant === "normal" ? "video/mp4" : "image/jpeg");
        if (variant === "original") {
          assert.equal(asset.filename, "original.mov");
          assert.deepEqual(content.bytes, new Uint8Array(movie));
        }
        if (variant === "normal") assert.equal(asset.sha256, done.result.videoPreparation.normalSha256);
      }
      if (quality === "original") {
        const savedMovie = join(temporary, "downloaded-original.mov");
        await cli("messages", "media", "sim_mei", id, "--quality", "original", "--output", savedMovie);
        assert.deepEqual(await readFile(savedMovie), movie);
      }
    }
    assert.equal((await a.sendByKey("personal", key)).id, done.id);
    if (quality === "original")
      assert.equal((await cli("messages", "send", "sim_mei", "--video", moviePath, "--video-quality", quality, "--idempotency-key", key, "--detach")).id, done.id);
    else
      assert.equal((await a.sendMedia("personal", movieRequest, key)).id, done.id);
  }

  const controlCursor = streamA.events.at(-1).cursor;
  await control({ op: "status_notification", account: "personal", operation_id: 2 });
  await control({ op: "status_notification", account: "personal", operation_id: 11 });
  await waitFor("unhandled status controls remain staged", async () => (await a.account("personal")).pendingProjectionBatches > 0);
  await waitFor("status control diagnostics", async () => {
    const events = (await a.events("personal", controlCursor, 200)).items;
    return [2, 11].every(operation => events.some(event => event.kind === "projection.blocked" && event.data.commands?.some(command => command.classification === "status_notification" && command.operationIds.includes(operation))));
  });
  await control({ op: "inbound", account: "personal", peer: "sim_mei", body: "Live message after blocked status controls" });
  const laterMessage = await waitFor("blocked controls do not delay live messages", async () => (await a.messages("personal", "sim_mei")).items.find(item => item.body === "Live message after blocked status controls"));
  assert.equal((await a.events("personal", controlCursor, 200)).items.find(event => event.kind === "message.committed" && event.data.messageId === laterMessage.id).data.origin, "live");
  assert.ok((await a.account("personal")).pendingProjectionBatches > 0);
  await control({ op: "unknown_sync", account: "personal" });
  await waitFor("unknown command remains staged", async () => (await a.account("personal")).pendingProjectionBatches > 0);
  await waitFor("unknown command blocks official cursor publication", async () => (await a.events("personal", controlCursor, 200)).items.some(event => event.kind === "projection.blocked"));
  await unsubscribe(streamA);
  await unsubscribe(streamB);
  const logout = await a.logout("personal", "logout-personal");
  await waitFor("logout committed", async () => (await a.operation("personal", logout.id)).state === "succeeded");
  assert.equal((await a.account("personal")).authenticated, false);
  await assert.rejects(a.sendText("personal", "sim_mei", "Must not submit after logout", "after-logout"));
  await stopHub();

  const launcher = await start("dev", [], null);
  assert.equal((await fetch(`${launcher.ready.endpoint}/health`)).status, 200);
  const launcherRemote = `http://${launcher.ready.upstream}/health`;
  assert.equal((await fetch(launcherRemote)).status, 200);
  if (process.platform !== "win32") {
    launcher.child.kill("SIGINT");
  } else {
    // Terminating an owned child exercises launcher cleanup without relying on
    // Node's Windows signal emulation to deliver a console Ctrl+C event.
    const token = (await readFile(launcher.ready.controlTokenFile, "utf8")).trim();
    await fetch(`http://${launcher.ready.upstream}/shutdown`, { method: "POST", headers: { Authorization: `Bearer ${token}` } });
  }
  await waitFor("launcher stops owned children", () => launcher.child.exitCode !== null || launcher.child.signalCode !== null);
  if (process.platform !== "win32") assert.equal(launcher.child.exitCode, 0);
  await assert.rejects(stat(launcher.ready.dataDir), error => error.code === "ENOENT");
  await assert.rejects(fetch(launcherRemote));
  console.log(JSON.stringify({ ok: true, checks: ["real QR login and profile projection", "two SDK event consumers", "actual wx CLI status and message fetch", "direct/group CLI and API pin/unpin with independent remote refresh", "group owner capability and denied all-members send without an intent", "pin/mute independence and keyed replay", "queued/accepted/uncertain sends", "quoted SDK and CLI member/Everyone mentions, inline positions, lost ACKs and restart recovery", "local_send without default echo", "fault-injected echo deduplication", "synthetic phone self-send and File Transfer", "50 MP image preparation, cached copies and idempotent replay", "legacy MOV normal/original CLI/API sends, exact original download and idempotent replay", "incoming/live and catchup origins", "inbound push", "SSE resume", "restart and catch-up", "account isolation", "revocation/logout", "virtual clock expiry", "advertised media stays unavailable", "unhandled status controls do not block live messages", "unknown sync retention", "launcher startup/shutdown and cleanup"] }));
} finally {
  for (const subscription of subscriptions) subscription.controller.abort();
  await Promise.allSettled(subscriptions.map(subscription => subscription.done));
  if (remote && controlToken) {
    await fetch(`http://${remote.ready.upstream}/shutdown`, { method: "POST", headers: { Authorization: `Bearer ${controlToken}` }, signal: AbortSignal.timeout(1000) }).catch(() => {});
  }
  for (const child of processes) child.kill();
  await Promise.all([...processes].map(child => new Promise(resolve => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve();
    child.once("exit", resolve);
    setTimeout(() => { child.kill("SIGKILL"); resolve(); }, 3000).unref();
  })));
  await rm(temporary, { recursive: true, force: true });
}
