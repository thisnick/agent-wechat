import { parse, walk, generate } from "./css-tree.js";
import { compiledStyles, componentConfigs } from "./modules.js";
import { remoteImage } from "./images.js";

const sheets = new Map();
const inlineStyles = new WeakMap();
let glassViewId = 0;

// Glass-easel's DOM backend uses native tag names, unlike the legacy adapter's
// data-wx-tag elements. Keep each page's CSS inside its own view and remove it
// with the view on navigation.
export function applyGlassStyles(view, route) {
  const scope = String(++glassViewId);
  view.dataset.miniGlass = scope;
  const owned = [];
  for (const name of ["app", route]) {
    const source = compiledStyles.get(name);
    if (source === undefined) continue;
    const ast = parse(source, { parseCustomProperty: true });
    responsiveUnits(ast);
    walk(ast, {
      visit: "TypeSelector",
      enter(node) {
        if (["body", "page", "wx-page"].includes(node.name)) node.name = "mini-page";
        else if (node.name.startsWith("wx-")) node.name = node.name.slice(3);
      },
    });
    const remote = imageUrls(ast, name);
    const style = new CSSStyleSheet();
    const apply = css => style.replaceSync(`@scope ([data-mini-glass="${scope}"]) { ${css} }`);
    apply(generate(ast));
    owned.push(style);
    loadImages(ast, remote, apply);
  }
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, ...owned];
  return () => {
    document.adoptedStyleSheets = document.adoptedStyleSheets.filter(sheet => !owned.includes(sheet));
  };
}

function responsiveUnits(ast) {
  walk(ast, {
    visit: "Dimension",
    enter(node, item, list) {
      if (node.unit.toLowerCase() !== "rpx" || !item || !list) return;
      list.replace(item, parse(`calc(${node.value} * 100vw / 750)`, { context: "value" }).children);
    },
  });
}

// Parse declarations rather than replacing text inside URLs or quoted content.
export function inlineStyle(source) {
  const ast = parse(source, { context: "declarationList", parseCustomProperty: true });
  responsiveUnits(ast);
  return generate(ast);
}

function imageUrls(ast, route) {
  const remote = [];
  walk(ast, {
    visit: "Url",
    enter(node) {
      // Resolve packaged assets beside their stylesheet, never the host toolbar.
      try { node.value = new URL(node.value, new URL("/" + route + ".wxss", location.origin)).href; }
      catch { node.value = ""; }
      if (node.value.startsWith("https://")) {
        remote.push({ node, url: node.value });
        node.value = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
      }
    },
  });
  return remote;
}

function loadImages(ast, remote, apply) {
  for (const { node, url } of remote) {
    void remoteImage(url).then(source => {
      node.value = source;
      apply(generate(ast));
    }, () => {});
  }
}

export function applyInlineStyle(element, source, route) {
  const ast = parse(source, { context: "declarationList", parseCustomProperty: true });
  responsiveUnits(ast);
  const remote = imageUrls(ast, route);
  inlineStyles.set(element, ast);
  element.style.cssText = generate(ast);
  loadImages(ast, remote, css => {
    if (element.isConnected && inlineStyles.get(element) === ast) element.style.cssText = css;
  });
}

export function clearInlineStyle(element) {
  inlineStyles.delete(element);
}

const defaults = new CSSStyleSheet();
defaults.replaceSync(`
  :host { display: block; }
  *, *::before, *::after { box-sizing: border-box; }
  .mini-view-root { min-height: 100%; }
  [hidden] { display: none !important; }
  [data-wx-tag="wx-scroll-view"] { overflow: auto; }
  button { min-height: 36px; padding: 6px 12px; }
  button:enabled, a[href], .mini-tappable:not(:disabled) { cursor: pointer; }
`);

function sheet(route) {
  if (sheets.has(route)) return sheets.get(route);
  const source = compiledStyles.get(route);
  if (source === undefined) return;
  const ast = parse(source, { parseCustomProperty: true });
  responsiveUnits(ast);
  walk(ast, {
    visit: "TypeSelector",
    enter(node, item, list) {
      if (!item || !list) return;
      const selector = ["body", "page"].includes(node.name) ? ".mini-view-root"
        : node.name.startsWith("wx-") ? `[data-wx-tag=${JSON.stringify(node.name)}]` : null;
      if (selector) list.replace(item, parse(selector, { context: "selector" }).children);
    },
  });
  const remote = imageUrls(ast, route);
  const result = new CSSStyleSheet();
  result.replaceSync(generate(ast));
  loadImages(ast, remote, css => result.replaceSync(css));
  sheets.set(route, result);
  return result;
}

export function createLegacyView(host, route, definition, context) {
  const isPage = !context;
  const options = definition.options ?? {};
  const mode = componentConfigs.get(route)?.styleIsolation ?? options.styleIsolation
    ?? (isPage ? "shared" : options.addGlobalClass ? "apply-shared" : "isolated");
  if (!["isolated", "apply-shared", "shared", ...(isPage ? ["page-isolated", "page-apply-shared", "page-shared"] : [])].includes(mode)) {
    throw new Error("Unsupported component style isolation");
  }
  const own = sheet(route);
  context ??= { roots: new Map(), shared: new Set(), page: [
    ...(!mode.startsWith("page-") && sheet("app") ? [sheet("app")] : []),
    ...(own ? [own] : []),
  ] };
  const root = host.attachShadow({ mode: "open" });
  const inherited = isPage || mode.endsWith("shared");
  context.roots.set(root, { own, inherited });
  if (!isPage && mode === "shared" && own) context.shared.add(own);
  for (const [target, config] of context.roots) {
    target.adoptedStyleSheets = [...new Set([
      defaults,
      ...(config.inherited ? [...context.page, ...context.shared] : []),
      ...(config.own ? [config.own] : []),
    ])];
  }
  const content = document.createElement("div");
  content.className = "mini-view-root";
  root.append(content);
  return { content, context, close: () => context.roots.delete(root) };
}
