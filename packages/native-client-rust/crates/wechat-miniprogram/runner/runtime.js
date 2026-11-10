import { MiniProgramEnv, glassEasel } from "./adapter.js";
import * as initial from "./page.js";
import { appConfig, componentConfigs, registerModules } from "./modules.js";
import { createSelectorQuery, createIntersectionObserver, mountLegacy, needsPrivacyAuthorization, registerBehavior, navigateBuiltin } from "./legacy.js";
import { applyGlassStyles } from "./styles.js";

const launchKey = window.location.hash.slice(1);
if (initial.allowWasm) {
  globalThis.WXWebAssembly = Object.freeze({
    async instantiate(path, imports) {
      if (typeof path !== "string" || !path.endsWith(".wasm") || !path.replace(/^\//, "").split("/").every(
        part => /^[A-Za-z0-9._@-]+$/.test(part) && part !== "." && part !== "..",
      )) throw new Error("Invalid packaged WebAssembly path");
      const response = await fetch("/" + path.replace(/^\//, ""), { credentials: "omit", redirect: "error" });
      return WebAssembly.instantiateStreaming(response, imports);
    },
  });
}
globalThis.__wxConfig = appConfig;
globalThis.__wxAppCode__ = Object.fromEntries(
  [...componentConfigs].map(([route, config]) => [route + ".json", config]),
);
// Some bundled libraries discover the host through the CommonJS global alias.
globalThis.global = globalThis;
const storage = new Map();
let app = { globalData: { theme: "light", hasLogin: false } };
const getApp = () => app;
const registerApp = value => { app = value; };
const factories = registerModules(registerApp, getApp);
// Compiled service factories shadow browser globals. Libraries that look
// for a global object can use these built-ins without evaluating a string.
const serviceSelf = Object.assign(Object.create(null), { Object, Date });
const modules = new Map();
const subpackageLoads = new Map();

function resolvePath(owner, name) {
  if (typeof name !== "string") throw new Error("Invalid module reference");
  const parts = name.startsWith(".") ? owner.split("/").slice(0, -1) : [];
  for (const part of name.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (!parts.length) throw new Error("Module escapes this package");
      parts.pop();
    } else {
      parts.push(part);
    }
  }
  return parts.join("/");
}

function modulePaths(owner, name) {
  const paths = [resolvePath(owner, name)];
  if (!name.startsWith(".") && !name.startsWith("/")) {
    paths.push(resolvePath(owner, "./" + name));
  }
  return paths.flatMap(path => [path, path + ".js", path + "/index.js"]);
}

function runFactory(factory, id, module) {
  factory(requireFrom(id), module, module.exports, undefined, undefined, undefined, serviceSelf);
}

function loadModule(id) {
  if (!id) throw new Error("This Mini Program module is unavailable");
  if (modules.has(id)) return modules.get(id).exports;
  const module = { exports: {} };
  modules.set(id, module);
  try {
    runFactory(factories.get(id), id, module);
    return module.exports;
  } catch (error) {
    modules.delete(id);
    throw error;
  }
}

function loadSubpackage(root, id) {
  if (!subpackageLoads.has(id)) {
    const load = import("/_wx/subpackages/" + id + ".js").then(({ registerModules }) => {
      const entries = registerModules(registerApp, getApp);
      for (const path of entries.keys()) {
        if (!path.startsWith(root + "/") || factories.has(path)) {
          throw new Error("Mini Program subpackage module conflicts with the current package");
        }
      }
      for (const [path, factory] of entries) factories.set(path, factory);
    });
    subpackageLoads.set(id, load);
  }
  return subpackageLoads.get(id);
}

function requireFrom(owner) {
  const requireModule = name => loadModule(modulePaths(owner, name).find(path => factories.has(path)));
  requireModule.async = async name => {
    const paths = modulePaths(owner, name);
    const available = paths.find(path => factories.has(path));
    if (available) return loadModule(available);
    const selected = [...(initial.subpackages ?? [])]
      .filter(([root]) => paths.some(path => path.startsWith(root + "/")))
      .sort(([left], [right]) => right.length - left.length)[0];
    if (!selected) throw { mod: name, errMsg: "require:fail module is unavailable" };
    try {
      await loadSubpackage(...selected);
      return loadModule(paths.find(path => factories.has(path)));
    } catch (error) {
      throw { mod: name, errMsg: "require:fail " + error.message };
    }
  };
  return requireModule;
}

function invoke(promise, options, api) {
  const result = promise.catch(() => { throw { errMsg: `${api}:fail` }; });
  if (!options?.success && !options?.fail && !options?.complete) return result;
  void result.then(
    value => { try { options.success?.(value); } finally { options.complete?.(value); } },
    error => { try { options.fail?.(error); } finally { options.complete?.(error); } },
  );
}

function localCall(api, options, operation) {
  return invoke(Promise.resolve().then(() => ({
    ...operation(),
    errMsg: `${api}:ok`,
  })), options, api);
}

function storageKey(key) {
  if (typeof key !== "string" || !key) throw new TypeError("Storage key is required");
  return key;
}

function storageCall(api, options, operation) {
  return localCall(api, options, () => {
    if (options?.encrypt) throw new Error("Encrypted storage is unavailable");
    return operation(options);
  });
}

function getStorageSync(key) {
  const id = storageKey(key);
  return storage.has(id) ? structuredClone(storage.get(id)) : "";
}

function setStorageSync(key, value) {
  const id = storageKey(key);
  try {
    storage.set(id, structuredClone(value));
  } catch (error) {
    if (error.name !== "DataCloneError") throw error;
    // Reactive objects can be JSON-serializable even when they are proxies.
    storage.set(id, JSON.parse(JSON.stringify(value)));
  }
}

function removeStorageSync(key) {
  storage.delete(storageKey(key));
}

function getStorage(options) {
  return storageCall("getStorage", options, ({ key }) => {
    if (!storage.has(storageKey(key))) throw new Error("Storage key is missing");
    return { data: getStorageSync(key) };
  });
}

function getStorageInfoSync() {
  const encoder = new TextEncoder();
  const keys = [...storage.keys()];
  let bytes = 0;
  for (const [key, value] of storage) {
    bytes += encoder.encode(key).length;
    try { bytes += encoder.encode(JSON.stringify(value)).length; } catch {}
  }
  return { keys, currentSize: Math.ceil(bytes / 1024) };
}

function getWindowInfo() {
  const top = document.querySelector(".mini-toolbar").getBoundingClientRect().height;
  const width = document.documentElement.clientWidth;
  const height = Math.max(0, window.innerHeight - top);
  return {
    pixelRatio: window.devicePixelRatio,
    screenWidth: window.screen.width,
    screenHeight: window.screen.height,
    windowWidth: width,
    windowHeight: height,
    screenTop: top,
    statusBarHeight: 0,
    safeArea: { left: 0, top, right: width, bottom: top + height, width, height },
  };
}

function getMenuButtonBoundingClientRect() {
  const { left, top, right, bottom, width, height } =
    document.querySelector("#mini-about").getBoundingClientRect();
  return { left, top, right, bottom, width, height };
}

function getDeviceInfo() {
  const os = navigator.userAgentData?.platform || navigator.platform;
  const platform = /^mac/i.test(os) ? "mac" : /^win/i.test(os) ? "windows" :
    /^linux/i.test(os) ? "linux" : "unknown";
  const system = { mac: "macOS", windows: "Windows", linux: "Linux", unknown: "" }[platform];
  return { platform, system, model: "" };
}

function getSystemInfoSync() {
  return { ...getDeviceInfo(), ...getWindowInfo(), ...getAppBaseInfo() };
}

function getAppBaseInfo() {
  // Keep version comparisons safe without claiming an official base library.
  const info = { SDKVersion: "0.0.0", language: navigator.language, enableDebug: false, host: {} };
  if (appConfig.darkmode) {
    info.theme = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return info;
}

function getPerformance() {
  return {
    getEntries: () => performance.getEntries(),
    getEntriesByName: (name, type) => performance.getEntriesByName(name, type),
    getEntriesByType: type => performance.getEntriesByType(type),
    createObserver(callback) {
      const observer = new PerformanceObserver(callback);
      return {
        observe({ entryTypes }) {
          const supported = entryTypes.filter(type => PerformanceObserver.supportedEntryTypes.includes(type));
          if (supported.length) observer.observe({ entryTypes: supported });
        },
        disconnect: () => observer.disconnect(),
      };
    },
  };
}

function listeners(onCallbackError, copy = structuredClone) {
  const callbacks = new Set();
  return {
    on(listener) {
      if (typeof listener === "function") callbacks.add(listener);
    },
    off(listener) {
      if (listener === undefined) callbacks.clear();
      else callbacks.delete(listener);
    },
    emit(...args) {
      for (const callback of callbacks) {
        queueMicrotask(() => {
          if (!callbacks.has(callback)) return;
          try {
            const result = callback(...copy(args));
            if (onCallbackError) void Promise.resolve(result).catch(onCallbackError);
          } catch (error) {
            if (!onCallbackError) throw error;
            onCallbackError(error);
          }
        });
      }
    },
  };
}

function currentNetworkStatus() {
  const type = navigator.connection?.type;
  const isConnected = navigator.onLine && type !== "none";
  // Browser speed estimates cannot identify the connection technology.
  const networkType = !isConnected ? "none" : type === "wifi" ? "wifi" : "unknown";
  return { isConnected, networkType };
}

const networkListeners = listeners();
const wifiListeners = listeners();
const networkEvents = new AbortController();
// App SDKs can wrap the console. A failing error handler must not itself
// publish another application error or rejection.
const logCallbackError = console.error.bind(console);
const errorListeners = listeners(logCallbackError);
const rejectionListeners = listeners(logCallbackError,
  ([{ reason, promise }]) => [{ reason, promise }]);
window.addEventListener("error", event => {
  if (event instanceof ErrorEvent) errorListeners.emit(event.message);
}, { signal: networkEvents.signal });
window.addEventListener("unhandledrejection", ({ reason, promise }) => {
  rejectionListeners.emit({ reason, promise });
}, { signal: networkEvents.signal });
let lastNetworkStatus = currentNetworkStatus();

function networkChanged() {
  const status = currentNetworkStatus();
  if (status.isConnected === lastNetworkStatus.isConnected &&
      status.networkType === lastNetworkStatus.networkType) return;
  const connectedToWifi = status.networkType === "wifi" && lastNetworkStatus.networkType !== "wifi";
  lastNetworkStatus = status;
  networkListeners.emit(status);
  // The browser can sometimes report the transport, but cannot read the SSID.
  if (connectedToWifi) wifiListeners.emit({ wifi: {} });
}

for (const event of ["online", "offline"]) {
  window.addEventListener(event, networkChanged, { signal: networkEvents.signal });
}
navigator.connection?.addEventListener("change", networkChanged, { signal: networkEvents.signal });

function getNetworkType(options) {
  const { networkType } = currentNetworkStatus();
  return invoke(Promise.resolve({ networkType, errMsg: "getNetworkType:ok" }), options, "getNetworkType");
}

function login(options) {
  return invoke(fetch("/_wx/ipc", {
    method: "POST",
    credentials: "omit",
    cache: "no-store",
    headers: {
      "Content-Type": "application/json",
      "x-wx-launch": launchKey,
      "Idempotency-Key": crypto.randomUUID(),
    },
    body: JSON.stringify({ method: "wx.login" }),
  }).then(async response => {
    if (!response.ok) throw new Error("Mini Program login failed");
    const { code } = await response.json();
    if (typeof code !== "string" || !code) throw new Error("Mini Program login reply is invalid");
    return { code, errMsg: "login:ok" };
  }), options, "login");
}

function showModal(options) {
  const dialog = document.querySelector("#mini-modal");
  return invoke(new Promise((resolve, reject) => {
    if (dialog.open) { reject(new Error("A dialog is already open")); return; }
    dialog.querySelector("h2").textContent = options?.title ?? "";
    dialog.querySelector("p").textContent = options?.content ?? "";
    const cancel = dialog.querySelector('button[value="cancel"]');
    cancel.textContent = options?.cancelText ?? "Cancel";
    cancel.hidden = options?.showCancel === false;
    dialog.querySelector('button[value="confirm"]').textContent = options?.confirmText ?? "Confirm";
    dialog.returnValue = "cancel";
    dialog.addEventListener("close", () => {
      const confirm = dialog.returnValue === "confirm";
      resolve({ confirm, cancel: !confirm, errMsg: "showModal:ok" });
    }, { once: true });
    dialog.showModal();
  }), options, "showModal");
}

const feedback = document.querySelector("#mini-feedback");
let feedbackTimer;

function clearFeedback(kind) {
  if (feedback.dataset.kind !== kind) return;
  clearTimeout(feedbackTimer);
  feedback.hidden = true;
  delete feedback.dataset.kind;
}

function showFeedback(options, kind) {
  if (typeof options?.title !== "string") throw new TypeError("Feedback title is required");
  const icon = kind === "loading" ? "loading" : options.icon ?? "success";
  const duration = options.duration ?? 1500;
  if (kind === "toast" && (!Number.isFinite(duration) || duration < 0)) {
    throw new TypeError("Invalid toast duration");
  }
  clearTimeout(feedbackTimer);
  feedback.querySelector("span").textContent = options.title;
  feedback.dataset.kind = kind;
  feedback.dataset.icon = icon;
  feedback.toggleAttribute("data-mask", options.mask === true);
  feedback.hidden = false;
  if (kind === "toast") feedbackTimer = setTimeout(() => clearFeedback(kind), Math.min(duration, 2 ** 31 - 1));
}

function getSetting(options) {
  return localCall("getSetting", options, () => ({
    authSetting: {},
    ...(options?.withSubscriptions ? { subscriptionsSetting: { mainSwitch: false } } : {}),
  }));
}

function getPrivacySetting(options) {
  return localCall("getPrivacySetting", options, () => ({ needAuthorization: needsPrivacyAuthorization() }));
}

function unavailableGateway() {
  return {
    call(options) {
      return invoke(Promise.reject(new Error("Cloud Gateway is unavailable")), options, "gateway.call");
    },
  };
}

function unavailable(api) {
  return options => invoke(Promise.reject(new Error("Capability is unavailable")), options, api);
}

const allowRequest = Boolean(initial.allowRequest && launchKey);

function formData(data) {
  if (typeof data === "string") return data;
  return new URLSearchParams(Object.entries(data ?? {})).toString();
}

async function sendRequest(options, signal) {
  if (!allowRequest) throw new Error("network access is disabled");
  if (typeof options?.url !== "string" || options.enableChunked || options.enableProfile) {
    throw new Error("unsupported request options");
  }
  const url = new URL(options.url);
  const method = String(options.method ?? "GET").toUpperCase();
  const header = Object.fromEntries(
    Object.entries(options.header ?? {}).map(([name, value]) => [name.toLowerCase(), String(value)]),
  );
  let body;
  if (method === "GET" || method === "HEAD") {
    for (const [key, value] of new URLSearchParams(formData(options.data))) {
      url.searchParams.append(key, value);
    }
  } else if (options.data !== undefined) {
    header["content-type"] ??= "application/json";
    if (typeof options.data === "string") body = options.data;
    else if (header["content-type"].startsWith("application/x-www-form-urlencoded")) {
      body = formData(options.data);
    } else {
      body = JSON.stringify(options.data);
    }
  }
  const reply = await fetch("/_wx/request", {
    method: "POST",
    credentials: "omit",
    cache: "no-store",
    signal,
    headers: { "Content-Type": "application/json", "x-wx-launch": launchKey },
    body: JSON.stringify({ url: url.href, method, header, body, timeout: options.timeout }),
  });
  if (!reply.ok) {
    const reason = { 403: "URL is not allowed", 429: "too many requests", 504: "timeout" }[reply.status];
    throw new Error(reason ?? "network request failed");
  }
  const { statusCode, header: responseHeader, cookies, body: encoded } = await reply.json();
  const bytes = Uint8Array.from(atob(encoded), char => char.charCodeAt(0));
  let data = bytes.buffer;
  if (options.responseType !== "arraybuffer") {
    data = new TextDecoder().decode(bytes);
    if ((options.dataType ?? "json") === "json") {
      try { data = JSON.parse(data); } catch { /* Non-JSON responses remain text. */ }
    }
  }
  return { data, statusCode, header: responseHeader, cookies, errMsg: "request:ok" };
}

function request(options) {
  const controller = new AbortController();
  const headers = new Set();
  void Promise.resolve().then(() => {
    controller.signal.throwIfAborted();
    return sendRequest(options, controller.signal);
  }).then(
    result => {
      try {
        for (const listener of headers) listener({ header: result.header, statusCode: result.statusCode });
        options?.success?.(result);
      } finally {
        options?.complete?.(result);
      }
    },
    error => {
      const reason = controller.signal.aborted ? "abort" : error.message;
      const result = { errMsg: `request:fail ${reason}` };
      try { options?.fail?.(result); } finally { options?.complete?.(result); }
    },
  );
  return {
    abort() { controller.abort(); },
    onHeadersReceived(listener) { if (typeof listener === "function") headers.add(listener); },
    offHeadersReceived(listener) { if (listener === undefined) headers.clear(); else headers.delete(listener); },
  };
}

function loadFontFace(options) {
  return localCallAsync("loadFontFace", options, async () => {
    const source = /^url\(\s*(?:"([^"]+)"|'([^']+)'|([^\s)]+))\s*\)$/.exec(options?.source);
    if (typeof options?.family !== "string" || !source) throw new Error("Invalid font");
    const { data, statusCode } = await sendRequest({
      url: source[1] ?? source[2] ?? source[3],
      responseType: "arraybuffer",
    }, AbortSignal.timeout(20_000));
    if (statusCode < 200 || statusCode >= 300) throw new Error("Font download failed");
    const font = new FontFace(options.family, data, options.desc);
    await font.load();
    document.fonts.add(font);
    return { status: "loaded" };
  });
}

function localCallAsync(api, options, operation) {
  return invoke(Promise.resolve().then(operation).then(result => ({
    ...result,
    errMsg: `${api}:ok`,
  })), options, api);
}

const parseQuery = query => Object.fromEntries(new URLSearchParams(query));
const launch = { path: initial.entry, query: parseQuery(initial.queryString) };
const main = document.querySelector("main");
const back = document.querySelector("#mini-back");
const stack = [];
let loadingPages;
const appShow = listeners();
const appHide = listeners();
let pending = false;
let foreground = false;
let registration;

// Legacy dependencies can wrap the global Page function before a page is evaluated.
globalThis.Page = definition => {
  if (!registration) throw new Error("No Mini Program page is being registered");
  return registration.page(definition);
};
globalThis.Component = definition => {
  if (!registration) throw new Error("No Mini Program component is being registered");
  return registration.component(definition);
};
globalThis.Behavior = definition => (registration?.behavior ?? registerBehavior)(definition);

function withRegistration(next, load) {
  const previous = registration;
  registration = next;
  try {
    return load();
  } finally {
    registration = previous;
  }
}

function loadPage(module, page, component, behavior) {
  return withRegistration({ page, component, behavior }, () => {
    const factory = module.pageFactory(value => globalThis.Page(value), getApp);
    runFactory(factory, module.entry + ".js", { exports: {} });
  });
}

const componentDefinitions = new Map();

function loadComponent(owner, tag) {
  const declarations = componentConfigs.get(owner)?.usingComponents;
  const target = Object.hasOwn(declarations ?? {}, tag) ? declarations[tag] : undefined;
  if (typeof target !== "string" || target.includes(":")) return;
  const route = resolvePath(owner, target);
  const id = route + ".js";
  if (!factories.has(id)) return;
  if (!componentDefinitions.has(route)) {
    withRegistration({ component: value => componentDefinitions.set(route, value) }, () => requireFrom("")(id));
  }
  const definition = componentDefinitions.get(route);
  if (definition) return { route, definition };
}

function setForeground(visible) {
  if (foreground === visible) return;
  foreground = visible;
  if (visible) {
    app.onShow?.(structuredClone(launch));
    appShow.emit(launch);
    stack.at(-1)?.lifecycle.show();
  } else {
    stack.at(-1)?.lifecycle.hide();
    app.onHide?.();
    appHide.emit();
  }
}

let glassBackend;
function mountGlass(view, module, query, setLoadingPage) {
  const { entry, templatesFactory } = module;
  const object = value => value ?? Object.create(null);
  const text = value => value == null ? "" : String(value);
  const changed = (change, key) => change === true || change?.[key];
  const groups = templatesFactory({}, undefined, undefined, {}, undefined, object, text, changed);
  const env = new MiniProgramEnv();
  const space = env.createCodeSpace("mini-program", true);
  space.addCompiledTemplate(entry, {
    groupList: groups, content: groups[entry], fallbackListenerOnNativeNode: true,
  });
  space.addComponentStaticConfig(entry, {});
  space.componentEnv(entry, ({ Page, Component, Behavior }) => loadPage(module, Page, Component, Behavior));
  if (!glassBackend) {
    glassBackend = new glassEasel.CurrentWindowBackendContext();
    glassBackend.onEvent(glassEasel.Event.triggerBackendEvent);
  }
  const dom = glassBackend;
  const root = env.associateBackend(dom).createRoot("mini-page", space, entry);
  const instance = root.get();
  instance.route = entry;
  instance.options = { ...query };
  setLoadingPage(instance);
  instance.onLoad?.({ ...query });
  root.attach(view, view.firstElementChild);
  view.addEventListener("click", event => {
    const navigator = event.target.closest?.("navigator");
    if (!navigator || !view.contains(navigator) || event.defaultPrevented) return;
    event.preventDefault();
    void navigateBuiltin(Object.fromEntries([...navigator.attributes].map(attr => [attr.name, attr.value])))
      .catch(() => wx.showToast({ title: "This page could not be opened.", icon: "none" }));
  });
  return {
    instance,
    show: () => instance.onShow?.(),
    hide: () => instance.onHide?.(),
    ready: () => instance.onReady?.(),
    close: () => { instance.onUnload?.(); root.release(); },
  };
}

function display(module, query, mode = "push") {
  const view = document.createElement("section");
  view.className = "mini-app-view";
  const content = document.createElement("section");
  content.append(document.createElement("span"));
  const releaseStyles = module.framework !== "exparser" ? applyGlassStyles(view, module.entry) : () => {};
  view.append(content);
  main.append(view);
  const setLoadingPage = instance => {
    const retained = mode === "reset" ? [] : mode === "replace" ? stack.slice(0, -1) : stack;
    loadingPages = [...retained.map(page => page.lifecycle.instance), instance];
  };
  let lifecycle;
  try {
    lifecycle = module.framework === "exparser"
      ? mountLegacy(content, module, query, register => loadPage(module, register, value => register(value, true)), loadComponent, setLoadingPage)
      : mountGlass(content, module, query, setLoadingPage);
  } catch (error) {
    releaseStyles();
    view.remove();
    throw error;
  } finally {
    loadingPages = undefined;
  }
  const close = lifecycle.close;
  lifecycle.close = () => { try { close(); } finally { releaseStyles(); } };
  const previous = stack.at(-1);
  if (previous) {
    if (foreground) previous.lifecycle.hide();
    if (mode !== "push") {
      const removed = mode === "reset" ? stack.splice(0) : [stack.pop()];
      for (const page of removed.reverse()) {
        page.lifecycle.close();
        page.view.remove();
      }
    } else {
      previous.view.hidden = true;
      const stylesheet = previous.view.querySelector(":scope > link");
      if (stylesheet) stylesheet.disabled = true;
    }
  }
  stack.push({ view, lifecycle, module, query });
  if (foreground) lifecycle.show();
  lifecycle.ready();
  back.hidden = stack.length < 2;
}

async function navigate(options, mode = "push") {
  if (pending || stack.length >= 10 && mode === "push") throw new Error("Navigation is unavailable");
  const raw = options?.url ?? "";
  const separator = raw.indexOf("?");
  const path = (separator < 0 ? raw : raw.slice(0, separator)).replace(/^\//, "");
  if (path.length > 240 || !path.split("/").every(
    part => /^[A-Za-z0-9._@-]+$/.test(part) && part !== "." && part !== "..",
  )) throw new Error("Invalid Mini Program page");
  pending = true;
  try {
    const module = await import("/_wx/pages/" + path);
    display(module, parseQuery(separator < 0 ? "" : raw.slice(separator + 1)), mode);
    return {};
  } finally {
    pending = false;
  }
}

async function navigateBack(delta) {
  if (pending || stack.length < 2 || !Number.isSafeInteger(delta) || delta < 1) {
    throw new Error("Cannot go back");
  }
  for (let remaining = delta; remaining > 0 && stack.length > 1; remaining--) {
    const page = stack.pop();
    if (foreground && remaining === delta) page.lifecycle.hide();
    page.lifecycle.close();
    page.view.remove();
  }
  const current = stack.at(-1);
  current.view.hidden = false;
  const stylesheet = current.view.querySelector(":scope > link");
  if (stylesheet) stylesheet.disabled = false;
  if (foreground) current.lifecycle.show();
  back.hidden = stack.length < 2;
  return {};
}

const wxApis = {
  createIntersectionObserver: (component, options) => createIntersectionObserver(component ?? stack.at(-1)?.lifecycle.instance, options),
  getLaunchOptionsSync: () => structuredClone(launch),
  getEnterOptionsSync: () => structuredClone(launch),
  onAppShow: appShow.on,
  offAppShow: appShow.off,
  onAppHide: appHide.on,
  offAppHide: appHide.off,
  getStorageSync,
  getStorageInfoSync,
  getStorageInfo: options => localCall("getStorageInfo", options, getStorageInfoSync),
  setStorageSync,
  removeStorageSync,
  clearStorageSync: () => storage.clear(),
  getStorage,
  setStorage: options => storageCall("setStorage", options, ({ key, data }) => setStorageSync(key, data)),
  removeStorage: options => storageCall("removeStorage", options, ({ key }) => removeStorageSync(key)),
  clearStorage: options => localCall("clearStorage", options, () => storage.clear()),
  getWindowInfo,
  getMenuButtonBoundingClientRect,
  getDeviceInfo,
  getAppBaseInfo,
  onError: errorListeners.on,
  offError: errorListeners.off,
  onUnhandledRejection: rejectionListeners.on,
  offUnhandledRejection: rejectionListeners.off,
  ...(typeof PerformanceObserver === "function" ? { getPerformance } : {}),
  getAccountInfoSync: () => ({ miniProgram: { appId: initial.appId, envVersion: "release" } }),
  getSystemInfoSync,
  getSystemInfo: options => localCall("getSystemInfo", options, getSystemInfoSync),
  getSetting,
  loadFontFace,
  getPrivacySetting,
  checkSession: options => invoke(Promise.reject(new Error("No verified Mini Program session")), options, "checkSession"),
  nextTick: callback => queueMicrotask(callback),
  createSelectorQuery: () => stack.at(-1)?.lifecycle.instance.createSelectorQuery?.() ?? createSelectorQuery(),
  showModal,
  showToast: options => localCall("showToast", options, () => showFeedback(options, "toast")),
  hideToast: options => localCall("hideToast", options, () => clearFeedback("toast")),
  showLoading: options => localCall("showLoading", options, () => showFeedback(options, "loading")),
  hideLoading: options => localCall("hideLoading", options, () => clearFeedback("loading")),
  setNavigationBarTitle: options => localCall("setNavigationBarTitle", options, () => {
    if (typeof options?.title !== "string") throw new Error("A title is required");
    document.title = options.title;
  }),
  // Hosted launches do not forward app analytics.
  reportAnalytics() {},
  getNetworkType,
  onNetworkStatusChange: networkListeners.on,
  offNetworkStatusChange: networkListeners.off,
  onWifiConnected: wifiListeners.on,
  offWifiConnected: wifiListeners.off,
  navigateTo: options => invoke(navigate(options), options, "navigateTo"),
  redirectTo: options => invoke(navigate(options, "replace"), options, "redirectTo"),
  reLaunch: options => invoke(navigate(options, "reset"), options, "reLaunch"),
  navigateBack: options => invoke(navigateBack(options?.delta ?? 1), options, "navigateBack"),
  ...(initial.allowLogin && launchKey ? { login } : {}),
};
// App libraries may attach metadata or wrap API methods. Capability checks
// still describe the host, even if application code adds its own methods.
globalThis.wx = {
  env: Object.freeze({}),
  cloud: {
    // Some apps initialize the cloud namespace before using ordinary pages.
    // Allow that call without enabling cloud operations or sending telemetry.
    init() {},
    services: Object.freeze({ Gateway: unavailableGateway }),
  },
  // The host supports local button confirmation. It does not request account
  // privacy authorization or trigger a WeChat privacy request.
  onNeedPrivacyAuthorization() {},
  requirePrivacyAuthorize: unavailable("requirePrivacyAuthorize"),
  getShareInfo: unavailable("getShareInfo"),
  getLocation: unavailable("getLocation"),
  login: options => invoke(Promise.reject(new Error("Mini Program login is unavailable")), options, "login"),
  ...wxApis,
  request,
  canIUse: name => name === "canIUse" || name === "request" && allowRequest || Object.hasOwn(wxApis, name),
};
globalThis.getCurrentPages = () => loadingPages?.slice() ?? stack.map(({ lifecycle }) => lifecycle.instance);

const details = document.querySelector("#mini-details");
document.querySelector("#mini-about").addEventListener("click", () => {
  const page = stack.at(-1);
  details.querySelector("p").textContent = page
    ? `Page: ${page.module.entry} · ${page.module.framework}`
    : "This Mini Program could not start.";
  details.showModal();
});
document.querySelector("#mini-reload").addEventListener("click", () => {
  const page = stack.at(-1);
  if (!page) { location.reload(); return; }
  try { display(page.module, page.query, "replace"); }
  catch { location.reload(); }
});
back.addEventListener("click", () => { void navigateBack(1).catch(() => {}); });
document.addEventListener("visibilitychange", () => setForeground(!document.hidden), {
  signal: networkEvents.signal,
});
window.addEventListener("pagehide", () => {
  networkEvents.abort();
  networkListeners.off();
  wifiListeners.off();
  errorListeners.off();
  rejectionListeners.off();
  setForeground(false);
  for (const page of stack.reverse()) page.lifecycle.close();
}, { once: true });

if (factories.has("app.js")) requireFrom("")("app.js");
app.onLaunch?.(structuredClone(launch));
setForeground(!document.hidden);
display(initial, launch.query);
