// Synthetic Mini Program hub, CLI and browser acceptance.
// Build wx and wx-dev with --profile test and build packages/shared first.
// Uses no WeChat account or third-party package. Requests stay on loopback.
// Set CARGO_TARGET_DIR or WX_BINARY / WX_DEV_BINARY for existing binaries.
// MINIPROGRAM_SCREENSHOTS retains browser screenshots when requested.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import { promisify } from "node:util";
import { stageGlassMiniProgram } from "./fixtures/native-miniprogram.mjs";
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(join(repository, "packages/client-ui/package.json"));
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(process.env.PLAYWRIGHT_MODULE) : require("@playwright/test");

const worktree = repository;
const { startStack } = await import(join(worktree, "packages/client-ui/scenes/stack.mjs"));
const { seeder, ACCOUNT, SPARE } = await import(join(worktree, "packages/client-ui/scenes/seed.mjs"));
const { startUiServer } = await import(join(worktree, "packages/client-ui/scenes/server.mjs"));
const stack = await startStack();
let browser, policyHub, ui, desktop;
const hosts = [];
const bin = process.env.WX_BINARY ?? join(process.env.CARGO_TARGET_DIR ?? join(repository, "packages/native-client-rust/target"), "debug", process.platform === "win32" ? "wx.exe" : "wx");
const output = process.env.MINIPROGRAM_SCREENSHOTS ? resolve(process.env.MINIPROGRAM_SCREENSHOTS) : join(stack.dataDir, "screenshots");
await mkdir(output, { recursive: true });
const app = "wx0123456789abcdef";
const childReply = async (args, config) => {
  const child = spawn(bin, args, { stdio: [config ? "pipe" : "ignore", "pipe", "pipe"] });
  const exited = once(child, "exit");
  hosts.push({ child, exited });
  let detail = "";
  child.stderr.on("data", data => { detail = (detail + data).slice(-300); });
  const line = once(createInterface({ input: child.stdout }), "line");
  if (config) child.stdin.write(JSON.stringify(config) + "\n");
  const [value] = await Promise.race([
    line, exited.then(([code]) => { throw new Error("Host exited " + code + " " + detail); }),
    new Promise((_, reject) => setTimeout(() => reject(new Error("Host readiness timed out")), 15000).unref()),
  ]);
  const reply = JSON.parse(value);
  assert.equal(reply.version, 1);
  return { child, exited, reply };
};
const childReady = async (args, config) => {
  const { child, exited, reply } = await childReply(args, config);
  assert.equal(reply.state, "ready");
  return { child, exited, url: reply.url };
};
try {
  const sim = seeder(stack);
  async function signIn(account) {
    const operation = await sim.client.login(account.id, { strategy: "auto", timeoutSeconds: 900 }, "mini-preview-" + account.id);
    for (let tries = 0; tries < 300; tries++) {
      const op = await sim.client.operation(account.id, operation.id);
      if (op.result?.qrAvailable) break;
      if (op.state === "failed") throw new Error("Synthetic login failed " + op.result?.code);
      await new Promise(resolve => setTimeout(resolve, 200));
    }
    await stack.control({ op: "login", account: account.id, status: "scanned" });
    await sim.advance(2);
    await stack.control({ op: "login", account: account.id, status: "approved" });
    let last;
    for (let tries = 0; tries < 300; tries++) {
      const op = await sim.client.operation(account.id, operation.id);
      last = [op.state, op.result?.code];
      if (op.state === "succeeded") return;
      if (op.state === "failed") break;
      await sim.advance(1);
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    throw new Error("Synthetic login ended " + JSON.stringify(last));
  }
  await sim.addAccount(ACCOUNT);
  await signIn(ACCOUNT);
  await sim.miniPrograms("sim_mei", app);
  await sim.addAccount(SPARE);
  await signIn(SPARE);
  const cards = await sim.messages("sim_mei");
  const card = cards.find(message => message.miniProgram?.appId === app);
  assert.equal(card?.miniProgram?.pagePath, "pages/menu/index?menu=seasonal");
  const listing = await sim.client.listMiniPrograms(ACCOUNT.id);
  assert.equal(listing.items.find(x => x.appId === app).available, true);
  assert.equal(listing.items.find(x => x.appId === app).title, "Cafe");
  assert.equal(listing.items.find(x => x.appId === app).iconMessageId, card.id);
  const iconAssets = await sim.client.mediaAssets(ACCOUNT.id, card.id);
  const icon = iconAssets.items.find(asset => asset.filename === "mini-program-icon");
  assert.ok(icon);
  assert.equal(listing.items.find(x => x.title === "Trail guide").available, false);
  assert.deepEqual((await sim.client.listMiniPrograms(SPARE.id)).items, []);
  const launch = await sim.client.prepareMiniProgram(ACCOUNT.id, app, { sourceMessageId: card.id });
  assert.equal(launch.entry.path, "pages/menu/index");
  assert.equal(launch.entry.query, "menu=seasonal");
  assert.equal(launch.entry.framework, "exparser");
  assert.equal(launch.capabilities.login, false);
  assert.deepEqual(launch.packages.map(({ id, root, version }) => [id, root ?? null, version]),
    [["main", null, "1"], ["utilities", "utilities", "1"]]);
  await assert.rejects(() => sim.client.prepareMiniProgram(ACCOUNT.id, app, { sourceMessageId: "bad" }));
  const apiKeyPath = join(stack.dataDir, "cli-test-key");
  const configPath = join(stack.dataDir, "cli-test.toml");
  await writeFile(apiKeyPath, stack.hub.token + "\n", { mode: 0o600 });
  await writeFile(configPath, "schemaVersion = 1\n", { mode: 0o600 });
  const command = ["--json", "--server", stack.hub.endpoint, "--account", ACCOUNT.id, "--token-file", apiKeyPath, "--config", configPath, "miniprogram"];
  const { stdout } = await promisify(execFile)(bin, [...command, "fetch", app]);
  assert.deepEqual(JSON.parse(stdout), launch.packages[0]);
  async function serveArgs(scopes) {
    const name = scopes.join("-");
    const { token } = await sim.client.request("v1/tokens", {
      method: "POST",
      body: JSON.stringify({ name, scopes, accounts: [ACCOUNT.id] }),
    });
    const tokenPath = join(stack.dataDir, `cli-${name}-key`);
    await writeFile(tokenPath, token + "\n", { mode: 0o600 });
    const scoped = [...command, "serve"];
    scoped[scoped.indexOf("--token-file") + 1] = tokenPath;
    return scoped;
  }
  const args = await serveArgs(["mini-programs"]);
  const cardArgs = await serveArgs(["read", "mini-programs"]);
  const host = await childReady([...cardArgs, app, "--message", card.id]);
  browser = await chromium.launch({ headless: true,
    executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
    args: ["--disable-background-networking", "--disable-dev-shm-usage"],
  });
  const glassApp = await stageGlassMiniProgram(stack.dataDir, ACCOUNT.id);
  const glassHost = await childReady([...args, glassApp]);
  const glassPage = await browser.newPage({ viewport: { width: 410, height: 776 } });
  const glassErrors = [];
  glassPage.on("pageerror", error => glassErrors.push(error.message));
  glassPage.on("console", message => { if (message.type() === "error") glassErrors.push(message.text()); });
  await glassPage.goto(glassHost.url);
  await glassPage.getByText("Count 0", { exact: true }).waitFor();
  const glassStyle = await glassPage.getByText("Count 0", { exact: true }).evaluate(node => ({
    color: getComputedStyle(node).color,
    padding: getComputedStyle(node.closest("mini-page")).paddingLeft,
    background: getComputedStyle(node.parentElement).backgroundColor,
  }));
  assert.equal(glassStyle.color, "rgb(18, 80, 62)");
  assert.equal(glassStyle.background, "rgb(230, 240, 250)");
  assert.ok(Math.abs(parseFloat(glassStyle.padding) - 16.4) < 0.1);
  await glassPage.locator(".mini-toolbar").waitFor({ state: "visible" });
  await glassPage.getByRole("button", { name: "Increment", exact: true }).click();
  await glassPage.getByText("Count 1", { exact: true }).waitFor();
  const glassSheets = await glassPage.evaluate(() => document.adoptedStyleSheets.length);
  await glassPage.getByText("Next page", { exact: true }).click();
  await glassPage.getByText("Second page", { exact: true }).waitFor();
  assert.equal(await glassPage.locator(".second").evaluate(node => getComputedStyle(node).backgroundColor), "rgb(250, 240, 230)");
  await glassPage.getByRole("button", { name: "Return", exact: true }).click();
  await glassPage.getByText("Count 1", { exact: true }).waitFor();
  assert.equal(await glassPage.evaluate(() => document.adoptedStyleSheets.length), glassSheets);
  await glassPage.getByRole("button", { name: "Increment", exact: true }).click();
  await glassPage.getByText("Count 2", { exact: true }).waitFor();
  assert.deepEqual(glassErrors, []);
  const titleResult = await glassPage.evaluate(() => wx.setNavigationBarTitle({ title: "Fixture title" }));
  assert.equal(titleResult.errMsg, "setNavigationBarTitle:ok");
  assert.equal(await glassPage.title(), "Fixture title");
  await glassPage.screenshot({ path: join(output, "cli-glass-easel.png") });
  await glassPage.close();
  console.log("PASS: Glass-easel compiled CSS, rpx, scoped toolbar, tap, native navigator, back and stylesheet cleanup");
  const page = await browser.newPage({ viewport: { width: 410, height: 776 } });
  const pageErrors = [];
  page.on("pageerror", error => pageErrors.push(error.message));
  page.on("console", message => { if (message.type() === "error") pageErrors.push(message.text()); });
  await page.goto(host.url);
  await page.getByText("Menu: seasonal", { exact: true }).waitFor({ timeout: 8000 }).catch(async error => {
    console.error(JSON.stringify({ body: await page.locator("body").innerText(), errors: pageErrors }));
    throw error;
  });
  await page.getByText("Menu: seasonal: 0", { exact: true }).waitFor();
  const styles = await page.getByPlaceholder("Add a note").evaluate(input => ({
    border: getComputedStyle(input).borderTopColor,
    padding: getComputedStyle(input.getRootNode().querySelector(".mini-view-root")).paddingLeft,
  }));
  assert.equal(styles.border, "rgb(18, 80, 62)");
  assert.ok(Math.abs(parseFloat(styles.padding) - 16.4) < 0.1);
  assert.equal(await page.getByText("Menu: seasonal: 0", { exact: true }).evaluate(n => getComputedStyle(n).color), "rgb(160, 40, 40)");
  await page.locator(".mini-toolbar").waitFor({ state: "visible" });
  console.log("PASS: compiled WXSS imports, responsive units, native selectors, isolated component styles and toolbar");
  assert.equal(await page.locator("#next-page").evaluate(node => getComputedStyle(node).cursor), "pointer");
  const failedPage = await browser.newPage();
  const startupError = failedPage.waitForEvent("console", { predicate: message => message.text().includes("Mini Program startup failed") });
  await failedPage.route("**/modules.js", route => route.abort());
  await failedPage.goto(host.url);
  await failedPage.getByText("This Mini Program page could not be opened.").waitFor();
  await startupError;
  await failedPage.close();
  console.log("PASS: tap cursor and visible module startup failure");

  const svg = await fetch(new URL("/icons/menu@2x.SVG", host.url));
  assert.equal(svg.headers.get("content-type"), "image/svg+xml");
  assert.match(svg.headers.get("content-security-policy"), /^sandbox; default-src 'none'/);
  await svg.body.cancel();
  const png = await fetch(new URL("/icons/menu.PNG", host.url));
  assert.equal(png.headers.get("content-type"), "image/png");
  await png.body.cancel();
  const embedded = page.locator("#embedded-image");
  await embedded.evaluate(image => image.decode());
  const imageResult = await embedded.evaluate(image => {
    const canvas = document.createElement("canvas");
    canvas.width = 2; canvas.height = 1;
    const context = canvas.getContext("2d");
    context.drawImage(image, 0, 0);
    return {
      pixel: [...context.getImageData(0, 0, 1, 1).data],
      width: image.getBoundingClientRect().width,
      backgroundWidth: image.getRootNode().querySelector("#embedded-background").getBoundingClientRect().width,
    };
  });
  assert.deepEqual(imageResult.pixel, [18, 80, 62, 255]);
  assert.ok(Math.abs(imageResult.width - 43.733) < 0.1);
  assert.ok(Math.abs(imageResult.backgroundWidth - 10.933) < 0.1);
  const declaredStyle = await page.evaluate(async () => {
    const { inlineStyle } = await import("/styles.js");
    return inlineStyle('width:30rpx;content:"30rpx";background-image:url("icons/30rpx.png")');
  });
  assert.match(declaredStyle, /calc\(30\s*\*\s*100vw\s*\/\s*750\)/);
  assert.match(declaredStyle, /content:"30rpx"/);
  assert.match(declaredStyle, /icons\/30rpx\.png/);
  const externalRequests = [];
  const trackExternal = request => {
    if (new URL(request.url()).origin !== new URL(host.url).origin) externalRequests.push(request.url());
  };
  page.on("request", trackExternal);
  const originalImage = await page.evaluate(() => getCurrentPages().at(-1).data.image);
  await page.evaluate(() => getCurrentPages().at(-1).setData({ image: "../../icons/menu@2x.SVG" }));
  assert.equal(await embedded.evaluate(async image => {
    await image.decode();
    return image.naturalWidth;
  }), 2);
  for (const image of ["//example.invalid/image.png", "data:text/html,<script>1</script>", "javascript:void(0)"]) {
    await page.evaluate(image => getCurrentPages().at(-1).setData({ image }), image);
    assert.equal(await embedded.getAttribute("src"), null);
  }
  page.off("request", trackExternal);
  assert.deepEqual(externalRequests, []);
  await page.evaluate(() => getCurrentPages().at(-1).setData({
    image: "data:image/svg+xml," + encodeURIComponent(
      '<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><script>parent.__embeddedSvgExecuted=true</script><path d="M0 0h2v1H0z"/></svg>',
    ),
  }));
  await embedded.evaluate(image => image.decode());
  assert.equal(await page.evaluate(() => globalThis.__embeddedSvgExecuted), undefined);
  await page.evaluate(image => getCurrentPages().at(-1).setData({ image }), originalImage);
  await embedded.evaluate(image => image.decode());
  console.log("PASS: embedded image pixels, inline/raw rpx sizing and rejected unsafe image schemes");
  const visibility = await page.evaluate(() => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Visibility observation timed out")), 5000);
    const observer = getCurrentPages().at(-1).createIntersectionObserver({ thresholds: ["0", "0.5", "1"], initialRatio: -1 });
    observer.relativeToViewport().observe("input", result => {
      observer.disconnect();
      clearTimeout(timer);
      resolve({ ratio: result.intersectionRatio, width: result.boundingClientRect.width });
    });
  }));
  assert.equal(visibility.ratio, 1);
  assert.ok(visibility.width > 0);
  console.log("PASS: component visibility observer reports actual browser geometry");


  await page.getByRole("button", { name: "Add one" }).click();
  await page.getByText("Menu: seasonal: 1", { exact: true }).waitFor();
  await page.getByText("Count reported: 1", { exact: true }).waitFor();
  assert.deepEqual(await page.evaluate(() => {
    const child = getCurrentPages().at(-1).selectComponent("#counter");
    return { id: child.id, dataset: child.dataset };
  }), { id: "counter", dataset: { ref: "Menu: seasonal", options: { enabled: true } } });
  const common = await page.evaluate(async () => {
    const current = getCurrentPages().at(-1);
    const selected = current.selectComponent("#counter");
    const [rect, missing] = await new Promise(resolve => {
      const query = current.createSelectorQuery();
      query.select("#counter").boundingClientRect();
      query.select("#missing").boundingClientRect();
      query.exec(resolve);
    });
    const original = { items: [1] };
    await wx.setStorage({ key: "object", data: original });
    original.items.push(2);
    const stored = await wx.getStorage({ key: "object" });
    wx.removeStorageSync("object");
    const absent = await wx.getStorage({ key: "object" }).then(() => null, error => error.errMsg);
    const blocked = await new Promise(resolve => {
      const task = wx.request({ url: "https://example.invalid", complete: resolve });
      task.abort();
    });
    const denied = await new Promise(resolve => wx.request({ url: "https://example.invalid", complete: resolve }));
    const settings = await wx.getSetting();
    const shownRoute = wx.getStorageSync("shownRoute");
    const listener = () => wx.setStorageSync("removed", true);
    wx.onAppHide(listener);
    wx.offAppHide(listener);
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    await Promise.resolve();
    const hidden = wx.getStorageSync("hidden");
    delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
    await Promise.resolve();
    await wx.showLoading({ title: "Loading the fixture" });
    return { selected: selected?.data.count, rect, missing, stored, absent, blocked, denied, settings,
      shownRoute, hidden, removed: wx.getStorageSync("removed"), window: wx.getWindowInfo() };
  });
  assert.equal(common.selected, 1);
  assert.equal(common.rect.id, "counter");
  assert.ok(common.rect.width > 0 && common.rect.height > 0);
  assert.equal(common.missing, null);
  assert.deepEqual(common.stored.data, { items: [1] });
  assert.equal(common.absent, "getStorage:fail");
  assert.equal(common.blocked.errMsg, "request:fail abort");
  assert.equal(common.denied.errMsg, "request:fail network access is disabled");
  assert.deepEqual(common.settings.authSetting, {});
  assert.equal(common.shownRoute, "pages/menu/index");
  assert.equal(common.hidden, true);
  assert.equal(common.removed, "");
  assert.equal(common.window.windowWidth, 410);
  assert.equal(common.window.windowHeight, 732);
  await page.getByRole("status").getByText("Loading the fixture").waitFor();
  await page.evaluate(() => wx.hideLoading());
  await page.getByRole("status").waitFor({ state: "hidden" });
  await page.evaluate(() => wx.showToast({ title: "Saved", icon: "none" }));
  await page.getByRole("status").getByText("Saved").waitFor();
  await page.evaluate(() => wx.hideToast());
  await page.getByRole("status").waitFor({ state: "hidden" });
  console.log("PASS: compiled component, behavior, custom event, slot, selector, app visibility, isolated storage, feedback, and denied request");
  const online = await page.evaluate(async () => {
    const initial = await wx.getNetworkType();
    globalThis.networkEvents = [];
    globalThis.primaryNetworkListener = status => networkEvents.push(["primary", status]);
    wx.onNetworkStatusChange(primaryNetworkListener);
    wx.onNetworkStatusChange(status => networkEvents.push(["secondary", status]));
    return initial;
  });
  assert.equal(online.errMsg, "getNetworkType:ok");
  assert.ok(["wifi", "unknown"].includes(online.networkType));
  await page.context().setOffline(true);
  await page.waitForFunction(() => networkEvents.length === 2);
  assert.deepEqual(await page.evaluate(() => networkEvents), [
    ["primary", { isConnected: false, networkType: "none" }],
    ["secondary", { isConnected: false, networkType: "none" }],
  ]);
  const offline = await page.evaluate(() => new Promise(resolve => wx.getNetworkType({ success: resolve })));
  assert.deepEqual(offline, { networkType: "none", errMsg: "getNetworkType:ok" });
  await page.evaluate(() => {
    window.dispatchEvent(new Event("offline"));
    wx.offNetworkStatusChange(primaryNetworkListener);
  });
  await page.context().setOffline(false);
  await page.waitForFunction(() => networkEvents.length === 3);
  assert.deepEqual(await page.evaluate(() => networkEvents[2]), [
    "secondary", { isConnected: true, networkType: online.networkType },
  ]);
  await page.evaluate(() => wx.offNetworkStatusChange());
  await page.context().setOffline(true);
  await page.waitForFunction(() => !navigator.onLine);
  assert.equal(await page.evaluate(() => networkEvents.length), 3);
  await page.context().setOffline(false);
  await page.waitForFunction(() => navigator.onLine);
  console.log("PASS: browser wx network snapshots, online/offline events, deduplication and listener removal");
  const changingButton = page.getByRole("button", { name: "Change title" });
  await changingButton.evaluate(node => { globalThis.pressedButton = node; });
  await changingButton.hover();
  await page.mouse.down();
  await page.evaluate(() => new Promise(resolve => getCurrentPages().at(-1).setData({ unrelatedRefresh: 1 }, resolve)));
  assert.equal(await changingButton.evaluate(node => node === globalThis.pressedButton && node.isConnected), true);
  await page.mouse.up();
  await page.getByText("Menu updated", { exact: true }).waitFor();
  await page.getByText("Menu updated: 1", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => getCurrentPages().at(-1).selectComponent("#counter").dataset.ref), "Menu updated");
  await page.getByPlaceholder("Add a note").pressSequentially("Lunch");
  await page.getByText("Lunch", { exact: true }).waitFor();
  await page.screenshot({ path: join(output, "cli-exparser.png") });
  await page.locator("#next-page").click();
  await page.getByText("Menu: next", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await page.getByText("Menu updated", { exact: true }).waitFor();
  await page.getByRole("link", { name: "Open linked page" }).focus();
  await page.keyboard.press("Enter");
  await page.getByText("Menu: linked", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await page.getByText("Menu updated", { exact: true }).waitFor();
  assert.deepEqual(pageErrors, []);
  host.child.kill("SIGINT");
  assert.equal((await host.exited)[0], 0);
  await assert.rejects(() => fetch(host.url));
  console.log("PASS: CLI Exparser shared route, edits, navigation and Ctrl-C");
  const home = await childReady([...args, app, "--allow-request-url", "https://example.invalid/session"]);
  await page.goto(home.url);
  await page.getByText("Cafe home", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => wx.canIUse("request")), false);
  let subpackageRequests = 0;
  page.on("request", request => {
    if (new URL(request.url()).pathname === "/_wx/subpackages/utilities.js") subpackageRequests++;
  });
  assert.deepEqual(await page.evaluate(async () => {
    const [first, second] = await getCurrentPages().at(-1).loadUtilities();
    return [first.value, first === second];
  }), [42, true]);
  assert.equal(subpackageRequests, 1);
  assert.equal(await page.evaluate(() => console.error === window.print), false);
  assert.equal(await page.evaluate(() => typeof window.print), "function");
  assert.deepEqual(await page.evaluate(async () => {
    const { instance } = await WXWebAssembly.instantiate("/utilities/value.wasm");
    const rejected = await WXWebAssembly.instantiate("https://example.invalid/value.wasm")
      .then(() => false, () => true);
    return [instance.exports.value(), rejected];
  }), [42, true]);
  await page.evaluate(async () => {
    await wx.navigateTo({ url: "/pages/menu/index" });
    await wx.reLaunch({ url: "/pages/home/index?privacyTest=1" });
  });
  assert.equal(await page.evaluate(() => getCurrentPages().length), 1);
  assert.equal(await page.evaluate(() => wx.getStorageSync("loadingRoute")), "pages/home/index");
  await page.getByText("Cafe home", { exact: true }).waitFor();
  console.log("PASS: same-version lazy subpackage and full page-stack reset");
  const accept = page.getByRole("button", { name: "Accept privacy terms" });
  const privacy = page.locator("#mini-privacy");
  await accept.evaluate(button => button.click());
  assert.equal(await privacy.isVisible(), false);
  await accept.click();
  await privacy.getByRole("button", { name: "Cancel" }).click();
  assert.equal(await page.evaluate(async () => (await wx.getPrivacySetting()).needAuthorization), true);
  await accept.click();
  await privacy.getByRole("button", { name: "Continue" }).click();
  await page.waitForFunction(() => wx.getStorageSync("privacyEvent") === "agreePrivacyAuthorization:ok");
  assert.equal(await page.evaluate(async () => (await wx.getPrivacySetting()).needAuthorization), false);
  console.log("PASS: local privacy confirmation requires a browser user action");
  const listenerLog = page.waitForEvent("console", {
    predicate: message => message.text().includes("fixture callback failure"),
  });
  assert.equal(await page.evaluate(async () => {
    const original = console.error;
    let calls = 0;
    const listener = async () => { calls++; throw new Error("fixture callback failure"); };
    console.error = () => { throw new Error("fixture console failure"); };
    wx.onError(listener);
    try {
      window.dispatchEvent(new ErrorEvent("error", { message: "fixture event" }));
      await new Promise(resolve => setTimeout(resolve, 25));
      return calls;
    } finally {
      wx.offError(listener);
      console.error = original;
    }
  }), 1);
  await listenerLog;
  assert.match(pageErrors.pop(), /fixture callback failure/);
  console.log("PASS: app error listener failures do not recurse through app-wrapped logging");
  home.child.kill("SIGINT");
  assert.equal((await home.exited)[0], 0);
  await assert.rejects(() => fetch(home.url));

  // The simulator cannot retrieve WeChat's app attributes. Supply a synthetic
  // policy in the launch response over loopback to exercise the real CLI gate.
  const launchPath = `/v1/accounts/${ACCOUNT.id}/mini-programs/${app}/launches`;
  let failPolicy = false;
  policyHub = createServer(async (request, response) => {
    try {
      if (failPolicy && request.url === launchPath && request.method === "POST") {
        response.writeHead(502, { "content-type": "application/json" });
        response.end(JSON.stringify({ error: { code: "mini_program_network_unavailable" } }));
        return;
      }
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const upstream = await fetch(stack.hub.endpoint + request.url, {
        method: request.method,
        headers: {
          authorization: request.headers.authorization,
          "content-type": "application/json",
        },
        body: request.method === "POST" ? Buffer.concat(chunks) : undefined,
      });
      response.statusCode = upstream.status;
      response.setHeader("content-type", upstream.headers.get("content-type") ?? "application/octet-stream");
      if (request.url === launchPath && request.method === "POST" && upstream.ok) {
        const launch = await upstream.json();
        launch.networkPolicy = { requestDomains: ["https://example.invalid"] };
        response.end(JSON.stringify(launch));
      } else {
        response.end(Buffer.from(await upstream.arrayBuffer()));
      }
    } catch {
      response.statusCode = 502;
      response.end();
    }
  });
  policyHub.listen(0, "127.0.0.1");
  await once(policyHub, "listening");
  const policyArgs = [...args];
  policyArgs[policyArgs.indexOf("--server") + 1] = `http://127.0.0.1:${policyHub.address().port}`;
  failPolicy = true;
  const policyError = await childReply(["__mini-program-host-v1"], {
    version: 1, endpoint: policyArgs[policyArgs.indexOf("--server") + 1], token: stack.hub.token,
    accountId: ACCOUNT.id, appId: app,
  });
  assert.equal(policyError.reply.code, "mini_program_network_unavailable");
  assert.equal((await policyError.exited)[0], 1);
  failPolicy = false;
  const deniedHost = await childReady([...policyArgs, app, "--allow-request-url", "https://example.invalid/session"]);
  const localUrl = new URL(deniedHost.url);
  const requestUrl = new URL("/_wx/request", localUrl.origin);
  const launchKey = localUrl.hash.slice(1);
  assert.ok(launchKey);
  const requestBody = { url: "https://example.invalid/other", method: "GET", header: {} };
  const postRequest = (origin, key, body) => fetch(requestUrl, {
    method: "POST", headers: { Origin: origin, "Content-Type": "application/json", "x-wx-launch": key },
    body: JSON.stringify(body),
  });
  assert.equal((await postRequest("https://foreign.invalid", launchKey, requestBody)).status, 403);
  assert.equal((await postRequest(localUrl.origin, "wrong", requestBody)).status, 403);
  assert.equal((await postRequest(localUrl.origin, launchKey, requestBody)).status, 403);
  assert.equal((await postRequest(localUrl.origin, launchKey, { ...requestBody, url: "https://127.0.0.1/session" })).status, 400);
  const imageUrl = new URL("/_wx/image", localUrl.origin);
  const postImage = body => fetch(imageUrl, {
    method: "POST", headers: { Origin: localUrl.origin, "Content-Type": "application/json", "x-wx-launch": launchKey },
    body: JSON.stringify(body),
  });
  assert.equal((await postImage({ url: "https://127.0.0.1/private" })).status, 400);
  assert.equal((await postImage({ url: "https://example.invalid/session", header: { Authorization: "no" } })).status, 422);
  await page.goto(deniedHost.url);
  await page.getByText("Cafe home", { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => wx.canIUse("request")), true);
  const denied = await page.evaluate(() => new Promise(resolve => wx.request({
    url: "https://example.invalid/other", complete: resolve,
  })));
  assert.equal(denied.errMsg, "request:fail URL is not allowed");
  await page.route("**/_wx/request", async route => {
    const input = route.request().postDataJSON();
    assert.equal(input.url, "https://example.invalid/session");
    assert.equal(input.method, "POST");
    assert.deepEqual(JSON.parse(input.body), { example: "offline fixture" });
    assert.equal(input.header["content-type"], "application/json");
    await route.fulfill({ json: { statusCode: 422, header: { "x-fixture": "yes" },
      cookies: ["fixture=example"], body: Buffer.from('{"status":"fixture"}').toString("base64") } });
  });
  const callbacks = await page.evaluate(() => new Promise(resolve => {
    const events = [];
    const task = wx.request({
      url: "https://example.invalid/session", method: "POST", data: { example: "offline fixture" },
      success: reply => events.push(["success", reply]),
      complete: reply => resolve([...events, ["complete", reply.statusCode]]),
    });
    task.onHeadersReceived(reply => events.push(["header", reply.header["x-fixture"]]));
  }));
  assert.deepEqual(callbacks, [
    ["header", "yes"], ["success", { data: { status: "fixture" }, statusCode: 422,
      header: { "x-fixture": "yes" }, cookies: ["fixture=example"], errMsg: "request:ok" }],
    ["complete", 422],
  ]);
  await page.unroute("**/_wx/request");
  const gatewayFallback = await page.evaluate(() => new Promise(resolve => {
    const events = [];
    wx.cloud.services.Gateway().call({
      success: () => events.push("unexpected gateway success"),
      fail: error => {
        events.push(error.errMsg);
        wx.request({ url: "https://example.invalid/other", complete: result => resolve([...events, result.errMsg]) });
      },
      complete: () => events.push("gateway complete"),
    });
  }));
  assert.deepEqual(gatewayFallback, ["gateway.call:fail", "gateway complete", "request:fail URL is not allowed"]);
  let imageRequests = 0;
  await page.route("**/_wx/image", async route => {
    const input = route.request().postDataJSON();
    assert.deepEqual(Object.keys(input), ["url"]);
    assert.equal(route.request().headers()["x-wx-launch"], launchKey);
    imageRequests++;
    const invalid = new URL(input.url).searchParams.has("invalid");
    await route.fulfill({ json: {
      statusCode: 200, header: { "content-type": invalid ? "text/html" : "image/svg+xml" }, cookies: [],
      body: Buffer.from(invalid ? "<html>not an image</html>"
        : '<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><path fill="#12503e" d="M0 0h2v1H0z"/></svg>').toString("base64"),
    } });
  });
  await page.evaluate(() => {
    const current = getCurrentPages().at(-1);
    current.loadedImage = undefined;
    current.setData({ image: "https://example.invalid/session" });
  });
  await page.waitForFunction(() => getCurrentPages().at(-1).loadedImage?.width === 2);
  assert.match(await page.locator("#embedded-image").getAttribute("src"), /^blob:/);
  assert.deepEqual(await page.evaluate(() => getCurrentPages().at(-1).loadedImage), { width: 2, height: 1 });
  const imageStyles = await page.evaluate(async () => {
    const { remoteImage } = await import("/images.js");
    const { applyInlineStyle, createLegacyView } = await import("/styles.js");
    const { compiledStyles } = await import("/modules.js");
    const source = "https://example.invalid/session";
    const blob = await remoteImage(source);
    compiledStyles.set("fixture/image", '#sample{background-image:url("' + source + '")}');
    const host = document.createElement("div");
    document.body.append(host);
    const view = createLegacyView(host, "fixture/image", {});
    const sample = document.createElement("div");
    sample.id = "sample"; view.content.append(sample);
    const inline = document.createElement("div");
    view.content.append(inline);
    applyInlineStyle(inline, 'background-image:url("' + source + '")', "fixture/image");
    // Both style paths attach to the same cached image promise.
    await remoteImage(source);
    await new Promise(resolve => requestAnimationFrame(resolve));
    const result = {
      sheet: getComputedStyle(sample).backgroundImage,
      inline: getComputedStyle(inline).backgroundImage, blob,
      rejectsHtml: await remoteImage(source + "?invalid=1").then(() => false, () => true),
      rejectsCredentials: await remoteImage("https://user:password@example.invalid/session").then(() => false, () => true),
    };
    view.close(); host.remove();
    return result;
  });
  assert.equal(imageStyles.sheet, 'url("' + imageStyles.blob + '")');
  assert.equal(imageStyles.inline, imageStyles.sheet);
  assert.equal(imageStyles.rejectsHtml, true);
  assert.equal(imageStyles.rejectsCredentials, true);
  assert.equal(imageRequests, 2);
  await page.unroute("**/_wx/image");
  let holdImage;
  const heldImage = new Promise(resolve => { holdImage = resolve; });
  await page.route("**/_wx/image", route => holdImage(route));
  await page.evaluate(() => getCurrentPages().at(-1).setData({ image: "https://example.invalid/slow-image" }));
  const imageRoute = await heldImage;
  await page.evaluate(image => getCurrentPages().at(-1).setData({ image }), originalImage);
  await page.locator("#embedded-image").evaluate(image => image.decode());
  await imageRoute.fulfill({ json: { statusCode: 200, header: { "content-type": "image/svg+xml" },
    body: Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>').toString("base64"),
  } });
  await page.evaluate(async () => {
    await (await import("/images.js")).remoteImage("https://example.invalid/slow-image");
    await new Promise(resolve => requestAnimationFrame(resolve));
  });
  assert.equal(await page.locator("#embedded-image").getAttribute("src"), originalImage);
  await page.unroute("**/_wx/image");
  console.log("PASS: scoped remote images, shared cache, CSS/inline backgrounds, load dimensions and non-image rejection");
  deniedHost.child.kill("SIGINT");
  assert.equal((await deniedHost.exited)[0], 0);
  assert.deepEqual(pageErrors.splice(0), [
    "Failed to load resource: the server responded with a status of 403 (Forbidden)",
    "Failed to load resource: the server responded with a status of 403 (Forbidden)",
  ]);
  console.log("PASS: opt-in bridge origin/key checks, exact URL denials, and browser response callbacks using synthetic data");
  const { reply: missingPackage, exited: missingExited } = await childReply(["__mini-program-host-v1"], {
    version: 1, endpoint: stack.hub.endpoint, token: stack.hub.token,
    accountId: ACCOUNT.id, appId: "wxfedcba9876543210",
  });
  assert.deepEqual(missingPackage, { version: 1, state: "error", code: "mini_program_package_unavailable" });
  assert.equal((await missingExited)[0], 1);
  const machine = await childReady(["__mini-program-host-v1"], {
    version: 1, endpoint: stack.hub.endpoint, token: stack.hub.token,
    accountId: ACCOUNT.id, appId: app, sourceMessageId: card.id,
  });
  await page.goto(machine.url);
  await page.getByText("Menu: seasonal", { exact: true }).waitFor();
  machine.child.stdin.end();
  assert.equal((await machine.exited)[0], 0);
  await assert.rejects(() => fetch(machine.url));
  console.log("PASS: desktop host opens the shared page and stops when its control pipe closes");
  ui = await startUiServer();
  ui.attach(stack);
  const base = ui.origin + "/#/" + stack.hub.serverId + "/" + ACCOUNT.id;
  desktop = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const uiErrors = [];
  desktop.on("pageerror", error => uiErrors.push(error.message));
  await desktop.route("**/v1/accounts/*/media/lookup", async route => {
    const response = await route.fetch();
    const json = await response.json();
    for (const item of json.items) {
      for (const asset of item.assets) {
        if (asset.id === icon.id) {
          asset.state = "available";
          asset.actualVariant = asset.variant;
        }
      }
    }
    await route.fulfill({ response, json });
  });
  await desktop.route(`**/media/${icon.id}/content*`, route => route.fulfill({
    contentType: "image/png",
    body: Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l2sAAAAASUVORK5CYII=", "base64"),
  }));
  await desktop.goto(base + "/chats/sim_mei");
  const openCard = desktop.getByRole("button", { name: "Open Seasonal menu" });
  await openCard.and(desktop.locator("button:enabled")).waitFor({ timeout: 15000 });
  assert.equal(await openCard.evaluate(node => getComputedStyle(node).cursor), "pointer");
  await desktop.getByText("Package unavailable on this hub", { exact: true }).waitFor();
  assert.equal(await desktop.getByRole("button", { name: "Open A weekend hike" }).isDisabled(), true);
  await desktop.screenshot({ path: join(output, "desktop-preview-cards.png") });
  const cardRequest = desktop.waitForRequest(req => req.method() === "POST" && req.url().endsWith("/launches"));
  await openCard.click();
  assert.equal((await cardRequest).postDataJSON().sourceMessageId, card.id);
  await desktop.getByText("Page is ready. AgentWechat opens it in a separate window.").waitFor();
  await desktop.getByRole("button", { name: "Mini Programs", exact: true }).click();
  await desktop.getByRole("heading", { name: "Mini Programs", exact: true }).waitFor();
  await desktop.getByText("Experimental", { exact: true }).waitFor();
  assert.equal(await desktop.locator(".sidebar").isVisible(), false);
  const listIcon = desktop.locator("li").filter({ has: desktop.getByRole("button", { name: "Open Cafe" }) }).locator(".mini-program-icon img");
  await listIcon.evaluate(image => image.decode());
  const listRequest = desktop.waitForRequest(req => req.method() === "POST" && req.url().endsWith("/launches"));
  await desktop.getByRole("button", { name: "Open Cafe" }).click();
  assert.equal((await listRequest).postDataJSON().sourceMessageId, undefined);
  await desktop.getByText("Page is ready. AgentWechat opens it in a separate window.").waitFor();
  await desktop.screenshot({ path: join(output, "desktop-preview-list.png") });
  await desktop.setViewportSize({ width: 390, height: 844 });
  await desktop.screenshot({ path: join(output, "desktop-preview-narrow.png") });
  await desktop.getByRole("button", { name: "Switch account" }).click();
  await desktop.getByRole("button", { name: new RegExp(SPARE.alias) }).click();
  await desktop.getByRole("button", { name: "Mini Programs", exact: true }).click();
  await desktop.getByText("No Mini Programs available for this account.").waitFor();
  assert.deepEqual(uiErrors, []);
  console.log("PASS: desktop frontend preview, cards, list, account switch, and distinct launch requests");
} finally {
  await desktop?.unrouteAll({ behavior: "ignoreErrors" });
  await browser?.close();
  await ui?.close();
  for (const { child, exited } of hosts) {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    await exited;
  }
  if (policyHub) {
    await new Promise(resolve => policyHub.close(resolve));
  }
  await stack.stop();
}
