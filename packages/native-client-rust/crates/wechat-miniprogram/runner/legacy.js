import { createLegacyView, applyInlineStyle } from "./styles.js";
import { intersection } from "./intersection.js";
import { remoteImage } from "./images.js";

// Adapt compiled legacy virtual nodes to a small allowlist of browser elements.
const tags = new Map([
  ["wx-page", "section"], ["wx-view", "div"], ["wx-text", "span"],
  ["wx-button", "button"], ["wx-input", "input"], ["wx-textarea", "textarea"],
  ["wx-scroll-view", "div"], ["wx-label", "label"], ["wx-image", "img"],
]);

const events = new Map([
  ["click", "tap"], ["input", "input"], ["change", "change"],
  ["focus", "focus"], ["blur", "blur"],
]);

const behaviors = new Map();
const roots = new WeakMap();
const observers = new WeakMap();
let privacyAccepted = false;

export function needsPrivacyAuthorization() {
  return !privacyAccepted;
}

async function confirmPrivacy() {
  if (privacyAccepted) return true;
  const dialog = document.querySelector("#mini-privacy");
  if (dialog.open) return false;
  const button = dialog.querySelector('button[value="confirm"]');
  const events = new AbortController();
  let confirmed = false;
  button.addEventListener("click", event => { confirmed = event.isTrusted; }, { signal: events.signal });
  const closed = new Promise(resolve => dialog.addEventListener("close", resolve, { once: true }));
  dialog.returnValue = "cancel";
  dialog.showModal();
  await closed;
  events.abort();
  privacyAccepted = confirmed && dialog.returnValue === "confirm";
  return privacyAccepted;
}

export function createIntersectionObserver(instance, options) {
  const root = roots.get(instance);
  if (!root) throw new Error("No active view for intersection observation");
  return intersection(root, options, observers.get(instance));
}


// The legacy prelude installs WXS helpers on this document's JS prototypes once.
export const compiler = {};
export const compilerState = { entrys: {}, defines: {}, modules: {}, ops: [], total_ops: 0 };

export function createSelectorQuery(instance) {
  let root = roots.get(instance);
  const operations = [];
  function select(selector, all) {
    const scope = root;
    function queue(read, callback) {
      operations.push(() => {
        const nodes = selector === null ? [null] : [...(scope?.querySelectorAll(selector.replaceAll(">>>", " ")) ?? [])];
        const result = all ? nodes.map(read) : nodes.length ? read(nodes[0]) : null;
        callback?.(result);
        return result;
      });
      return query;
    }
    return {
      boundingClientRect(callback) {
        return queue(node => {
          const { top, left, right, bottom, width, height } = node?.getBoundingClientRect() ?? {
            top: 0, left: 0, right: innerWidth, bottom: innerHeight, width: innerWidth, height: innerHeight,
          };
          return { id: node?.id ?? "", dataset: { ...node?.dataset }, top, left, right, bottom, width, height };
        }, callback);
      },
      scrollOffset(callback) {
        return queue(node => ({
          id: node?.id ?? "",
          dataset: { ...node?.dataset },
          scrollLeft: node?.scrollLeft ?? scrollX,
          scrollTop: node?.scrollTop ?? scrollY,
        }), callback);
      },
    };
  }
  const query = {
    in(scope) { root = roots.get(scope); return query; },
    select(selector) { return select(selector, false); },
    selectAll(selector) { return select(selector, true); },
    selectViewport() { return select(null, false); },
    exec(callback) {
      const batch = operations.splice(0);
      queueMicrotask(() => {
        const results = batch.map(read => read());
        callback?.(results);
      });
    },
  };
  return query;
}

export function registerBehavior(definition) {
  const id = `wx://behavior/${behaviors.size + 1}`;
  behaviors.set(id, definition);
  return id;
}

function definitions(definition) {
  const ordered = [];
  const visited = new Set();
  function visit(current) {
    for (const id of current.behaviors ?? []) {
      if (visited.has(id) || !behaviors.has(id)) continue;
      visited.add(id);
      visit(behaviors.get(id));
    }
    ordered.push(current);
  }
  visit(definition);
  return ordered;
}

function writeData(data, path, value) {
  const keys = path.replace(/\[(\d+)\]/g, ".$1").split(".");
  if (keys.some(key => !key || ["__proto__", "constructor", "prototype"].includes(key))) return;
  let target = data;
  for (let i = 0; i < keys.length - 1; i++) {
    const key = keys[i];
    if (!Object.hasOwn(target, key) || !target[key] || typeof target[key] !== "object") {
      target[key] = /^\d+$/.test(keys[i + 1]) ? [] : {};
    }
    target = target[key];
  }
  target[keys.at(-1)] = value;
}

export function mountLegacy(main, module, query, loadPage, loadComponent, setLoadingPage) {
  let definition;
  loadPage(value => { definition = value; });
  if (!definition) throw new Error("Incomplete legacy page registration");
  const templates = new Map();
  function template(route) {
    if (!templates.has(route)) templates.set(route, module.templatesFactory(`./${route}.wxml`));
    return templates.get(route);
  }
  return mountInstance(main, { route: module.entry, definition, query, template, loadComponent, setLoadingPage, depth: 0 });
}

function mountInstance(main, { route, definition, query, template, loadComponent, setLoadingPage, depth, owner, attrs, slot, budget, styleContext }) {
  const styled = createLegacyView(main, route, definition, styleContext);
  const view = styled.content;
  const generate = template(route);
  if (typeof generate !== "function") throw new Error("Missing legacy template");
  const sources = definitions(definition);
  const methods = Object.assign({}, ...sources.map(source => source.methods));
  const data = Object.assign({}, ...sources.map(source => source.data));
  const properties = Object.assign({}, ...sources.map(source => source.properties));
  const page = { ...definition, ...methods, is: route, route, options: { ...query }, data: structuredClone(data) };
  page.properties = page.data;
  if (owner) page.selectOwnerComponent = () => owner;
  const children = new Map();
  function matchingChildren(selector) {
    return [...children.values()].filter(child => child.element.matches(selector)).map(child => child.instance);
  }
  page.selectComponent = selector => matchingChildren(selector)[0] ?? null;
  page.selectAllComponents = matchingChildren;
  roots.set(page, view);
  observers.set(page, new Set());
  page.createIntersectionObserver = options => createIntersectionObserver(page, options);
  page.createSelectorQuery = () => createSelectorQuery(page);
  page.triggerEvent = (name, detail, options) => {
    main.dispatchEvent(new CustomEvent(`mini:${name}`, { detail, bubbles: options?.bubbles === true }));
  };
  const lifetime = name => {
    for (const source of sources) (source.lifetimes?.[name] ?? source[name])?.call(page);
  };
  let attached = false;
  let rendered;
  let pending = false;
  function update(nextAttrs, nextSlot) {
    slot = nextSlot;
    const changes = [];
    for (const [key, spec] of Object.entries(properties)) {
      const attribute = key.replace(/[A-Z]/g, letter => "-" + letter.toLowerCase());
      const fallback = spec?.value ?? ({ String: "", Number: 0, Boolean: false, Array: [], Object: null }[
        (spec?.type ?? spec)?.name
      ] ?? null);
      const value = nextAttrs[key] ?? nextAttrs[attribute] ?? fallback;
      const previous = page.data[key];
      if (Object.is(previous, value)) continue;
      page.data[key] = value;
      const observer = typeof spec?.observer === "string" ? page[spec.observer] : spec?.observer;
      if (typeof observer === "function") changes.push(() => observer.call(page, value, previous, key));
    }
    for (const change of changes) change();
  }

  page.setData = (patch, callback) => {
    for (const [path, value] of Object.entries(patch)) writeData(page.data, path, value);
    if (attached && !pending) {
      pending = true;
      queueMicrotask(() => {
        pending = false;
        if (attached) render();
      });
    }
    if (callback) queueMicrotask(() => { if (attached) callback.call(page); });
  };

  function render(limit = { nodes: 0 }) {
    const tree = generate(page.data, Object.create(null), Object.create(null));
    const active = view.getRootNode().activeElement;
    const focused = rendered?.contains(active) ? active : null;
    const index = focused ? [...rendered.querySelectorAll("input,textarea,button")].indexOf(focused) : -1;
    const cursor = focused?.selectionStart;
    const retained = new Set();
    function make(node, level, key) {
      if (++limit.nodes > 20000 || level > 64) throw new Error("Legacy view exceeded limits");
      if (node == null || typeof node === "boolean") return document.createTextNode("");
      if (typeof node !== "object") return document.createTextNode(String(node));
      if (node.tag === "wx-slot") {
        const element = document.createElement("slot");
        element.name = node.attr?.name ?? "";
        return element;
      }
      if (Array.isArray(node) || node.tag === "virtual") {
        const fragment = document.createDocumentFragment();
        const items = Array.isArray(node) ? node : node.children ?? [];
        items.forEach((child, index) => fragment.append(make(child, level + 1, `${key}/${index}`)));
        return fragment;
      }
      const tag = tags.get(node.tag) ?? "div";
      const tagName = node.tag?.replace(/^wx-/, "");
      const component = depth < 16 ? loadComponent(route, tagName) : undefined;
      const supported = component && typeof template(component.route) === "function";
      const childKey = `${key}:${node.wxKey ?? ""}:${tagName}`;
      const existing = supported ? children.get(childKey) : undefined;
      const element = existing?.element ?? document.createElement(tag);
      const attrs = node.attr ?? {};
      applyAttributes(element, tag, node.tag, attrs, route);
      const controller = supported ? new AbortController() : undefined;
      existing?.controller.abort();
      bindEvents(element, attrs, page, controller, supported);
      if (supported) {
        const projectSlot = name => {
          const fragment = document.createDocumentFragment();
          (node.children ?? []).forEach((child, index) => {
            if ((child?.attr?.slot ?? "") === name) fragment.append(make(child, level + 1, `${key}/${index}`));
          });
          return fragment;
        };
        element.replaceChildren(projectSlot(""));
        for (const name of new Set((node.children ?? []).map(child => child?.attr?.slot).filter(Boolean))) {
          element.append(projectSlot(name));
        }
        const child = existing ?? {
          ...mountInstance(element, {
            ...component, template, loadComponent, depth: depth + 1, owner: page, attrs, slot: projectSlot,
            budget: limit, styleContext: styled.context,
          }),
          element,
        };
        child.controller = controller;
        children.set(childKey, child);
        retained.add(childKey);
        if (existing) child.update(attrs, projectSlot, limit);
        else child.ready();
      } else {
        (node.children ?? []).forEach((child, index) => element.append(make(child, level + 1, `${key}/${index}`)));
      }
      return element;
    }

    rendered = document.createElement("section");
    rendered.append(make(tree, 0, ""));
    for (const [key, child] of children) {
      if (retained.has(key)) continue;
      child.close();
      children.delete(key);
    }
    view.replaceChildren(rendered);
    const replacement = rendered.querySelectorAll("input,textarea,button")[index];
    if (replacement) {
      replacement.focus({ preventScroll: true });
      if (typeof cursor === "number") replacement.setSelectionRange?.(cursor, cursor);
    }
  }

  const lifecycle = {
    instance: page,
    update(nextAttrs, nextSlot, limit) { update(nextAttrs, nextSlot); render(limit); },
    show() {
      page.onShow?.();
      if (owner) definition.pageLifetimes?.show?.call(page);
      for (const child of children.values()) child.show();
    },
    hide() {
      for (const child of children.values()) child.hide();
      if (owner) definition.pageLifetimes?.hide?.call(page);
      page.onHide?.();
    },
    ready() { lifetime("ready"); page.onReady?.(); },
    close() {
      for (const child of children.values()) child.close();
      children.clear();
      for (const observer of observers.get(page) ?? []) observer.disconnect();
      observers.delete(page);
      roots.delete(page);
      styled.close();
      if (attached) {
        attached = false;
        lifetime("detached");
        page.onUnload?.();
      }
    },
  };
  try {
    setLoadingPage?.(page);
    lifetime("created");
    if (attrs) update(attrs, slot);
    if (query) page.onLoad?.(query);
    render(budget);
    attached = true;
    lifetime("attached");
    return lifecycle;
  } catch (error) {
    lifecycle.close();
    throw error;
  }
}

function applyAttributes(element, tag, wxTag, attrs, route) {
  for (const attr of [...element.attributes]) element.removeAttribute(attr.name);
  element.className = wxTag === "wx-scroll-view" ? "mini-legacy-node mini-scroll" : "mini-legacy-node";
  element.setAttribute("data-wx-tag", wxTag ?? "wx-view");
  for (const [name, value] of Object.entries(attrs)) {
    if (value == null) continue;
    if (name === "class") element.className += " " + String(value);
    else if (["id", "title", "aria-label", "placeholder", "slot"].includes(name)) element.setAttribute(name, String(value));
    else if (name === "style") applyInlineStyle(element, String(value), route);
    else if (name === "src" && tag === "img" && typeof value === "string" && value.startsWith("https://")) {
      element.dataset.imageState = "loading";
      void remoteImage(value).then(source => {
        if (!element.isConnected) return;
        element.src = source;
        element.dataset.imageState = "loaded";
      }, () => {
        if (!element.isConnected) return;
        element.dataset.imageState = "unavailable";
        element.dispatchEvent(new CustomEvent("error", { detail: { errMsg: "Image is unavailable" } }));
      });
    }
    else if (name === "src" && tag === "img" && typeof value === "string"
      && value.length <= 2 * 1024 * 1024
      && /^data:image\/(?:png|jpeg|gif|webp|svg\+xml)(?:;[^,]*)?,/i.test(value)) {
      // Image-context decoding cannot execute SVG scripts. The host CSP keeps
      // data URLs unavailable for scripts, frames and network connections.
      element.src = value;
    }
    else if (name === "src" && tag === "img" && typeof value === "string"
      && /^[a-zA-Z0-9_./-]+$/.test(value) && !value.startsWith("//")) {
      const base = new URL("/" + route, location.origin);
      const image = new URL(value, base);
      if (image.origin === location.origin) element.src = image.href;
    }
    else if (name === "hidden" || name === "disabled") element.toggleAttribute(name, Boolean(value));
    else if (name === "value" && (tag === "input" || tag === "textarea")) element.value = String(value);
    else if (name.startsWith("data-") && /^[a-z0-9-]+$/.test(name)) element.setAttribute(name, String(value));
  }
}

function bindEvents(element, attrs, page, controller, custom) {
  const bindings = new Map(events);
  if (element.tagName === "BUTTON" && (attrs.openType ?? attrs["open-type"]) === "agreePrivacyAuthorization") {
    bindings.set("mini:agreeprivacyauthorization", "agreeprivacyauthorization");
    element.addEventListener("click", async event => {
      if (!event.isTrusted || !await confirmPrivacy() || !element.isConnected) return;
      element.dispatchEvent(new CustomEvent("mini:agreeprivacyauthorization", {
        detail: { errMsg: "agreePrivacyAuthorization:ok" },
      }));
    }, { signal: controller?.signal });
  }
  if (element.tagName === "IMG") {
    bindings.set("load", "load");
    bindings.set("error", "error");
  }
  if (custom) {
    for (const name of Object.keys(attrs)) {
      const match = /^(?:bind|catch):?([\w-]+)$/.exec(name);
      if (match) bindings.set(`mini:${match[1]}`, match[1]);
    }
  }
  for (const [event, miniEvent] of bindings) {
    const caught = attrs["catch" + miniEvent] ?? attrs["catch:" + miniEvent];
    const handler = caught ?? attrs["bind" + miniEvent] ?? attrs["bind:" + miniEvent];
    if (typeof handler !== "string" || !Object.hasOwn(page, handler) || typeof page[handler] !== "function") continue;
    element.addEventListener(event, ev => {
      if (caught) ev.stopPropagation();
      const target = node => ({ id: node.id, dataset: { ...node.dataset } });
      page[handler]({
        type: miniEvent,
        currentTarget: target(element),
        target: target(ev.target),
        detail: ev instanceof CustomEvent ? ev.detail
          : element.tagName === "IMG" && event === "load" ? { width: element.naturalWidth, height: element.naturalHeight }
          : element.tagName === "IMG" && event === "error" ? { errMsg: "Image is unavailable" }
          : { value: ev.target.value },
        timeStamp: ev.timeStamp,
      });
    }, { signal: controller?.signal });
  }
}
